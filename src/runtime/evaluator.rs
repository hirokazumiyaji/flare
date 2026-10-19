use std::ops::{ControlFlow, Deref, DerefMut};

use crate::ast::{CallArg, ExprKind, StrPiece, UnOp};
use crate::error::{
    add_call_frame, BudgetError, CapabilityError, ErrorKind, ServiceId, Span, UserRaisedError,
};
use crate::firestore::QueryResultLimitExceeded;
use crate::runtime::builtins::auth::auth_session_field;
use crate::runtime::builtins::storage::forbid_storage_in_batch_or_transaction;
use crate::runtime::builtins::{
    is_builtin_name, lookup_global_builtin, lookup_module_builtin, BuiltinSpec,
};
use crate::runtime::contract::{
    check_block_arity, keyword_names, positional_arg_count, validate_call_contract,
    validate_func_params, validate_unique_keywords, validate_unique_params,
    validate_user_call_contract, BlockPolicy, PositionalArity,
};
use crate::runtime::error::RuntimeError;
use crate::runtime::methods::{
    check_method_receiver, eval_method_call, module_builtin_spec, required_service_for_method,
    validate_method_call,
};
use crate::services::ServiceAccess;
use crate::value::{self, BuiltinModule, UserFunc};

use super::state::RuntimeState;
use super::*;

pub(crate) use super::ExecFlow;

type CallArgValues = (Vec<Value>, BTreeMap<String, Value>);

pub(crate) struct Evaluator<'a> {
    pub(crate) services: ServiceAccess<'a>,
    pub(crate) state: &'a mut RuntimeState,
}

impl<'a> Evaluator<'a> {
    pub(crate) fn new(services: ServiceAccess<'a>, state: &'a mut RuntimeState) -> Self {
        Self { services, state }
    }
}

impl Deref for Evaluator<'_> {
    type Target = RuntimeState;

    fn deref(&self) -> &Self::Target {
        self.state
    }
}

impl DerefMut for Evaluator<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.state
    }
}

/// Scope cleanup guard for function and block calls.
///
/// A function temporarily hides all caller-local scopes while keeping the
/// global scope visible. A block only adds one frame. In both cases the
/// original scope stack is restored on every exit path, including `?` returns
/// and panic unwinding.
pub(super) struct ScopeGuard<'guard, 'runtime> {
    pub(super) runtime: &'guard mut Evaluator<'runtime>,
    // `Some` is a function frame and contains the previous frame base. `None`
    // is a block frame, which is removed from the end on drop.
    saved_base: Option<usize>,
}

impl<'guard, 'runtime> ScopeGuard<'guard, 'runtime> {
    pub(super) fn function(runtime: &'guard mut Evaluator<'runtime>) -> Self {
        let saved_base = runtime.current_frame_base;
        runtime.current_frame_base = runtime.scopes.len();
        runtime.scopes.push(HashMap::new());
        Self {
            runtime,
            saved_base: Some(saved_base),
        }
    }

    pub(super) fn block(runtime: &'guard mut Evaluator<'runtime>) -> Self {
        runtime.scopes.push(HashMap::new());
        Self {
            runtime,
            saved_base: None,
        }
    }
}

impl Drop for ScopeGuard<'_, '_> {
    fn drop(&mut self) {
        if let Some(saved_base) = self.saved_base {
            // Keep the caller frames and discard every frame created while the
            // function was running before restoring the caller frame base.
            let base = self.runtime.current_frame_base;
            self.runtime.scopes.truncate(base);
            self.runtime.current_frame_base = saved_base;
        } else {
            self.runtime.scopes.pop();
        }
    }
}

