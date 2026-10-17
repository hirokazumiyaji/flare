use std::ops::{Deref, DerefMut};

use crate::error::{CapabilityError, ServiceId};
use crate::services::ServiceAccess;
use crate::value::BuiltinModule;

use super::builtins::storage::forbid_storage_in_batch_or_transaction;
use super::builtins::{
    is_builtin_name, lookup_global_builtin, lookup_module_builtin, BuiltinSpec,
};
use super::contract::{
    bind_function_args, keyword_names, positional_arg_count, validate_call_contract,
    validate_unique_keywords, validate_unique_params, validate_user_call_contract,
};
use super::error::RuntimeError;
use super::methods::{eval_method_call, required_service_for_method, validate_method_call};
use super::state::RuntimeState;
use super::*;

pub(crate) use super::ExecFlow;

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
pub(crate) struct ScopeGuard<'guard, 'runtime> {
    pub(crate) runtime: &'guard mut Evaluator<'runtime>,
    // `Some` is a function frame and contains the caller-local suffix. `None`
    // is a block frame, which is removed from the end on drop.
    saved: Option<Vec<HashMap<String, Value>>>,
}

impl<'guard, 'runtime> ScopeGuard<'guard, 'runtime> {
    pub(crate) fn function(runtime: &'guard mut Evaluator<'runtime>) -> Self {
        let saved = runtime.scopes.split_off(1);
        runtime.scopes.push(HashMap::new());
        Self {
            runtime,
            saved: Some(saved),
        }
    }

    pub(crate) fn block(runtime: &'guard mut Evaluator<'runtime>) -> Self {
        runtime.scopes.push(HashMap::new());
        Self {
            runtime,
            saved: None,
        }
    }
}

impl Drop for ScopeGuard<'_, '_> {
    fn drop(&mut self) {
        if let Some(mut saved) = self.saved.take() {
            // Keep the global frame and discard every frame created while the
            // function was running before restoring the caller-local suffix.
            self.runtime.scopes.truncate(1);
            self.runtime.scopes.append(&mut saved);
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
            return Err(FlareError::runtime(
                RuntimeError::WallClockLimitExceeded(format_duration(limit)).to_string(),
            )
            .into());
        }
        Ok(())
    }

    pub(super) fn consume_step(&mut self) -> Result<(), Box<dyn Error>> {
        self.check_wall_clock()?;
        if let Some(max) = self.limits.max_steps {
            if self.usage.steps >= max {
                return Err(FlareError::runtime(
                    RuntimeError::StepLimitExceeded(max).to_string(),
                )
                .into());
            }
        }
        self.usage.steps = self.usage.steps.saturating_add(1);
        Ok(())
    }

    pub(super) fn consume_loop_iteration(&mut self) -> Result<(), Box<dyn Error>> {
        self.check_wall_clock()?;
        if let Some(max) = self.limits.max_loop_iterations {
            if self.usage.loop_iterations >= max {
                return Err(FlareError::runtime(
                    RuntimeError::LoopIterationLimitExceeded(max).to_string(),
                )
                .into());
            }
        }
        self.usage.loop_iterations = self.usage.loop_iterations.saturating_add(1);
        Ok(())
    }

