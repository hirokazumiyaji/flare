use std::ops::{Deref, DerefMut};

use crate::error::{CapabilityError, ServiceId};
use crate::services::ServiceAccess;

use super::state::RuntimeState;
use super::*;

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
    // `Some` is a function frame and contains the caller-local suffix. `None`
    // is a block frame, which is removed from the end on drop.
    saved: Option<Vec<HashMap<String, Value>>>,
}

impl<'guard, 'runtime> ScopeGuard<'guard, 'runtime> {
    pub(super) fn function(runtime: &'guard mut Evaluator<'runtime>) -> Self {
        let saved = runtime.scopes.split_off(1);
        runtime.scopes.push(HashMap::new());
        Self {
            runtime,
            saved: Some(saved),
        }
    }

    pub(super) fn block(runtime: &'guard mut Evaluator<'runtime>) -> Self {
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
            return Err(FlareError::runtime(format!(
                "wall-clock limit exceeded (max {})",
                format_duration(limit)
            ))
            .into());
        }
        Ok(())
    }

    pub(super) fn consume_step(&mut self) -> Result<(), Box<dyn Error>> {
        self.check_wall_clock()?;
        if let Some(max) = self.limits.max_steps {
            if self.usage.steps >= max {
                return Err(FlareError::runtime(format!("step limit exceeded (max {max})")).into());
            }
        }
        self.usage.steps = self.usage.steps.saturating_add(1);
        Ok(())
    }

    pub(super) fn consume_loop_iteration(&mut self) -> Result<(), Box<dyn Error>> {
        self.check_wall_clock()?;
        if let Some(max) = self.limits.max_loop_iterations {
            if self.usage.loop_iterations >= max {
                return Err(FlareError::runtime(format!(
                    "loop iteration limit exceeded (max {max})"
                ))
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
                return Err(
                    FlareError::runtime(format!("call depth limit exceeded (max {max})")).into(),
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
                if self.var_exists(name) {
                    return Err(format!("reassignment is not allowed: {name}").into());
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
                    return Err(format!("cannot redefine builtin: {name}").into());
                }
                if self.var_exists(name) {
                    return Err(format!("reassignment is not allowed: {name}").into());
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

    pub(super) fn forbid_query_in_batch_or_transaction(&self) -> Result<(), Box<dyn Error>> {
        match self.exec {
            ExecCtx::Batch { .. } => Err("queries are not allowed inside batch".into()),
            ExecCtx::Transaction { .. } => Err("queries are not allowed inside transaction".into()),
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
                                    return Err(FlareError::runtime(format!(
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
                                    return Err(FlareError::runtime(format!(
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
            Expr::Bool(b) => Ok(ExecFlow::Value(Value::Bool(*b))),
            Expr::Nil => Ok(ExecFlow::Value(Value::Nil)),
            Expr::Var(name) => match self.get_var(name).cloned() {
                Some(value) => Ok(ExecFlow::Value(value)),
                None => BuiltinModule::from_name(name)
                    .map(|module| ExecFlow::Value(Value::Module(module)))
                    .ok_or_else(|| format!("undefined variable: {name}").into()),
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
                Some(expr) => Some(into_method_receiver(match self.eval_expr(expr)? {
                    ExecFlow::Value(v) => v,
                    flow @ ExecFlow::Return(_) => return Ok(flow),
                })?),
                None => None,
            };
            let spec = if let Some(MethodReceiver::Module(module)) = receiver_value.as_ref() {
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
                    return Err(format!("undefined function: {name}").into());
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
        receiver: Option<&MethodReceiver>,
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
            return Err(format!("nested {name} is not allowed").into());
        }
        if matches!(name, "get" | "count")
            && matches!(
                receiver,
                Some(
                    MethodReceiver::Collection(_)
                        | MethodReceiver::CollectionGroup(_)
                        | MethodReceiver::Query(_)
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
                ExecCtx::Batch { .. } => return Err("get is not allowed inside batch".into()),
                ExecCtx::Transaction { writes } if !writes.is_empty() => {
                    return Err("get is not allowed after the first write in a transaction".into());
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub(super) fn eval_call(
        &mut self,
        receiver: Option<MethodReceiver>,
        name: &str,
        args: Vec<Value>,
        kwargs: BTreeMap<String, Value>,
        block: Option<&Block>,
    ) -> Result<ExecFlow, Box<dyn Error>> {
        if let Some(rec) = receiver {
            if let MethodReceiver::Module(module) = rec {
                let spec = lookup_module_builtin(module, name)
                    .ok_or_else(|| format!("unknown module method: {}.{name}", module.name()))?;
                return (spec.handler)(self, args, kwargs, block);
            }
            if !kwargs.is_empty() {
                return Err("keyword args are not supported for method calls yet".into());
            }
            return self.eval_method_call(rec, name, args, kwargs, block);
        }

        // ビルトインはテーブル駆動で引く。ユーザー定義より優先されるため、
        // 同名の fn 定義は `Stmt::Def` 側でエラーにする(is_builtin_name)。
        if let Some(spec) = lookup_global_builtin(name) {
            return (spec.handler)(self, args, kwargs, block);
        }

        let Some(Value::Func(func)) = self.get_var(name).cloned() else {
            return Err(format!("undefined function: {name}").into());
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

    pub(super) fn eval_method_call(
        &mut self,
        receiver: MethodReceiver,
        name: &str,
        args: Vec<Value>,
        kwargs: BTreeMap<String, Value>,
        block: Option<&Block>,
    ) -> Result<ExecFlow, Box<dyn Error>> {
        validate_method_call(&receiver, name, args.len(), &kwargs, block)?;
        self.eval_method_call_unchecked(receiver, name, args, kwargs, block)
    }

    pub(super) fn eval_method_call_unchecked(
        &mut self,
        receiver: MethodReceiver,
        name: &str,
        args: Vec<Value>,
        _kwargs: BTreeMap<String, Value>,
        block: Option<&Block>,
    ) -> Result<ExecFlow, Box<dyn Error>> {
        match receiver {
            MethodReceiver::Collection(collection) if name == "doc" => {
                let id = expect_string(args.first())?;
                validate_document_id(id)?;
                Ok(ExecFlow::Value(Value::DocRef(DocRef {
                    path: format!("{}/{}", collection.path, id),
                })))
            }
            MethodReceiver::Collection(collection) => self
                .eval_query_method(collection.to_query(), name, args)
                .map(ExecFlow::Value),
            MethodReceiver::CollectionGroup(group) => self
                .eval_query_method(group.to_query(), name, args)
                .map(ExecFlow::Value),
            MethodReceiver::Query(query) => self
                .eval_query_method(query, name, args)
                .map(ExecFlow::Value),
            MethodReceiver::Array(items) => self.eval_array_method(items, name, args, block),
            MethodReceiver::Timestamp(ts) => match name {
                "to_unix" => Ok(ExecFlow::Value(Value::Int(ts.seconds))),
                "to_rfc3339" => Ok(ExecFlow::Value(Value::Str(ts.to_rfc3339()?))),
                "add_seconds" => Ok(ExecFlow::Value(Value::Timestamp(Timestamp {
                    seconds: ts
                        .seconds
                        .checked_add(expect_int(args.first())?)
                        .ok_or("integer overflow")?,
                }))),

                _ => Err(format!("unknown timestamp method: {name}").into()),
            },
            MethodReceiver::Bytes(bytes) if name == "len" && args.is_empty() => {
                Ok(ExecFlow::Value(Value::Int(bytes.len() as i64)))
            }
            MethodReceiver::Bytes(_) => Err(format!("unknown bytes method: {name}").into()),
            MethodReceiver::Map(map) => {
                // Map レシーバは `m.key` 形式のキーアクセス(Ruby の OpenStruct 風)のみ。
                // 引数を取るメソッドは持たないので、Args がある場合はエラーにする。
                if !args.is_empty() {
                    return Err(format!("unknown map method: {name}").into());
                }
                Ok(ExecFlow::Value(
                    map.get(name).cloned().unwrap_or(Value::Nil),
                ))
            }
            MethodReceiver::AuthSession(session) => {
                if !args.is_empty() {
                    return Err(format!("unknown auth session method: {name}").into());
                }
                Ok(ExecFlow::Value(auth_session_field(&session, name)))
            }
            MethodReceiver::Module(module) => {
                let spec = lookup_module_builtin(module, name)
                    .ok_or_else(|| format!("unknown module method: {}.{name}", module.name()))?;
                (spec.handler)(self, args, BTreeMap::new(), block)
            }
        }
    }

    pub(super) fn eval_array_method(
        &mut self,
        items: Vec<Value>,
        name: &str,
        args: Vec<Value>,
        block: Option<&Block>,
    ) -> Result<ExecFlow, Box<dyn Error>> {
        if !args.is_empty() {
            return Err(format!("array method {name} does not take arguments").into());
        }
        match name {
            "each" => {
                let Some(block) = block else {
                    return Err("each requires { ... } block".into());
                };
                // 空配列でも呼び出し側の引数個数違反を検出できるよう、
                // ループの前にブロック引数の個数を検証する。
                check_block_arity(block, 1)?;
                for item in items {
                    self.consume_loop_iteration()?;
                    match self.call_block(block, vec![item])? {
                        ExecFlow::Value(_) => {}
                        ExecFlow::Return(v) => return Ok(ExecFlow::Return(v)),
                    }
                }
                Ok(ExecFlow::Value(Value::Nil))
            }
            "map" => {
                let Some(block) = block else {
                    return Err("map requires { ... } block".into());
                };
                check_block_arity(block, 1)?;
                let mut out = vec![];
                for item in items {
                    self.consume_loop_iteration()?;
                    match self.call_block(block, vec![item])? {
                        ExecFlow::Value(v) => out.push(v),
                        ExecFlow::Return(v) => return Ok(ExecFlow::Return(v)),
                    }
                }
                Ok(ExecFlow::Value(Value::Array(out)))
            }
            "len" => Ok(ExecFlow::Value(Value::Int(items.len() as i64))),
            _ => Err(format!("unknown array method: {name}").into()),
        }
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

    pub(super) fn eval_query_method(
        &mut self,
        mut query: QueryRef,
        name: &str,
        args: Vec<Value>,
    ) -> Result<Value, Box<dyn Error>> {
        match name {
            "where" => {
                // Field path は正規化してから保存する。in-memory の lookup と
                // emulator の wire 表現が同じ spelling を共有する。
                let field = normalize_query_field_path(expect_string(args.first())?)?;
                let op = QueryOp::parse(expect_string(args.get(1))?)?;
                let value = args.get(2).ok_or("where requires value")?;
                reject_non_document_value(&field, value)?;
                query.filters.push(QueryFilter {
                    field,
                    op,
                    value: value.clone(),
                });
                Ok(Value::QueryRef(query))
            }
            "limit" => {
                let n = expect_int(args.first())?;
                let n = usize::try_from(n).map_err(|_| "limit must be >= 0")?;
                query.limit = Some(n);
                Ok(Value::QueryRef(query))
            }
            "offset" => {
                let n = expect_int(args.first())?;
                let n = usize::try_from(n).map_err(|_| "offset must be >= 0")?;
                query.offset = Some(n);
                Ok(Value::QueryRef(query))
            }
            "order_by" => {
                let field = normalize_query_field_path(expect_string(args.first())?)?;
                let dir = Direction::parse(expect_string(args.get(1))?)?;
                query.order_by.push((field, dir));
                Ok(Value::QueryRef(query))
            }
            "count" => {
                self.forbid_query_in_batch_or_transaction()?;
                // 集計はフィルタ一致件数を数える。順序・offset・limit は無視する。
                let mut unpaged = query.clone();
                unpaged.order_by.clear();
                unpaged.offset = None;
                unpaged.limit = None;
                // `count` returns a number, not rows. Counting still walks
                // every match, so it must respect the candidate budget too.
                Ok(Value::Int(self.count_query_with_budget(&unpaged)?))
            }
            "get" => {
                self.forbid_query_in_batch_or_transaction()?;
                let rows = self.run_query_with_budget(&query)?;
                let result = Value::Array(
                    rows.into_iter()
                        .map(|(path, doc)| wrap_doc(&path, &doc))
                        .collect(),
                );
                // `run_query_with_budget` checks each document field, but the
                // value handed to the script also contains the row maps, their
                // `__name__` entries, and the enclosing array.
                self.check_value(&result)?;
                Ok(result)
            }
            _ => Err(format!("unknown query method: {name}").into()),
        }
    }

    /// Count query matches under the result budget.
    ///
    /// `count_query` returns a number, so nothing is materialized; the budget
    /// still applies because counting walks every match.
    pub(super) fn count_query_with_budget(
        &mut self,
        query: &QueryRef,
    ) -> Result<i64, Box<dyn Error>> {
        self.check_wall_clock()?;
        let max_results = self.limits.max_query_results;
        let count = match self.services.firestore.count_query(query) {
            Ok(count) => count,
            Err(e) => {
                if let Some(limit) = e.downcast_ref::<QueryResultLimitExceeded>() {
                    self.usage.max_query_results =
                        self.usage.max_query_results.max(limit.max_results);
                    return Err(FlareError::runtime(limit.to_string()).into());
                }
                return Err(backend_err(e));
            }
        };
        self.check_wall_clock()?;
        let count = usize::try_from(count)
            .map_err(|_| "query result is too large to represent as an integer".to_string())?;
        if let Some(max) = max_results {
            if count > max {
                self.usage.max_query_results = self.usage.max_query_results.max(max);
                return Err(FlareError::runtime(format!(
                    "query result limit exceeded (max {max})"
                ))
                .into());
            }
        }
        self.usage.query_results = self.usage.query_results.saturating_add(count as u64);
        self.usage.max_query_results = self.usage.max_query_results.max(count);
        Ok(count as i64)
    }

    /// Run a query page under the result budget.
    pub(super) fn run_query_with_budget(
        &mut self,
        query: &QueryRef,
    ) -> Result<QueryCandidates, Box<dyn Error>> {
        self.check_wall_clock()?;
        let max_results = self.limits.max_query_results;
        // The bounded hook is the resource boundary: a backend that refuses to
        // materialize more than `max` rows errors instead of returning a
        // partial vector. The length check below stays as a defense for legacy
        // backends that keep the trait's compatibility fallback.
        let result = match max_results {
            Some(max) => self.services.firestore.run_query_bounded(query, max),
            None => self.services.firestore.run_query_page(query),
        };
        let rows = match result {
            Ok(rows) => rows,
            Err(e) => {
                // Budget consumption stays observable on the failure path, but
                // only for the dedicated marker: a genuine backend error must
                // not be recorded as a budget failure.
                if let Some(limit) = e.downcast_ref::<QueryResultLimitExceeded>() {
                    self.usage.max_query_results =
                        self.usage.max_query_results.max(limit.max_results);
                    return Err(FlareError::runtime(limit.to_string()).into());
                }
                return Err(backend_err(e));
            }
        };
        self.check_wall_clock()?;

        let count = rows.len();
        if let Some(max) = max_results {
            if count > max {
                self.usage.max_query_results = self.usage.max_query_results.max(max);
                return Err(FlareError::runtime(format!(
                    "query result limit exceeded (max {max})"
                ))
                .into());
            }
        }
        self.usage.query_results = self.usage.query_results.saturating_add(count as u64);
        self.usage.max_query_results = self.usage.max_query_results.max(count);
        for (_, fields) in &rows {
            for value in fields.values() {
                self.check_value(value)?;
            }
        }
        Ok(rows)
    }

    pub(super) fn get_var(&self, name: &str) -> Option<&Value> {
        for scope in self.scopes.iter().rev() {
            if let Some(v) = scope.get(name) {
                return Some(v);
            }
        }
        None
    }

    pub(super) fn set_var(&mut self, name: &str, value: Value) -> Result<(), Box<dyn Error>> {
        if self.scopes.last().expect("scope").contains_key(name) {
            return Err(format!("reassignment is not allowed: {name}").into());
        }
        self.scopes
            .last_mut()
            .expect("scope")
            .insert(name.to_string(), value);
        Ok(())
    }

    pub(super) fn var_exists(&self, name: &str) -> bool {
        self.scopes.iter().rev().any(|s| s.contains_key(name))
    }

    fn require_service(&self, service: ServiceId) -> Result<(), Box<dyn Error>> {
        let available = match service {
            ServiceId::Firestore => self.services.firestore.is_available(),
            ServiceId::Auth => self.services.auth.is_available(),
            ServiceId::Storage => self.services.storage.is_available(),
        };
        if available {
            Ok(())
        } else {
            Err(CapabilityError::new(service).into())
        }
    }
}

fn required_service_for_method(receiver: &MethodReceiver, name: &str) -> Option<ServiceId> {
    match receiver {
        MethodReceiver::Collection(_)
        | MethodReceiver::CollectionGroup(_)
        | MethodReceiver::Query(_)
            if name == "get" || name == "count" =>
        {
            Some(ServiceId::Firestore)
        }
        MethodReceiver::Module(module) => {
            lookup_module_builtin(*module, name).and_then(|spec| spec.required_service)
        }
        _ => None,
    }
}

pub(super) type Builtin = for<'a> fn(
    &mut Evaluator<'a>,
    Vec<Value>,
    BTreeMap<String, Value>,
    Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>>;

#[derive(Clone, Copy)]
pub(super) struct BuiltinSpec {
    pub(super) module: Option<BuiltinModule>,
    pub(super) method: &'static str,
    pub(super) global_name: &'static str,
    pub(super) contract: CallContract,
    pub(super) required_service: Option<ServiceId>,
    pub(super) handler: Builtin,
}

fn spec(
    module: Option<BuiltinModule>,
    method: &'static str,
    global_name: &'static str,
    contract: CallContract,
    required_service: Option<ServiceId>,
    handler: Builtin,
) -> BuiltinSpec {
    BuiltinSpec {
        module,
        method,
        global_name,
        contract,
        required_service,
        handler,
    }
}

pub(super) fn builtin_specs() -> [BuiltinSpec; 28] {
    use BuiltinModule::{Auth, Firestore, Storage};
    use ServiceId::{
        Auth as AuthService, Firestore as FirestoreService, Storage as StorageService,
    };

    [
        spec(
            None,
            "puts",
            "puts",
            CallContract::any(),
            None,
            builtin_puts,
        ),
        spec(
            Some(Firestore),
            "doc",
            "doc",
            CallContract::exact(1),
            None,
            builtin_doc,
        ),
        spec(
            Some(Firestore),
            "col",
            "col",
            CallContract::exact(1),
            None,
            builtin_col,
        ),
        spec(
            Some(Firestore),
            "col_group",
            "col_group",
            CallContract::exact(1),
            None,
            builtin_col_group,
        ),
        spec(
            None,
            "array",
            "array",
            CallContract::any(),
            None,
            builtin_array,
        ),
        spec(
            None,
            "len",
            "len",
            CallContract::exact(1),
            None,
            builtin_len,
        ),
        spec(
            None,
            "now",
            "now",
            CallContract::exact(0),
            None,
            builtin_now,
        ),
        spec(
            None,
            "timestamp",
            "timestamp",
            CallContract::exact(1),
            None,
            builtin_timestamp,
        ),
        spec(
            None,
            "bytes",
            "bytes",
            CallContract::exact(1),
            None,
            builtin_bytes,
        ),
        spec(
            Some(Firestore),
            "server_timestamp",
            "server_timestamp",
            CallContract::exact(0),
            None,
            builtin_server_timestamp,
        ),
        spec(
            Some(Firestore),
            "array_union",
            "array_union",
            CallContract::exact(1),
            None,
            builtin_array_union,
        ),
        spec(
            Some(Firestore),
            "array_remove",
            "array_remove",
            CallContract::exact(1),
            None,
            builtin_array_remove,
        ),
        spec(
            Some(Firestore),
            "delete_field",
            "delete_field",
            CallContract::exact(0),
            None,
            builtin_delete_field,
        ),
        spec(
            Some(Firestore),
            "increment",
            "increment",
            CallContract::exact(1).with_missing_hint("expected int argument"),
            None,
            builtin_increment,
        ),
        spec(
            Some(Firestore),
            "get",
            "get",
            CallContract::exact(1),
            Some(FirestoreService),
            builtin_get,
        ),
        spec(
            Some(Firestore),
            "set",
            "set",
            CallContract::range(2, 3)
                .with_keywords(&["merge"])
                .with_positional_keyword_conflicts(&[("merge", 2)]),
            Some(FirestoreService),
            builtin_set,
        ),
        spec(
            Some(Firestore),
            "update",
            "update",
            CallContract::exact(2),
            Some(FirestoreService),
            builtin_update,
        ),
        spec(
            Some(Firestore),
            "delete",
            "delete",
            CallContract::exact(1),
            Some(FirestoreService),
            builtin_delete,
        ),
        spec(
            Some(Firestore),
            "batch",
            "batch",
            CallContract::exact(0).requiring_block(),
            Some(FirestoreService),
            builtin_batch,
        ),
        spec(
            Some(Firestore),
            "transaction",
            "transaction",
            CallContract::exact(0).requiring_block(),
            Some(FirestoreService),
            builtin_transaction,
        ),
        spec(
            Some(Storage),
            "upload",
            "storage_upload",
            CallContract::range(2, 3)
                .with_keywords(&["content_type"])
                .with_positional_keyword_conflicts(&[("content_type", 2)]),
            Some(StorageService),
            builtin_storage_upload,
        ),
        spec(
            Some(Storage),
            "download",
            "storage_download",
            CallContract::exact(1),
            Some(StorageService),
            builtin_storage_download,
        ),
        spec(
            Some(Storage),
            "metadata",
            "storage_metadata",
            CallContract::exact(1),
            Some(StorageService),
            builtin_storage_metadata,
        ),
        spec(
            Some(Storage),
            "delete",
            "storage_delete",
            CallContract::exact(1),
            Some(StorageService),
            builtin_storage_delete,
        ),
        spec(
            Some(Auth),
            "sign_up",
            "auth_sign_up",
            CallContract::exact(2),
            Some(AuthService),
            builtin_auth_sign_up,
        ),
        spec(
            Some(Auth),
            "sign_in",
            "auth_sign_in",
            CallContract::exact(2),
            Some(AuthService),
            builtin_auth_sign_in,
        ),
        spec(
            Some(Auth),
            "current_user",
            "auth_current_user",
            CallContract::exact(0),
            Some(AuthService),
            builtin_auth_current_user,
        ),
        spec(
            Some(Auth),
            "sign_out",
            "auth_sign_out",
            CallContract::exact(0),
            Some(AuthService),
            builtin_auth_sign_out,
        ),
    ]
}

pub(super) fn lookup_global_builtin(name: &str) -> Option<BuiltinSpec> {
    builtin_specs()
        .into_iter()
        .find(|spec| spec.global_name == name)
}

pub(super) fn lookup_module_builtin(module: BuiltinModule, method: &str) -> Option<BuiltinSpec> {
    builtin_specs()
        .into_iter()
        .find(|spec| spec.module == Some(module) && spec.method == method)
}

pub(super) fn is_builtin_name(name: &str) -> bool {
    lookup_global_builtin(name).is_some()
}

fn builtin_puts(
    _rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    for v in args {
        println!("{v}");
    }
    Ok(ExecFlow::Value(Value::Nil))
}

fn builtin_doc(
    _rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let path = expect_string(args.first())?;
    validate_doc_path(path)?;
    Ok(ExecFlow::Value(Value::DocRef(DocRef {
        path: path.to_string(),
    })))
}

fn builtin_col(
    _rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let path = expect_string(args.first())?;
    validate_collection_path(path)?;
    Ok(ExecFlow::Value(Value::CollectionRef(CollectionRef {
        path: path.to_string(),
    })))
}

fn builtin_col_group(
    _rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let id = expect_string(args.first())?;
    validate_collection_group_id(id)?;
    Ok(ExecFlow::Value(Value::CollectionGroupRef(
        CollectionGroupRef { id: id.to_string() },
    )))
}

fn builtin_array(
    _rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    Ok(ExecFlow::Value(Value::Array(args)))
}

fn builtin_len(
    _rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    match args.first() {
        Some(Value::Array(a)) => Ok(ExecFlow::Value(Value::Int(a.len() as i64))),
        Some(Value::Map(m)) => Ok(ExecFlow::Value(Value::Int(m.len() as i64))),
        Some(Value::Str(s)) => Ok(ExecFlow::Value(Value::Int(s.chars().count() as i64))),
        Some(Value::Bytes(bytes)) => Ok(ExecFlow::Value(Value::Int(bytes.len() as i64))),
        _ => Err("len expects array, map, string, or bytes".into()),
    }
}

fn builtin_now(
    _rt: &mut Evaluator<'_>,
    _args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    Ok(ExecFlow::Value(Value::Timestamp(Timestamp::now()?)))
}

fn builtin_timestamp(
    _rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    Ok(ExecFlow::Value(Value::Timestamp(Timestamp {
        seconds: parse_timestamp_arg(args.first())?,
    })))
}

fn builtin_bytes(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    kwargs: BTreeMap<String, Value>,
    block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    if args.len() != 1 || !kwargs.is_empty() || block.is_some() {
        return Err(BYTES_USAGE.into());
    }
    // `args.len() == 1` は上で保証されているので `next()` は必ず `Some`。
    // 取り出しを下の `match` にまとめ、到達不能な `ok_or` 分岐を作らない。
    let bytes = match args.into_iter().next() {
        Some(Value::Str(s)) => {
            if let Some(max) = rt.limits.max_bytes_size {
                if s.len() > max {
                    return Err(FlareError::runtime(format!(
                        "bytes size limit exceeded (max {max})"
                    ))
                    .into());
                }
            }
            s.into_bytes()
        }
        Some(Value::Array(items)) => {
            if let Some(max) = rt.limits.max_bytes_size {
                if items.len() > max {
                    return Err(FlareError::runtime(format!(
                        "bytes size limit exceeded (max {max})"
                    ))
                    .into());
                }
            }
            items
                .into_iter()
                .map(|value| match value {
                    Value::Int(byte) => u8::try_from(byte).map_err(|_| BYTES_ELEMENT_ERR.to_string()),
                    _ => Err(BYTES_ELEMENT_ERR.to_string()),
                })
                .collect::<Result<Vec<_>, _>>()?
        }
        _ => return Err(BYTES_USAGE.into()),
    };
    Ok(ExecFlow::Value(Value::Bytes(bytes)))
}

fn builtin_server_timestamp(
    _rt: &mut Evaluator<'_>,
    _args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    Ok(ExecFlow::Value(Value::ServerTimestamp))
}

fn builtin_array_union(
    _rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    Ok(ExecFlow::Value(Value::ArrayUnion(
        expect_array(args.first())?.clone(),
    )))
}

fn builtin_array_remove(
    _rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    Ok(ExecFlow::Value(Value::ArrayRemove(
        expect_array(args.first())?.clone(),
    )))
}

fn builtin_delete_field(
    _rt: &mut Evaluator<'_>,
    _args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    Ok(ExecFlow::Value(Value::DeleteField))
}

fn builtin_increment(
    _rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    Ok(ExecFlow::Value(Value::Increment(expect_int(args.first())?)))
}

fn builtin_storage_upload(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    forbid_storage_in_batch_or_transaction(rt)?;
    reject_unknown_kwargs("storage_upload", &kwargs, &["content_type"])?;
    if args.len() > 3 {
        return Err("storage_upload expects path, bytes, and optional content_type".into());
    }
    let path = expect_storage_path(args.first())?;
    let content = expect_storage_bytes(args.get(1))?;
    let content_type = match (args.get(2), kwargs.get("content_type")) {
        (Some(Value::Str(value)), None) => value.as_str(),
        (None, Some(Value::Str(value))) => value.as_str(),
        (None, None) => "application/octet-stream",
        (Some(_), Some(_)) => {
            return Err(
                "storage_upload received content_type twice (positional and keyword)".into(),
            )
        }
        (Some(_), None) => return Err("storage_upload content_type must be a string".into()),
        (None, Some(_)) => return Err("storage_upload content_type must be a string".into()),
    };
    validate_storage_path(path)?;
    validate_content_type(content_type)?;
    rt.services
        .storage
        .upload(path, content, content_type)
        .map(|metadata| ExecFlow::Value(metadata.to_value()))
        .map_err(backend_err)
}

fn builtin_storage_download(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    forbid_storage_in_batch_or_transaction(rt)?;
    reject_storage_kwargs("storage_download", &kwargs)?;
    expect_storage_arity("storage_download", &args, 1)?;
    let path = expect_storage_path(args.first())?;
    validate_storage_path(path)?;
    let bytes = rt.services.storage.download(path).map_err(backend_err)?;
    if let Some(max) = rt.limits.max_bytes_size {
        if bytes.len() > max {
            return Err(FlareError::runtime(format!(
                "bytes size limit exceeded (max {max})"
            ))
            .into());
        }
    }
    Ok(ExecFlow::Value(Value::Bytes(bytes)))
}

fn builtin_storage_metadata(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    forbid_storage_in_batch_or_transaction(rt)?;
    reject_storage_kwargs("storage_metadata", &kwargs)?;
    expect_storage_arity("storage_metadata", &args, 1)?;
    let path = expect_storage_path(args.first())?;
    validate_storage_path(path)?;
    rt.services
        .storage
        .metadata(path)
        .map(|metadata| ExecFlow::Value(metadata.to_value()))
        .map_err(backend_err)
}

fn builtin_storage_delete(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    forbid_storage_in_batch_or_transaction(rt)?;
    reject_storage_kwargs("storage_delete", &kwargs)?;
    expect_storage_arity("storage_delete", &args, 1)?;
    let path = expect_storage_path(args.first())?;
    validate_storage_path(path)?;
    rt.services.storage.delete(path).map_err(backend_err)?;
    Ok(ExecFlow::Value(Value::Nil))
}

fn forbid_storage_in_batch_or_transaction(rt: &Evaluator<'_>) -> Result<(), Box<dyn Error>> {
    match rt.exec {
        ExecCtx::Batch { .. } => Err("storage APIs are not allowed inside batch".into()),
        ExecCtx::Transaction { .. } => {
            Err("storage APIs are not allowed inside transaction".into())
        }
        ExecCtx::Normal => Ok(()),
    }
}

fn reject_storage_kwargs(
    name: &str,
    kwargs: &BTreeMap<String, Value>,
) -> Result<(), Box<dyn Error>> {
    if kwargs.is_empty() {
        Ok(())
    } else {
        Err(format!("{name} does not take keyword arguments").into())
    }
}

fn reject_unknown_kwargs(
    name: &str,
    kwargs: &BTreeMap<String, Value>,
    allowed: &[&str],
) -> Result<(), Box<dyn Error>> {
    let unknown: Vec<&str> = kwargs
        .keys()
        .filter(|key| !allowed.contains(&key.as_str()))
        .map(String::as_str)
        .collect();
    if unknown.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{name} received unknown keyword arguments: {}",
            unknown.join(", ")
        )
        .into())
    }
}

fn expect_storage_arity(name: &str, args: &[Value], expected: usize) -> Result<(), Box<dyn Error>> {
    if args.len() == expected {
        Ok(())
    } else {
        Err(format!("{name} expects {expected} argument(s), got {}", args.len()).into())
    }
}

fn expect_storage_path(v: Option<&Value>) -> Result<&str, Box<dyn Error>> {
    match v {
        Some(Value::Str(path)) => Ok(path),
        _ => Err("storage API expects an object path string".into()),
    }
}

fn expect_storage_bytes(v: Option<&Value>) -> Result<&[u8], Box<dyn Error>> {
    match v {
        Some(Value::Bytes(bytes)) => Ok(bytes),
        Some(_) => Err("storage_upload expects bytes(...) content".into()),
        None => Err("storage_upload requires bytes(...) content".into()),
    }
}

fn builtin_get(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let path = expect_doc_path(args.first())?;
    rt.check_wall_clock()?;
    let v = match &rt.exec {
        ExecCtx::Batch { .. } => {
            return Err("get is not allowed inside batch".into());
        }
        ExecCtx::Transaction { writes } if !writes.is_empty() => {
            return Err("get is not allowed after the first write in a transaction".into());
        }
        ExecCtx::Transaction { .. } => rt
            .services
            .firestore
            .transaction_get(path)
            .map_err(backend_err)?
            .unwrap_or(Value::Nil),
        ExecCtx::Normal => rt
            .services
            .firestore
            .get(path)
            .map_err(backend_err)?
            .unwrap_or(Value::Nil),
    };
    // `get` only reads, so a deadline that fires afterwards cannot invalidate
    // work the script already observed.
    rt.check_wall_clock()?;
    Ok(ExecFlow::Value(v))
}

fn builtin_set(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let path = expect_doc_path(args.first())?.to_string();
    let data = expect_map(args.get(1))?.clone();
    validate_write_data(&data)?;
    let merge = match (args.get(2), kwargs.get("merge")) {
        (_, Some(Value::Bool(v))) => *v,
        (Some(Value::Bool(v)), None) => *v,
        (None, None) => false,
        _ => return Err("set merge must be bool".into()),
    };
    rt.dispatch_write(WriteOp::Set { path, data, merge })?;
    Ok(ExecFlow::Value(Value::Nil))
}

fn builtin_update(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let path = expect_doc_path(args.first())?.to_string();
    let data = expect_map(args.get(1))?.clone();
    validate_write_data(&data)?;
    rt.dispatch_write(WriteOp::Update { path, data })?;
    Ok(ExecFlow::Value(Value::Nil))
}

fn builtin_delete(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let path = expect_doc_path(args.first())?.to_string();
    rt.dispatch_write(WriteOp::Delete { path })?;
    Ok(ExecFlow::Value(Value::Nil))
}

fn builtin_batch(
    rt: &mut Evaluator<'_>,
    _args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let Some(stmts) = block else {
        return Err("batch requires { ... } block".into());
    };
    if !stmts.params.is_empty() {
        return Err("batch block does not take parameters".into());
    }
    if !matches!(rt.exec, ExecCtx::Normal) {
        return Err("batch cannot be nested inside batch/transaction".into());
    }
    let (flow, writes) = rt.run_staged_block(ExecCtx::Batch { writes: vec![] }, &stmts.body);
    let flow = flow?;
    rt.check_wall_clock()?;
    // `apply` は永続書き込みなので、ここでは wall-clock を再検査しない。
    // 書き込み後の timeout 判定は「成功したはずの write」を失敗として返し、
    // 呼び出し側の再試行と重なって二重書き込みを招く。実際の RPC には
    // `Runtime::with_limits` が設定した backend deadline が効く。
    rt.services.firestore.apply(writes).map_err(backend_err)?;
    Ok(flow)
}

fn builtin_transaction(
    rt: &mut Evaluator<'_>,
    _args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let Some(stmts) = block else {
        return Err("transaction requires { ... } block".into());
    };
    if !stmts.params.is_empty() {
        return Err("transaction block does not take parameters".into());
    }
    if !matches!(rt.exec, ExecCtx::Normal) {
        return Err("transaction cannot be nested inside batch/transaction".into());
    }
    // A previous transaction may have left the backend with an open
    // transaction because its rollback failed. Clear it before starting a
    // new one, otherwise the begin below fails forever.
    rt.retry_pending_rollback();
    rt.check_wall_clock()?;
    rt.services
        .firestore
        .begin_transaction()
        .map_err(backend_err)?;
    // `begin_transaction` は遅いことがあるため、budget 超過を begin 後に
    // 検出したら必ず abort する。さもないと server transaction が開き続け、
    // 以降の `begin_transaction` が "already in transaction" で失敗する。
    if let Err(e) = rt.check_wall_clock() {
        rt.abort_transaction_best_effort();
        return Err(e);
    }
    let (flow, writes) = rt.run_staged_block(ExecCtx::Transaction { writes: vec![] }, &stmts.body);
    match flow {
        Ok(flow) => {
            // commit 前の budget 超過でも transaction を開いたままにしない。
            // in-memory は tx_snapshot、emulator は transaction_id を保持し、
            // どちらの backend も以降の transaction を "already in
            // transaction" で失敗させるため、ここは abort が必要。
            if let Err(e) = rt.check_wall_clock() {
                rt.abort_transaction_best_effort();
                return Err(e);
            }
            if let Err(e) = rt.services.firestore.commit_transaction(writes) {
                rt.abort_transaction_best_effort();
                return Err(backend_err(e));
            }
            // commit 後も wall-clock を再検査しない。batch と同じく、
            // 永続化済みの操作を timeout として返すと再試行が二重書き込みに
            // つながる。commit 自体の deadline は backend timeout が担保する。
            Ok(flow)
        }
        Err(e) => {
            // Preserve the block error if rollback also fails.
            rt.abort_transaction_best_effort();
            Err(e)
        }
    }
}

fn builtin_auth_sign_up(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    kwargs: BTreeMap<String, Value>,
    block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    reject_auth_call_args("auth_sign_up", &args, &kwargs, block)?;
    let email = expect_string(args.first())?.to_string();
    let password = expect_string(args.get(1))?.to_string();
    let session = rt
        .services
        .auth
        .sign_up(&email, &password)
        .map_err(auth_backend_err)?;
    Ok(ExecFlow::Value(Value::AuthSession(session)))
}

fn builtin_auth_sign_in(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    kwargs: BTreeMap<String, Value>,
    block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    reject_auth_call_args("auth_sign_in", &args, &kwargs, block)?;
    let email = expect_string(args.first())?.to_string();
    let password = expect_string(args.get(1))?.to_string();
    let session = rt
        .services
        .auth
        .sign_in(&email, &password)
        .map_err(auth_backend_err)?;
    Ok(ExecFlow::Value(Value::AuthSession(session)))
}

fn builtin_auth_current_user(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    kwargs: BTreeMap<String, Value>,
    block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    reject_auth_call_args("auth_current_user", &args, &kwargs, block)?;
    Ok(ExecFlow::Value(
        rt.services
            .auth
            .current_user()
            .map_err(auth_backend_err)?
            .map(|user| user.to_value())
            .unwrap_or(Value::Nil),
    ))
}

fn builtin_auth_sign_out(
    rt: &mut Evaluator<'_>,
    args: Vec<Value>,
    kwargs: BTreeMap<String, Value>,
    block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    reject_auth_call_args("auth_sign_out", &args, &kwargs, block)?;
    rt.services.auth.sign_out().map_err(auth_backend_err)?;
    Ok(ExecFlow::Value(Value::Nil))
}

fn reject_auth_call_args(
    name: &str,
    args: &[Value],
    kwargs: &BTreeMap<String, Value>,
    block: Option<&Block>,
) -> Result<(), Box<dyn Error>> {
    if (name == "auth_current_user" || name == "auth_sign_out") && !args.is_empty() {
        return Err(format!("{name} does not take arguments").into());
    }
    if args.len() > 2 {
        return Err(format!("{name} takes at most 2 arguments").into());
    }
    if !kwargs.is_empty() {
        return Err(format!("{name} does not accept keyword arguments").into());
    }
    if block.is_some() {
        return Err(format!("{name} does not accept a block").into());
    }
    Ok(())
}

fn auth_backend_err(error: AuthError) -> Box<dyn Error> {
    FlareError::auth(error.kind(), error.to_string()).into()
}

pub(super) fn auth_session_field(session: &crate::auth::AuthSession, name: &str) -> Value {
    match name {
        "user" => session.user_value(),
        "id_token" => Value::RedactedToken("id_token"),
        "refresh_token" => Value::RedactedToken("refresh_token"),
        "expires_at" => Value::Int(session.expires_at),
        _ => Value::Nil,
    }
}