impl Evaluator<'_> {
    pub(super) fn eval_program(&mut self, stmts: &[Stmt]) -> Result<Value, Box<dyn Error>> {
        self.with_execution(|rt| {
            rt.validate_external_ast_depth(stmts)?;
            rt.check_wall_clock_at_source()
                .and_then(|_| rt.eval_block(stmts).map(ExecFlow::into_value))
        })
    }

    /// トップレベル文の開始位置付きで評価し、失敗した文の位置を
    /// runtime/backend エラーに付与する。既に span を持つ lex/parse エラーは
    /// そのまま通す。
    pub(super) fn eval_program_with_spans(
        &mut self,
        stmts: &[Stmt],
        spans: &[Span],
    ) -> Result<Value, Box<dyn Error>> {
        self.with_execution(|rt| {
            rt.check_wall_clock_at_source()
                .and_then(|_| rt.eval_program_with_spans_active(stmts, spans))
        })
    }

    /// Externally supplied ASTs (an embedder calling the public API) may nest
    /// far deeper than the parser would produce, so the parser-depth budget is
    /// enforced here too. Without it, a hand-built deeply nested `Call` or
    /// `Binary` chain would recurse into `eval_expr` without ever reaching the
    /// parser guard.
    pub(super) fn validate_external_ast_depth(&self, stmts: &[Stmt]) -> Result<(), Box<dyn Error>> {
        let Some(max) = self.limits.max_parser_depth else {
            return Ok(());
        };
        Parser::validate_stmt_depth(stmts, max)
            .map_err(|error| attach_span(error, Some(Span::new(1, 1))))
    }

    pub(super) fn eval_program_with_spans_active(
        &mut self,
        stmts: &[Stmt],
        spans: &[Span],
    ) -> Result<Value, Box<dyn Error>> {
        self.validate_external_ast_depth(stmts)?;
        let mut last = Value::Nil;
        for (i, stmt) in stmts.iter().enumerate() {
            let span = spans.get(i).copied();
            match self.eval_stmt(stmt) {
                Ok(ExecFlow::Value(v)) => last = v,
                Ok(ExecFlow::Return(v)) => return Ok(v),
                Err(e) => return Err(attach_span(e, span)),
            }
        }
        Ok(last)
    }

    /// ソースを字句解析・構文解析・評価する一連の処理。`lib.rs` の
    /// エントリポイントとテストの共有ヘルパー。
    pub(super) fn run(&mut self, source: &str) -> Result<Value, Box<dyn Error>> {
        self.with_execution(|rt| rt.run_active(source))
    }

    pub(super) fn run_active(&mut self, source: &str) -> Result<Value, Box<dyn Error>> {
        // Check before lexing as well as after parsing.  This matters for a
        // zero/very-small wall-clock budget: source-size/token limits are
        // separate guards and may be intentionally disabled by an embedder.
        //
        // A budget failure raised before evaluation has no statement to point
        // at, so attach the start of the program.  The CLI contract is that
        // runtime failures always carry a position.
        self.check_wall_clock_at_source()?;
        let spanned = crate::lexer::lex_spanned_with_limits(source, self.limits.parser_limits())?;
        self.check_wall_clock_at_source()?;
        let mut parser = Parser::from_spanned_with_limits(spanned, self.limits.parser_limits());
        let (program, spans) = parser.parse_program_with_spans()?;
        self.check_wall_clock_at_source()?;
        self.eval_program_with_spans_active(&program, &spans)
    }

    /// Wall-clock check for the pre-evaluation phase, reported at the start of
    /// the program because no statement has begun yet.
    pub(super) fn finish_execution(&mut self) {
        if let Some(started_at) = self.started_at.take() {
            self.usage.elapsed = started_at.elapsed();
        }
        self.call_depth = 0;
        self.current_frame_base = 0;
        self.execution_active = false;
    }

    pub(super) fn check_wall_clock_at_source(&self) -> Result<(), Box<dyn Error>> {
        self.check_wall_clock()
            .map_err(|e| attach_span(e, Some(Span::new(1, 1))))
    }

    /// 実行を開始し、budget の起点を記録する。
    ///
    /// 公開エントリポイント (`run` / `eval_program*`) は再入を許さない。
    /// 許すと `start_execution` が実行中の `usage` / `started_at` を静かに
    /// リセットし、step / value / wall-clock の budget が実質無効になる。
    pub(super) fn start_execution(&mut self) -> Result<(), Box<dyn Error>> {
        if self.execution_active {
            return Err(FlareError::runtime(
                "run() / eval_program() is already executing on this Runtime",
            )
            .into());
        }
        self.usage = RuntimeUsage::default();
        self.call_depth = 0;
        self.started_at = Some(Instant::now());
        self.execution_active = true;
        Ok(())
    }

    /// Run one public entry point with the execution state cleared on every
    /// exit path, including an unwind.
    ///
    /// A backend that panics must not leave `execution_active` set: the next
    /// `run` would then be rejected as reentrant instead of observing the
    /// scopes that the scope guards restored while unwinding. The panic is
    /// resumed after the reset so callers still see it.
    pub(super) fn with_execution<T>(
        &mut self,
        body: impl FnOnce(&mut Self) -> Result<T, Box<dyn Error>>,
    ) -> Result<T, Box<dyn Error>> {
        self.start_execution()?;
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(self)));
        self.finish_execution();
        match outcome {
            Ok(result) => result,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    pub(super) fn check_wall_clock(&self) -> Result<(), Box<dyn Error>> {
        let Some(limit) = self.limits.max_wall_clock else {
            return Ok(());
        };
        let Some(started_at) = self.started_at else {
            return Ok(());
        };
        let elapsed = started_at.elapsed();
        if elapsed >= limit {
            return Err(BudgetError::new(format!(
                "wall-clock limit exceeded (max {})",
                format_duration(limit)
            ))
            .into());
        }
        Ok(())
    }

    pub(super) fn consume_step(&mut self) -> Result<(), Box<dyn Error>> {
        if self.usage.steps & 1023 == 0 {
            self.check_wall_clock()?;
        }
        if let Some(max) = self.limits.max_steps {
            if self.usage.steps >= max {
                return Err(BudgetError::new(format!("step limit exceeded (max {max})")).into());
            }
        }
        self.usage.steps = self.usage.steps.saturating_add(1);
        Ok(())
    }

    pub(super) fn consume_loop_iteration(&mut self) -> Result<(), Box<dyn Error>> {
        if self.usage.loop_iterations & 1023 == 0 {
            self.check_wall_clock()?;
        }
        if let Some(max) = self.limits.max_loop_iterations {
            if self.usage.loop_iterations >= max {
                return Err(
                    BudgetError::new(format!("loop iteration limit exceeded (max {max})")).into(),
                );
            }
        }
        self.usage.loop_iterations = self.usage.loop_iterations.saturating_add(1);
        Ok(())
    }

    pub(super) fn enter_call(&mut self) -> Result<(), Box<dyn Error>> {
        if self.call_depth & 127 == 0 {
            self.check_wall_clock()?;
        }
        if let Some(max) = self.limits.max_call_depth {
            if self.call_depth >= max {
                return Err(
                    BudgetError::new(format!("call depth limit exceeded (max {max})")).into(),
                );
            }
        }
        self.call_depth = self.call_depth.saturating_add(1);
        self.usage.max_call_depth = self.usage.max_call_depth.max(self.call_depth);
        Ok(())
    }

    pub(super) fn leave_call(&mut self) {
        self.call_depth = self.call_depth.saturating_sub(1);
    }

    /// 検証と depth 観測を 1 パスで行う。
    ///
    /// スカラー(数値・文字列・真偽値など)も root 1 ノードとして数える。
    /// 子が無いので depth は 0 相当だが、ノード予算は 0 のときに「ルートが
    /// すでに 1 ノードである」ことで拒否する。Composite 値のときは子を展開
    /// しながら数えるので、巨大な配列を 1 ノードとして誤認しない。
    pub(super) fn check_value(&mut self, value: &Value) -> Result<(), Box<dyn Error>> {
        if !matches!(
            value,
            Value::Array(_) | Value::ArrayUnion(_) | Value::ArrayRemove(_) | Value::Map(_)
        ) {
            // スカラー値は子が無いので depth 0 相当。ただし root 1 ノードと
            // して数え、ノード予算が 0 のときはここで弾く。
            if let Some(0) = self.limits.max_value_nodes {
                return Err(BudgetError::new("value node limit exceeded (max 0)").into());
            }
            match value {
                Value::Str(s) => {
                    if let Some(max) = self.limits.max_string_bytes {
                        if s.len() > max {
                            return Err(BudgetError::new(format!(
                                "string size limit exceeded (max {max})"
                            ))
                            .into());
                        }
                    }
                }
                Value::Bytes(b) => {
                    if let Some(max) = self.limits.max_bytes_size {
                        if b.len() > max {
                            return Err(BudgetError::new(format!(
                                "bytes size limit exceeded (max {max})"
                            ))
                            .into());
                        }
                    }
                }
                _ => {}
            }
            return Ok(());
        }
        let observed_depth = value::validate_value_limits_with_sizes(
            value,
            self.limits.max_value_depth,
            self.limits.max_value_nodes,
            self.limits.max_string_bytes,
            self.limits.max_bytes_size,
        )
        .map_err(|e| BudgetError::new(e.to_string()))?;
        self.usage.max_value_depth = self.usage.max_value_depth.max(observed_depth);
        Ok(())
    }

    pub(super) fn eval_stmt(&mut self, stmt: &Stmt) -> Result<ExecFlow, Box<dyn Error>> {
        self.consume_step()?;
        match stmt {
            Stmt::Expr(e) => self.eval_expr(e),
            Stmt::Assign { name, value } => {
                // ビルトイン関数名と同じ名前の変数定義(例: set = 5)は許可する。
                // 呼び出し時(Expr::Call)はビルトインディスパッチが常に優先され、
                // 変数参照時(Expr::Var)は変数テーブルから読み出されるため構文上区別できる。
                // 一方で fn 定義(Stmt::Def)は定義してもビルトインに隠されて呼び出せないため
                // 禁止している。
                if self.var_exists(name)
                    && !(self.repl_mode && self.current_frame_base == 0 && self.scopes.len() == 1)
                {
                    return Err(RuntimeError::ReassignmentNotAllowed(name.to_string()).into());
                }
                let flow = self.eval_expr(value)?;
                let v = match flow {
                    ExecFlow::Value(v) => v,
                    flow @ ExecFlow::Return(_) => return Ok(flow),
                };
                self.set_var(name, v.clone())?;
                Ok(ExecFlow::Value(v))
            }
            Stmt::Def { name, params, body } => {
                validate_func_params(params, "function")?;
                if is_builtin_name(name) {
                    return Err(RuntimeError::CannotRedefineBuiltin(name.to_string()).into());
                }
                if self.var_exists(name)
                    && !(self.repl_mode && self.current_frame_base == 0 && self.scopes.len() == 1)
                {
                    return Err(RuntimeError::ReassignmentNotAllowed(name.to_string()).into());
                }
                self.set_var(
                    name,
                    Value::Func(UserFunc::new(params.clone(), body.clone())),
                )?;
                Ok(ExecFlow::Value(Value::Nil))
            }
            Stmt::Return(e) => {
                let value = match e {
                    Some(e) => match self.eval_expr(e)? {
                        ExecFlow::Value(v) => v,
                        flow @ ExecFlow::Return(_) => return Ok(flow),
                    },
                    None => Value::Nil,
                };
                Ok(ExecFlow::Return(value))
            }
        }
    }

    pub(super) fn eval_block(&mut self, stmts: &[Stmt]) -> Result<ExecFlow, Box<dyn Error>> {
        let mut last = Value::Nil;
        for stmt in stmts {
            match self.eval_stmt(stmt)? {
                ExecFlow::Value(v) => last = v,
                ExecFlow::Return(v) => return Ok(ExecFlow::Return(v)),
            }
        }
        Ok(ExecFlow::Value(last))
    }

    /// `exec` をステージング用 ctx に差し替え、子スコープでブロックを評価する。
    /// ブロックの成否にかかわらず元の ctx とスコープへ戻し、ステージされた
    /// writes を返す。
    pub(super) fn run_staged_block(
        &mut self,
        ctx: ExecCtx,
        body: &[Stmt],
    ) -> (Result<ExecFlow, Box<dyn Error>>, Vec<WriteOp>) {
        let prev = std::mem::replace(&mut self.exec, ctx);
        // batch / transaction は呼び出し元を読める字句的な子スコープで
        // 評価する。ブロック内で定義した変数・関数は外へ漏れず、成功・
        // ランタイムエラー/backend エラーのどちらでも同じスコープを片付ける。
        // Scope frames are balanced by eval_call/call_block before any
        // fallible work is performed. Record only the caller's depth instead
        // of deep-cloning every global and local value for each staged block.
        let caller_scope_depth = self.scopes.len();
        self.scopes.push(HashMap::new());
        let flow = self.eval_block(body);
        // `truncate` would silently discard a frame leaked by a nested call,
        // so assert the balance explicitly. This only runs in debug builds,
        // which is where the test suite executes.
        debug_assert_eq!(
            self.scopes.len(),
            caller_scope_depth + 1,
            "staged block must leave the scope stack balanced"
        );
        self.scopes.truncate(caller_scope_depth);
        let writes = match std::mem::replace(&mut self.exec, prev) {
            ExecCtx::Batch { writes } | ExecCtx::Transaction { writes } => writes,
            ExecCtx::Normal => unreachable!("exec ctx changed inside batch/transaction"),
        };
        (flow, writes)
    }

    pub(super) fn forbid_query_in_batch_or_transaction(&self) -> Result<(), Box<dyn Error>> {
        match self.exec {
            ExecCtx::Batch { .. } => Err(RuntimeError::QueryInBatch.into()),
            ExecCtx::Transaction { .. } => Err(RuntimeError::QueryInTransaction.into()),
            ExecCtx::Normal => Ok(()),
        }
    }

    /// Abort is cleanup, not the error being reported by a transaction.
    /// Keeping the call non-propagating is important: if the block or commit
    /// already failed, that original error must reach the caller even when
    /// rollback also fails. A failed rollback is remembered so the next
    /// `transaction` block retries it; see `retry_pending_rollback`.
    pub(super) fn abort_transaction_best_effort(&mut self) {
        if self.services.firestore.abort_transaction().is_err() {
            // The backend kept the transaction open. Remember it: a script
            // has no other way to reach the backend and clear the state.
            self.pending_rollback = true;
        }
    }

    /// Retry a rollback that previously failed. Called before every
    /// `begin_transaction`. If the retry also fails, `begin_transaction`
    /// surfaces its own "already in transaction" error, which is at least an
    /// honest report of a backend that is still busy.
    pub(super) fn retry_pending_rollback(&mut self) {
        if !self.pending_rollback {
            return;
        }
        if self.services.firestore.abort_transaction().is_ok() {
            self.pending_rollback = false;
        }
    }

    /// Validate that a staged write carries only document-representable
    /// values, then hand it to the active execution context.
    ///
    /// Auth sessions and their token projections are deliberately rejected
    /// here rather than in a backend: they hold secrets, and letting the
    /// in-memory backend accept what the emulator would refuse would persist
    /// tokens into documents and split the backends' behavior.
    pub(super) fn dispatch_write(&mut self, op: WriteOp) -> Result<(), Box<dyn Error>> {
        let data = match &op {
            WriteOp::Set { data, .. } | WriteOp::Update { data, .. } => Some(data),
            WriteOp::Delete { .. } => None,
        };
        if let Some(data) = data {
            for (field, value) in data {
                reject_non_document_value(field, value)?;
            }
        }
        match &mut self.exec {
            ExecCtx::Normal => {
                self.check_wall_clock()?;
                let result = self.services.firestore.apply(vec![op]).map_err(backend_err);
                // A write that already persisted must not be turned into a
                // timeout by a later re-check: the caller would retry and apply
                // an `increment()` twice. The RPC itself is covered by the
                // backend deadline configured in `with_limits`.
                result
            }
            ExecCtx::Batch { writes } | ExecCtx::Transaction { writes } => {
                writes.push(op);
                Ok(())
            }
        }
    }

    /// Evaluate one expression and validate the resulting `Value` against the
    /// node/depth budgets.
    ///
    /// The check runs on the value the script observes, so a deeply nested
    /// result cannot be materialized and handed to the script before the
    /// budget rejects it.
    pub(super) fn eval_expr(&mut self, expr: &Expr) -> Result<ExecFlow, Box<dyn Error>> {
        let flow = self
            .eval_expr_inner(expr)
            .map_err(|e| attach_expr_span(e, expr.span))?;
        if let ExecFlow::Value(value) = &flow {
            self.check_value(value)
                .map_err(|e| attach_expr_span(e, expr.span))?;
        }
        Ok(flow)
    }

    pub(super) fn is_budget_error(err: &(dyn Error + 'static)) -> bool {
        err.downcast_ref::<BudgetError>().is_some()
            || err.downcast_ref::<QueryResultLimitExceeded>().is_some()
    }

    pub(super) fn eval_expr_inner(&mut self, expr: &Expr) -> Result<ExecFlow, Box<dyn Error>> {
        match &expr.kind {
            ExprKind::Int(i) => Ok(ExecFlow::Value(Value::Int(*i))),
            ExprKind::Str(s) => Ok(ExecFlow::Value(Value::Str(s.clone()))),
            ExprKind::StrInterp(pieces) => {
                let mut out = String::new();
                for piece in pieces {
                    match piece {
                        StrPiece::Lit(s) => {
                            if let Some(max) = self.limits.max_string_bytes {
                                if out.len().saturating_add(s.len()) > max {
                                    return Err(BudgetError::new(format!(
                                        "string size limit exceeded (max {max})"
                                    ))
                                    .into());
                                }
                            }
                            out.push_str(s);
                        }
                        StrPiece::Expr(e) => {
                            let v = match self.eval_expr(e)? {
                                ExecFlow::Value(v) => v,
                                flow @ ExecFlow::Return(_) => return Ok(flow),
                            };
                            let s = v.to_string();
                            if let Some(max) = self.limits.max_string_bytes {
                                if out.len().saturating_add(s.len()) > max {
                                    return Err(BudgetError::new(format!(
                                        "string size limit exceeded (max {max})"
                                    ))
                                    .into());
                                }
                            }
                            out.push_str(&s);
                        }
                    }
                }
                Ok(ExecFlow::Value(Value::Str(out)))
            }
            ExprKind::Bool(b) => Ok(ExecFlow::Value(Value::Bool(*b))),
            ExprKind::Nil => Ok(ExecFlow::Value(Value::Nil)),
            ExprKind::Var(name) => match self.get_var(name).cloned() {
                Some(value) => Ok(ExecFlow::Value(value)),
                None => BuiltinModule::from_name(name)
                    .map(|module| ExecFlow::Value(Value::Module(module)))
                    .ok_or_else(|| RuntimeError::UndefinedVariable(name.clone()).into()),
            },
            ExprKind::Binary { left, op, right } => {
                let l = match self.eval_expr(left)? {
                    ExecFlow::Value(v) => v,
                    flow @ ExecFlow::Return(_) => return Ok(flow),
                };
                // `&&` / `||` は短絡評価し、Ruby と同じくオペランドそのものを返す。
                match op {
                    BinOp::And => {
                        if !l.truthy() {
                            Ok(ExecFlow::Value(l))
                        } else {
                            self.eval_expr(right)
                        }
                    }
                    BinOp::Or => {
                        if l.truthy() {
                            Ok(ExecFlow::Value(l))
                        } else {
                            self.eval_expr(right)
                        }
                    }
                    _ => {
                        let r = match self.eval_expr(right)? {
                            ExecFlow::Value(v) => v,
                            flow @ ExecFlow::Return(_) => return Ok(flow),
                        };
                        if *op == BinOp::Add {
                            if let (Value::Str(a), Value::Str(b)) = (&l, &r) {
                                if let Some(max) = self.limits.max_string_bytes {
                                    if a.len().saturating_add(b.len()) > max {
                                        return Err(BudgetError::new(format!(
                                            "string size limit exceeded (max {max})"
                                        ))
                                        .into());
                                    }
                                }
                            }
                            if let (Value::Array(a), Value::Array(b)) = (&l, &r) {
                                if self.limits.max_value_nodes.is_some()
                                    || self.limits.max_value_depth.is_some()
                                {
                                    let mut count = 1usize;
                                    let mut stack: Vec<(&Value, usize)> =
                                        a.iter().chain(b.iter()).map(|v| (v, 1usize)).collect();
                                    while let Some((val, depth)) = stack.pop() {
                                        if let Some(max) = self.limits.max_value_depth {
                                            if depth > max {
                                                return Err(BudgetError::new(format!(
                                                    "value nesting limit exceeded (max {max})"
                                                ))
                                                .into());
                                            }
                                        }
                                        if let Some(max) = self.limits.max_value_nodes {
                                            count = count.saturating_add(1);
                                            if count > max {
                                                return Err(BudgetError::new(format!(
                                                    "value node limit exceeded (max {max})"
                                                ))
                                                .into());
                                            }
                                        }
                                        let child_depth = depth.saturating_add(1);
                                        match val {
                                            Value::Array(items)
                                            | Value::ArrayUnion(items)
                                            | Value::ArrayRemove(items) => {
                                                stack
                                                    .extend(items.iter().map(|v| (v, child_depth)));
                                            }
                                            Value::Map(map) => {
                                                stack
                                                    .extend(map.values().map(|v| (v, child_depth)));
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                            }
                        }
                        eval_binary(l, *op, r).map(ExecFlow::Value)
                    }
                }
            }
            ExprKind::Unary { op, expr } => {
                let v = match self.eval_expr(expr)? {
                    ExecFlow::Value(v) => v,
                    flow @ ExecFlow::Return(_) => return Ok(flow),
                };
                Ok(ExecFlow::Value(match op {
                    UnOp::Not => Value::Bool(!v.truthy()),
                    UnOp::Neg => match v {
                        Value::Int(i) => Value::Int(i.checked_neg().ok_or("integer overflow")?),
                        _ => {
                            return Err(
                                format!("unsupported unary - operand: {}", v.type_name()).into()
                            )
                        }
                    },
                }))
            }
            ExprKind::Map(entries) => {
                let mut out = BTreeMap::new();
                for (k, expr) in entries {
                    match self.eval_expr(expr)? {
                        ExecFlow::Value(v) => {
                            out.insert(k.clone(), v);
                        }
                        flow @ ExecFlow::Return(_) => return Ok(flow),
                    }
                }
                Ok(ExecFlow::Value(Value::Map(out)))
            }
            ExprKind::Array(items) => {
                let mut out = vec![];
                for item in items {
                    match self.eval_expr(item)? {
                        ExecFlow::Value(v) => out.push(v),
                        flow @ ExecFlow::Return(_) => return Ok(flow),
                    }
                }
                Ok(ExecFlow::Value(Value::Array(out)))
            }
            ExprKind::Index { base, index } => {
                let base = match self.eval_expr(base)? {
                    ExecFlow::Value(v) => v,
                    flow @ ExecFlow::Return(_) => return Ok(flow),
                };
                let index = match self.eval_expr(index)? {
                    ExecFlow::Value(v) => v,
                    flow @ ExecFlow::Return(_) => return Ok(flow),
                };
                eval_index(base, index).map(ExecFlow::Value)
            }
            ExprKind::FieldAccess { receiver, name } => {
                let base = match self.eval_expr(receiver)? {
                    ExecFlow::Value(v) => v,
                    flow @ ExecFlow::Return(_) => return Ok(flow),
                };
                self.eval_field_access(base, name).map(ExecFlow::Value)
            }
            ExprKind::Call {
                receiver,
                name,
                args,
                block,
            } => {
                // Count the call before evaluating its arguments. A deeply
                // nested call expression must consume depth as well, so the
                // guard is entered before the receiver runs.
                self.enter_call()?;
                let outcome =
                    self.eval_call_body(receiver.as_deref(), name, args, block.as_ref(), expr.span);
                self.leave_call();
                outcome
            }
            ExprKind::If {
                cond,
                then_body,
                else_body,
            } => {
                let cond = match self.eval_expr(cond)? {
                    ExecFlow::Value(v) => v,
                    flow @ ExecFlow::Return(_) => return Ok(flow),
                };
                let body = if cond.truthy() { then_body } else { else_body };
                self.eval_block(body)
            }
            ExprKind::While { cond, body } => {
                let mut last = Value::Nil;
                loop {
                    // The condition evaluation itself is a checkpoint: a long
                    // body must not be able to hide past the wall-clock budget.
                    self.check_wall_clock()?;
                    let cond = match self.eval_expr(cond)? {
                        ExecFlow::Value(v) => v,
                        flow @ ExecFlow::Return(_) => return Ok(flow),
                    };
                    if !cond.truthy() {
                        break;
                    }
                    self.consume_loop_iteration()?;
                    match self.eval_block(body)? {
                        ExecFlow::Value(v) => last = v,
                        flow @ ExecFlow::Return(_) => return Ok(flow),
                    }
                }
                Ok(ExecFlow::Value(last))
            }
            ExprKind::TryRescue {
                try_body,
                err_param,
                rescue_body,
            } => {
                let prev_writes_len = match &self.exec {
                    ExecCtx::Batch { writes } | ExecCtx::Transaction { writes } => {
                        Some(writes.len())
                    }
                    ExecCtx::Normal => None,
                };
                let scope = ScopeGuard::block(self);
                let try_result = scope.runtime.eval_block(try_body);
                drop(scope);

                match try_result {
                    Ok(flow) => Ok(flow),
                    Err(err) => {
                        if Self::is_budget_error(&*err) {
                            return Err(err);
                        }
                        // Discard any staged Firestore writes buffered during the failed try body,
                        // preserving writes staged before entering try. Unstaged side effects (e.g. File I/O)
                        // are outside the transaction buffer and cannot be rolled back.
                        if let Some(prev_len) = prev_writes_len {
                            match &mut self.exec {
                                ExecCtx::Batch { writes } | ExecCtx::Transaction { writes } => {
                                    writes.truncate(prev_len);
                                }
                                ExecCtx::Normal => {}
                            }
                        }

                        let (kind_str, message_str) =
                            if let Some(uerr) = err.downcast_ref::<UserRaisedError>() {
                                ("Runtime", uerr.message.clone())
                            } else if let Some(ferr) = err.downcast_ref::<FlareError>() {
                                let k = match ferr.kind {
                                    ErrorKind::Runtime => "Runtime",
                                    ErrorKind::Backend => "Backend",
                                    ErrorKind::Parse => "Parse",
                                    ErrorKind::Lex => "Lex",
                                };
                                (k, ferr.message.clone())
                            } else if let Some(cap) = err.downcast_ref::<CapabilityError>() {
                                ("Runtime", cap.to_string())
                            } else {
                                ("Runtime", err.to_string())
                            };
                        let mut err_map = BTreeMap::new();
                        err_map.insert("kind".to_string(), Value::Str(kind_str.to_string()));
                        err_map.insert("message".to_string(), Value::Str(message_str));
                        let err_val = Value::Map(err_map);

                        let scope = ScopeGuard::block(self);
                        if let Some(param) = err_param {
                            scope.runtime.set_var(param, err_val)?;
                        }
                        let rescue_flow = scope.runtime.eval_block(rescue_body);
                        drop(scope);
                        rescue_flow
                    }
                }
            }
            ExprKind::Fn { params, body } => {
                validate_func_params(params, "function")?;
                Ok(ExecFlow::Value(Value::Func(UserFunc::new(
                    params.clone(),
                    body.clone(),
                ))))
            }
            ExprKind::Invoke {
                callee,
                args,
                block,
            } => {
                self.enter_call()?;
                let outcome = self.eval_invoke_body(callee, args, block.as_ref(), expr.span);
                self.leave_call();
                outcome
            }
        }
    }

    fn eval_field_access(&mut self, base: Value, name: &str) -> Result<Value, Box<dyn Error>> {
        match base {
            Value::Map(map) => Ok(map.get(name).cloned().unwrap_or(Value::Nil)),
            Value::AuthSession(session) => Ok(auth_session_field(&session, name)),
            Value::Module(module) => {
                if let Some(spec) = lookup_module_builtin(module, name) {
                    if spec.contract.positional == PositionalArity::Exact(0)
                        && spec.contract.block == BlockPolicy::Forbidden
                    {
                        if let Some(service) = spec.required_service {
                            self.require_service(service)?;
                        }
                        let flow = (spec.handler)(self, vec![], BTreeMap::new(), None)?;
                        return Ok(flow.into_value());
                    }
                }
                Err(RuntimeError::UnknownModuleMethod {
                    module: module.name().to_string(),
                    name: name.to_string(),
                }
                .into())
            }
            _ => Err(RuntimeError::UnsupportedFieldAccess.into()),
        }
    }

    /// Evaluate one call expression. Separated from the `match` arm so the
    /// depth guard can pair `enter_call` with `leave_call` on every exit path.
    pub(super) fn eval_call_body(
        &mut self,
        receiver: Option<&Expr>,
        name: &str,
        args: &[CallArg],
        block: Option<&Block>,
        call_span: Span,
    ) -> Result<ExecFlow, Box<dyn Error>> {
        {
            // Validate the call shape before evaluating argument
            // expressions.  Invalid calls must not be able to perform
            // writes (or other side effects) before their contract error
            // is reported.  Keyword values are irrelevant for this
            // pre-check; only their names and arity are needed.
            // A receiver is the first expression in a method call.  Keep
            // this evaluation here (rather than in `eval_call`) so a
            // receiver failure is returned before any argument expression
            // can run.
            validate_unique_keywords(args)?;
            let receiver_value = match receiver {
                Some(expr) => {
                    let value = match self.eval_expr(expr)? {
                        ExecFlow::Value(v) => v,
                        flow @ ExecFlow::Return(_) => return Ok(flow),
                    };
                    check_method_receiver(&value, name)?;
                    Some(value)
                }
                None => None,
            };
            let has_keywords = args.iter().any(|a| matches!(a, CallArg::Keyword { .. }));
            let keyword_names = if has_keywords {
                keyword_names(args)
            } else {
                BTreeMap::new()
            };
            let spec = if let Some(Value::Module(module)) = receiver_value.as_ref() {
                let spec = module_builtin_spec(*module, name)?;
                let label = format!("{}.{name}", module.name());
                validate_call_contract(
                    &label,
                    positional_arg_count(args),
                    &keyword_names,
                    block,
                    spec.contract,
                )?;
                Some(spec)
            } else if let Some(rec) = receiver_value.as_ref() {
                validate_method_call(rec, name, positional_arg_count(args), &keyword_names, block)?;
                None
            } else if let Some(spec) = lookup_global_builtin(name) {
                validate_call_contract(
                    name,
                    positional_arg_count(args),
                    &keyword_names,
                    block,
                    spec.contract,
                )?;
                Some(spec)
            } else {
                let Some(var) = self.get_var(name) else {
                    return Err(RuntimeError::UndefinedFunction(name.to_string()).into());
                };
                let Value::Func(func) = var else {
                    return Err(
                        RuntimeError::CannotCallNonFunction(var.type_name().to_string()).into(),
                    );
                };
                validate_user_call_contract(name, &func.params, args, block)?;
                None
            };

            let required_service = spec.and_then(|spec| spec.required_service).or_else(|| {
                receiver_value
                    .as_ref()
                    .and_then(|receiver| required_service_for_method(receiver, name))
            });
            if let Some(service) = required_service {
                self.require_service(service)?;
            }
            self.validate_call_context(receiver_value.as_ref(), name, spec)?;

            // `args` is one ordered sequence; splitting it into
            // positional/keyword collections for binding must not change
            // the order in which the expressions above are evaluated.
            let call_args = match self.eval_call_args(args)? {
                ControlFlow::Continue(pair) => pair,
                ControlFlow::Break(flow) => return Ok(flow),
            };
            self.eval_call(receiver_value, name, spec, call_args, block, call_span)
        }
    }

    pub(super) fn eval_invoke_body(
        &mut self,
        callee: &Expr,
        args: &[CallArg],
        block: Option<&Block>,
        call_span: Span,
    ) -> Result<ExecFlow, Box<dyn Error>> {
        validate_unique_keywords(args)?;
        let callee_val = match self.eval_expr(callee)? {
            ExecFlow::Value(v) => v,
            flow @ ExecFlow::Return(_) => return Ok(flow),
        };
        let func = match callee_val {
            Value::Func(f) => f,
            other => {
                return Err(
                    RuntimeError::CannotCallNonFunction(other.type_name().to_string()).into(),
                );
            }
        };
        validate_user_call_contract("anonymous function", &func.params, args, block)?;

        let (argv, kw) = match self.eval_call_args(args)? {
            ControlFlow::Continue(pair) => pair,
            ControlFlow::Break(flow) => return Ok(flow),
        };
        self.call_user_func(&func, argv, kw)
            .map_err(|e| add_call_frame(e, "<anonymous>", call_span))
    }

    /// Evaluate a call argument list into positional and keyword value
    /// collections, short-circuiting on `Return` from any argument expression.
    fn eval_call_args(
        &mut self,
        args: &[CallArg],
    ) -> Result<ControlFlow<ExecFlow, CallArgValues>, Box<dyn Error>> {
        let mut argv = vec![];
        let mut kw = BTreeMap::new();
        for arg in args {
            match arg {
                CallArg::Positional(expr) => match self.eval_expr(expr)? {
                    ExecFlow::Value(v) => argv.push(v),
                    flow @ ExecFlow::Return(_) => return Ok(ControlFlow::Break(flow)),
                },
                CallArg::Keyword { name, value } => match self.eval_expr(value)? {
                    ExecFlow::Value(v) => {
                        kw.insert(name.clone(), v);
                    }
                    flow @ ExecFlow::Return(_) => return Ok(ControlFlow::Break(flow)),
                },
            }
        }
        Ok(ControlFlow::Continue((argv, kw)))
    }

    fn validate_call_context(
        &self,
        receiver: Option<&Value>,
        name: &str,
        spec: Option<BuiltinSpec>,
    ) -> Result<(), Box<dyn Error>> {
        if matches!(
            spec.and_then(|spec| spec.module),
            Some(BuiltinModule::Storage)
        ) {
            return forbid_storage_in_batch_or_transaction(self);
        }
        let is_staging_call = spec.is_some_and(|spec| {
            spec.module == Some(BuiltinModule::Firestore)
                && matches!(spec.method, "batch" | "transaction")
        });
        if is_staging_call && !matches!(self.exec, ExecCtx::Normal) {
            return Err(RuntimeError::NestedStaging(name.to_string()).into());
        }
        if matches!(name, "get" | "count")
            && matches!(
                receiver,
                Some(Value::CollectionRef(_) | Value::CollectionGroupRef(_) | Value::QueryRef(_))
            )
        {
            return self.forbid_query_in_batch_or_transaction();
        }
        let is_document_get = spec.is_some_and(|spec| {
            spec.module == Some(BuiltinModule::Firestore) && spec.method == "get"
        });
        if is_document_get {
            match &self.exec {
                ExecCtx::Batch { .. } => return Err(RuntimeError::GetInBatch.into()),
                ExecCtx::Transaction { writes } if !writes.is_empty() => {
                    return Err(RuntimeError::GetAfterWriteInTransaction.into());
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub(super) fn eval_call(
        &mut self,
        receiver: Option<Value>,
        name: &str,
        spec: Option<BuiltinSpec>,
        (args, kwargs): CallArgValues,
        block: Option<&Block>,
        call_span: Span,
    ) -> Result<ExecFlow, Box<dyn Error>> {
        if let Some(rec) = receiver {
            if let Value::Module(module) = rec {
                let spec = match spec {
                    Some(spec) => spec,
                    None => module_builtin_spec(module, name)?,
                };
                return (spec.handler)(self, args, kwargs, block);
            }
            if !kwargs.is_empty() {
                return Err("keyword args are not supported for method calls yet".into());
            }
            return eval_method_call(self, rec, name, args, kwargs, block);
        }

        // ビルトインはテーブル駆動で引く。ユーザー定義より優先されるため、
        // 同名の fn 定義は `Stmt::Def` 側でエラーにする(is_builtin_name)。
        if let Some(spec) = spec.or_else(|| lookup_global_builtin(name)) {
            return (spec.handler)(self, args, kwargs, block);
        }

        let Some(var) = self.get_var(name).cloned() else {
            return Err(RuntimeError::UndefinedFunction(name.to_string()).into());
        };
        let Value::Func(func) = var else {
            return Err(RuntimeError::CannotCallNonFunction(var.type_name().to_string()).into());
        };
        self.call_user_func(&func, args, kwargs)
            .map_err(|e| add_call_frame(e, name, call_span))
    }

    pub(super) fn call_user_func(
        &mut self,
        func: &UserFunc,
        args: Vec<Value>,
        mut kwargs: BTreeMap<String, Value>,
    ) -> Result<ExecFlow, Box<dyn Error>> {
        let scope = ScopeGuard::function(self);
        if kwargs.is_empty() && args.len() == func.params.len() {
            for (param, value) in func.params.iter().zip(args) {
                scope.runtime.set_var(&param.name, value)?;
            }
        } else {
            // Callers must pre-validate via `validate_user_call_contract` so default
            // expressions do not run on invalid calls and side effects stay off.
            let mut bound = Vec::with_capacity(func.params.len());
            let mut idx = 0;
            for param in &*func.params {
                if idx < args.len() {
                    bound.push((param.name.clone(), args[idx].clone()));
                    idx += 1;
                    continue;
                }
                if let Some(v) = kwargs.remove(&param.name) {
                    bound.push((param.name.clone(), v));
                    continue;
                }
                if let Some(default_expr) = &param.default {
                    let flow = scope.runtime.eval_expr(default_expr)?;
                    let v = match flow {
                        ExecFlow::Value(v) => v,
                        flow @ ExecFlow::Return(_) => return Ok(flow),
                    };
                    bound.push((param.name.clone(), v));
                    continue;
                }
                unreachable!("missing argument caught by pre-validation");
            }
            for (name, val) in bound {
                scope.runtime.set_var(&name, val)?;
            }
        }
        let result = scope.runtime.eval_block(&func.body);
        drop(scope);
        // 関数呼び出しは制御フロー境界。関数本体(または本体内のブロック)からの
        // Return は、この関数の戻り値として消費する。外側のブロックへ伝播するのは
        // call_block や batch/transaction のブロックから直接来た Return だけ。
        result.map(|flow| ExecFlow::Value(flow.into_value()))
    }
    pub(super) fn call_block(
        &mut self,
        block: &Block,
        args: Vec<Value>,
    ) -> Result<ExecFlow, Box<dyn Error>> {
        check_block_arity(block, args.len())?;
        let scope = ScopeGuard::block(self);
        // `ast::Block` is public with public fields, so a hand-built Block (via
        // the public `eval_program`) can carry duplicate params even though the
        // parser now rejects them. Without this, the only remaining path into
        // duplicates reported a misleading "reassignment is not allowed: <name>".
        validate_unique_params(&block.params, "block")?;
        for (param, value) in block.params.iter().zip(args) {
            scope.runtime.set_var(param, value)?;
        }
        // `call_block` propagates Return to its caller (unlike `eval_call`,
        // which consumes it): the `each`/`map` dispatch decides per item.
        let result = scope.runtime.eval_block(&block.body);
        drop(scope);
        result
    }
    pub(super) fn get_var(&self, name: &str) -> Option<&Value> {
        for scope in self.scopes[self.current_frame_base..].iter().rev() {
            if let Some(v) = scope.get(name) {
                return Some(v);
            }
        }
        if self.current_frame_base > 0 {
            if let Some(v) = self.scopes[0].get(name) {
                return Some(v);
            }
        }
        None
    }

    pub(super) fn set_var(&mut self, name: &str, value: Value) -> Result<(), Box<dyn Error>> {
        let is_reassign = self.scopes.last().expect("scope").contains_key(name);
        if is_reassign
            && !(self.repl_mode && self.current_frame_base == 0 && self.scopes.len() == 1)
        {
            return Err(RuntimeError::ReassignmentNotAllowed(name.to_string()).into());
        }
        self.scopes
            .last_mut()
            .expect("scope")
            .insert(name.to_string(), value);
        Ok(())
    }

    pub(super) fn var_exists(&self, name: &str) -> bool {
        self.scopes[self.current_frame_base..]
            .iter()
            .rev()
            .any(|s| s.contains_key(name))
            || (self.current_frame_base > 0 && self.scopes[0].contains_key(name))
    }

    fn require_service(&self, service: ServiceId) -> Result<(), Box<dyn Error>> {
        let available = match service {
            ServiceId::Firestore => self.services.firestore.is_available(),
            ServiceId::Auth => self.services.auth.is_available(),
            ServiceId::Storage => self.services.storage.is_available(),
            ServiceId::IO => self.services.io.is_available(),
        };
        if available {
            Ok(())
        } else {
            Err(CapabilityError::new(service).into())
        }
    }
}