    pub(super) fn enter_call(&mut self) -> Result<(), Box<dyn Error>> {
        self.check_wall_clock()?;
        if let Some(max) = self.limits.max_call_depth {
            if self.call_depth >= max {
                return Err(FlareError::runtime(
                    RuntimeError::CallDepthLimitExceeded(max).to_string(),
                )
                .into());
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
            value::validate_value_limits_with_sizes(
                value,
                self.limits.max_value_depth,
                self.limits.max_value_nodes.map(|budget| budget.min(1)),
                self.limits.max_string_bytes,
                self.limits.max_bytes_size,
            )?;
            return Ok(());
        }
        let observed_depth = value::validate_value_limits_with_sizes(
            value,
            self.limits.max_value_depth,
            self.limits.max_value_nodes,
            self.limits.max_string_bytes,
            self.limits.max_bytes_size,
        )?;
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
                if self.var_exists(name) && !(self.repl_mode && self.scopes.len() == 1) {
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
                validate_unique_params(params, "function")?;
                if is_builtin_name(name) {
                    return Err(RuntimeError::CannotRedefineBuiltin(name.to_string()).into());
                }
                if self.var_exists(name) && !(self.repl_mode && self.scopes.len() == 1) {
                    return Err(RuntimeError::ReassignmentNotAllowed(name.to_string()).into());
                }
                self.set_var(
                    name,
                    Value::Func(UserFunc {
                        params: params.clone(),
                        body: body.clone(),
                    }),
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
            Stmt::If {
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
            Stmt::While { cond, body } => {
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
                        ExecFlow::Return(v) => return Ok(ExecFlow::Return(v)),
                    }
                }
                Ok(ExecFlow::Value(last))
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

    pub(crate) fn forbid_query_in_batch_or_transaction(&self) -> Result<(), Box<dyn Error>> {
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
        let flow = self.eval_expr_inner(expr)?;
        if let ExecFlow::Value(value) = &flow {
            self.check_value(value)?;
        }
        Ok(flow)
    }

    pub(super) fn eval_expr_inner(&mut self, expr: &Expr) -> Result<ExecFlow, Box<dyn Error>> {
        match expr {
            Expr::Int(i) => Ok(ExecFlow::Value(Value::Int(*i))),
            Expr::Str(s) => Ok(ExecFlow::Value(Value::Str(s.clone()))),
            Expr::StrInterp(pieces) => {
                let mut out = String::new();
                for piece in pieces {
                    match piece {
                        StrPiece::Lit(s) => {
                            if let Some(max) = self.limits.max_string_bytes {
                                if out.len().saturating_add(s.len()) > max {
                                    return Err(FlareError::runtime(
                                        RuntimeError::StringSizeLimitExceeded(max).to_string(),
                                    )
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
                                    return Err(FlareError::runtime(
                                        RuntimeError::StringSizeLimitExceeded(max).to_string(),
                                    )
                                    .into());
                                }
                            }
                            out.push_str(&s);
                        }
                    }
                }
                Ok(ExecFlow::Value(Value::Str(out)))
            }
            Expr::Bool(b) => Ok(ExecFlow::Value(Value::Bool(*b))),
            Expr::Nil => Ok(ExecFlow::Value(Value::Nil)),
            Expr::Var(name) => match self.get_var(name).cloned() {
                Some(value) => Ok(ExecFlow::Value(value)),
                None => BuiltinModule::from_name(name)
                    .map(|module| ExecFlow::Value(Value::Module(module)))
                    .ok_or_else(|| RuntimeError::UndefinedVariable(name.clone()).into()),
            },
            Expr::Binary { left, op, right } => {
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
                                        return Err(FlareError::runtime(format!(
                                            "string size limit exceeded (max {max})"
                                        ))
                                        .into());
                                    }
                                }
                            }
                        }
                        eval_binary(l, *op, r).map(ExecFlow::Value)
                    }
                }
            }
            Expr::Unary { op, expr } => {
                let v = match self.eval_expr(expr)? {
                    ExecFlow::Value(v) => v,
                    flow @ ExecFlow::Return(_) => return Ok(flow),
                };
                Ok(ExecFlow::Value(match op {
                    UnOp::Not => Value::Bool(!v.truthy()),
                    UnOp::Neg => match v {
                        Value::Int(i) => Value::Int(i.checked_neg().ok_or("integer overflow")?),
                        _ => return Err("unsupported unary - operand".into()),
                    },
                }))
            }
            Expr::Map(entries) => {
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
            Expr::Array(items) => {
                let mut out = vec![];
                for item in items {
                    match self.eval_expr(item)? {
                        ExecFlow::Value(v) => out.push(v),
                        flow @ ExecFlow::Return(_) => return Ok(flow),
                    }
                }
                Ok(ExecFlow::Value(Value::Array(out)))
            }
            Expr::Index { base, index } => {
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
            Expr::Call {
                receiver,
                name,
                args,
                block,
            } => {
                // Count the call before evaluating its arguments. A deeply
                // nested call expression must consume depth as well, so the
                // guard is entered before the receiver runs.
                self.enter_call()?;
                let outcome = self.eval_call_body(receiver.as_deref(), name, args, block.as_ref());
                self.leave_call();
                outcome
            }
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
                Some(expr) => match self.eval_expr(expr)? {
                    ExecFlow::Value(v) => Some(v),
                    flow @ ExecFlow::Return(_) => return Ok(flow),
                },
                None => None,
            };
            let spec = if let Some(Value::Module(module)) = receiver_value.as_ref() {
                let spec = lookup_module_builtin(*module, name)
                    .ok_or_else(|| format!("unknown module method: {}.{name}", module.name()))?;
                let label = format!("{}.{name}", module.name());
                validate_call_contract(
                    &label,
                    positional_arg_count(args),
                    &keyword_names(args),
                    block,
                    spec.contract,
                )?;
                Some(spec)
            } else if let Some(rec) = receiver_value.as_ref() {
                let keyword_names = keyword_names(args);
                validate_method_call(rec, name, positional_arg_count(args), &keyword_names, block)?;
                None
            } else if let Some(spec) = lookup_global_builtin(name) {
                let keyword_names = keyword_names(args);
                validate_call_contract(
                    name,
                    positional_arg_count(args),
                    &keyword_names,
                    block,
                    spec.contract,
                )?;
                Some(spec)
            } else {
                let Some(Value::Func(func)) = self.get_var(name).cloned() else {
                    return Err(RuntimeError::UndefinedFunction(name.to_string()).into());
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
            let mut argv = vec![];
            let mut kw = BTreeMap::new();
            for arg in args {
                match arg {
                    CallArg::Positional(expr) => match self.eval_expr(expr)? {
                        ExecFlow::Value(v) => argv.push(v),
                        flow @ ExecFlow::Return(_) => return Ok(flow),
                    },
                    CallArg::Keyword { name, value } => match self.eval_expr(value)? {
                        ExecFlow::Value(v) => {
                            kw.insert(name.clone(), v);
                        }
                        flow @ ExecFlow::Return(_) => return Ok(flow),
                    },
                }
            }
            self.eval_call(receiver_value, name, argv, kw, block)
        }
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
                Some(
                    Value::CollectionRef(_)
                        | Value::CollectionGroupRef(_)
                        | Value::QueryRef(_)
                )
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

    pub(crate) fn eval_call(
        &mut self,
        receiver: Option<Value>,
        name: &str,
        args: Vec<Value>,
        kwargs: BTreeMap<String, Value>,
        block: Option<&Block>,
    ) -> Result<ExecFlow, Box<dyn Error>> {
        if let Some(rec) = receiver {
            if let Value::Module(module) = rec {
                let spec = lookup_module_builtin(module, name)
                    .ok_or_else(|| format!("unknown module method: {}.{name}", module.name()))?;
                return (spec.handler)(self, args, kwargs, block);
            }
            if !kwargs.is_empty() {
                return Err("keyword args are not supported for method calls yet".into());
            }
            return eval_method_call(self, rec, name, args, block);
        }

        // ビルトインはテーブル駆動で引く。ユーザー定義より優先されるため、
        // 同名の fn 定義は `Stmt::Def` 側でエラーにする(is_builtin_name)。
        if let Some(spec) = lookup_global_builtin(name) {
            return (spec.handler)(self, args, kwargs, block);
        }

        let Some(Value::Func(func)) = self.get_var(name).cloned() else {
            return Err(RuntimeError::UndefinedFunction(name.to_string()).into());
        };
        let bound = bind_function_args(&func.params, args, kwargs)?;
        // 呼び出し元のローカル変数・呼び出された関数自身のローカルは隠す。
        //
        // caller scope の保存と frame の破棄を guard に任せる。引数の
        // binding、本体の runtime error、panic unwind のいずれでも同じ復元が
        // 進み、手動の cleanup 処理に依存しない。
        let scope = ScopeGuard::function(self);
        for (param, value) in bound {
            scope.runtime.set_var(&param, value)?;
        }
        let result = scope.runtime.eval_block(&func.body);
        drop(scope);
        // 関数呼び出しは制御フロー境界。関数本体(または本体内のブロック)からの
        // Return は、この関数の戻り値として消費する。外側のブロックへ伝播するのは
        // call_block や batch/transaction のブロックから直接来た Return だけ。
        result.map(|flow| ExecFlow::Value(flow.into_value()))
    }

    pub(crate) fn call_block(
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

    pub(crate) fn get_var(&self, name: &str) -> Option<&Value> {
        for scope in self.scopes.iter().rev() {
            if let Some(v) = scope.get(name) {
                return Some(v);
            }
        }
        None
    }

    pub(crate) fn set_var(&mut self, name: &str, value: Value) -> Result<(), Box<dyn Error>> {
        let is_reassign = self.scopes.last().expect("scope").contains_key(name);
        if is_reassign && !(self.repl_mode && self.scopes.len() == 1) {
            return Err(RuntimeError::ReassignmentNotAllowed(name.to_string()).into());
        }
        self.scopes
            .last_mut()
            .expect("scope")
            .insert(name.to_string(), value);
        Ok(())
    }

    pub(crate) fn var_exists(&self, name: &str) -> bool {
        self.scopes.iter().rev().any(|s| s.contains_key(name))
    }

    pub(crate) fn require_service(&self, service: ServiceId) -> Result<(), Box<dyn Error>> {
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
