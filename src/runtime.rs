use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::time::{Duration, Instant};

use crate::ast::{BinOp, Block, Expr, Stmt, StrPiece, UnOp};
use crate::error::{FirelangError, Span};
use crate::firestore::{Firestore, QueryCandidates, QueryResultLimitExceeded, WriteOp};
use crate::limits::{RuntimeLimits, RuntimeUsage};
use crate::parser::Parser;
use crate::value::{
    self, CollectionGroupRef, CollectionRef, Direction, DocRef, QueryFilter, QueryOp, QueryRef,
    Timestamp, UserFunc, Value,
};

enum ExecCtx {
    Normal,
    Batch { writes: Vec<WriteOp> },
    Transaction { writes: Vec<WriteOp> },
}

/// Propagate the parts of the budget that constrain the *backend*.
///
/// A backend that recurses while converting values (the emulator codec) must
/// know the depth ceiling, otherwise it overflows the native stack before the
/// runtime can validate the result. A disabled value-depth budget still needs a
/// finite number for that recursion guard, so the shipped default is used as
/// the backstop.
fn apply_backend_limits<F: Firestore>(fs: &mut F, limits: &RuntimeLimits) {
    fs.set_timeout(limits.backend_timeout);
    fs.set_max_value_depth(
        limits
            .max_value_depth
            .unwrap_or(crate::limits::DEFAULT_MAX_VALUE_DEPTH),
    );
}

pub struct Runtime<F: Firestore> {
    scopes: Vec<HashMap<String, Value>>,
    fs: F,
    exec: ExecCtx,
    limits: RuntimeLimits,
    usage: RuntimeUsage,
    call_depth: usize,
    started_at: Option<Instant>,
    execution_active: bool,
}

impl<F: Firestore> Runtime<F> {
    /// Construct a runtime with conservative, finite limits.
    pub fn new(fs: F) -> Self {
        Self::with_limits(fs, RuntimeLimits::default())
    }

    /// Construct a runtime with embedder-provided limits.
    ///
    /// The backend hook is applied to subsequent operations.  A
    /// `FirestoreEmulator` that must also apply the same deadline while
    /// connecting should be created with `FirestoreEmulator::new_with_timeout`.
    pub fn with_limits(mut fs: F, limits: RuntimeLimits) -> Self {
        apply_backend_limits(&mut fs, &limits);
        Self {
            scopes: vec![HashMap::new()],
            fs,
            exec: ExecCtx::Normal,
            limits,
            usage: RuntimeUsage::default(),
            call_depth: 0,
            started_at: None,
            execution_active: false,
        }
    }

    /// Explicit constructor for trusted scripts with all budgets disabled.
    pub fn new_unlimited(fs: F) -> Self {
        Self::with_limits(fs, RuntimeLimits::unlimited())
    }

    pub fn limits(&self) -> RuntimeLimits {
        self.limits
    }

    /// Report whether the configured backend can enforce `backend_timeout`.
    /// Legacy synchronous backends may retain the trait's compatibility
    /// no-op; the built-in emulator returns `true`.
    pub fn backend_timeout_supported(&self) -> bool {
        self.fs.supports_backend_timeout()
    }

    pub fn set_limits(&mut self, limits: RuntimeLimits) {
        self.limits = limits;
        apply_backend_limits(&mut self.fs, &self.limits);
    }

    /// Return a snapshot of the counters consumed by the most recent run.
    pub fn usage(&self) -> RuntimeUsage {
        let mut usage = self.usage;
        usage.elapsed = self
            .started_at
            .map(|start| start.elapsed())
            .unwrap_or_default();
        usage
    }

    pub fn reset_usage(&mut self) {
        self.usage = RuntimeUsage::default();
        self.call_depth = 0;
        self.started_at = None;
        self.execution_active = false;
    }

    pub fn eval_program(&mut self, stmts: &[Stmt]) -> Result<Value, Box<dyn Error>> {
        self.start_execution()?;
        let result = self
            .check_wall_clock_at_source()
            .and_then(|_| self.eval_block(stmts).map(ExecFlow::into_value));
        self.finish_execution();
        result
    }

    /// トップレベル文の開始位置付きで評価し、失敗した文の位置を
    /// runtime/backend エラーに付与する。既に span を持つ lex/parse エラーは
    /// そのまま通す。
    pub fn eval_program_with_spans(
        &mut self,
        stmts: &[Stmt],
        spans: &[Span],
    ) -> Result<Value, Box<dyn Error>> {
        self.start_execution()?;
        let result = self
            .check_wall_clock_at_source()
            .and_then(|_| self.eval_program_with_spans_active(stmts, spans));
        self.finish_execution();
        result
    }

    fn eval_program_with_spans_active(
        &mut self,
        stmts: &[Stmt],
        spans: &[Span],
    ) -> Result<Value, Box<dyn Error>> {
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
    pub fn run(&mut self, source: &str) -> Result<Value, Box<dyn Error>> {
        self.start_execution()?;
        let result = self.run_active(source);
        self.finish_execution();
        result
    }

    fn run_active(&mut self, source: &str) -> Result<Value, Box<dyn Error>> {
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
    fn check_wall_clock_at_source(&self) -> Result<(), Box<dyn Error>> {
        self.check_wall_clock()
            .map_err(|e| attach_span(e, Some(Span::new(1, 1))))
    }

    /// 実行を開始し、budget の起点を記録する。
    ///
    /// 公開エントリポイント (`run` / `eval_program*`) は再入を許さない。
    /// 許すと `start_execution` が実行中の `usage` / `started_at` を静かに
    /// リセットし、step / value / wall-clock の budget が実質無効になる。
    fn start_execution(&mut self) -> Result<(), Box<dyn Error>> {
        if self.execution_active {
            return Err(FirelangError::runtime(
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

    fn finish_execution(&mut self) {
        if let Some(started_at) = self.started_at.take() {
            self.usage.elapsed = started_at.elapsed();
        }
        self.call_depth = 0;
        self.execution_active = false;
    }

    fn check_wall_clock(&self) -> Result<(), Box<dyn Error>> {
        let Some(limit) = self.limits.max_wall_clock else {
            return Ok(());
        };
        let Some(started_at) = self.started_at else {
            return Ok(());
        };
        let elapsed = started_at.elapsed();
        if elapsed >= limit {
            return Err(FirelangError::runtime(format!(
                "wall-clock limit exceeded (max {})",
                format_duration(limit)
            ))
            .into());
        }
        Ok(())
    }

    fn consume_step(&mut self) -> Result<(), Box<dyn Error>> {
        self.check_wall_clock()?;
        if let Some(max) = self.limits.max_steps {
            if self.usage.steps >= max {
                return Err(
                    FirelangError::runtime(format!("step limit exceeded (max {max})")).into(),
                );
            }
        }
        self.usage.steps = self.usage.steps.saturating_add(1);
        Ok(())
    }

    fn consume_loop_iteration(&mut self) -> Result<(), Box<dyn Error>> {
        self.check_wall_clock()?;
        if let Some(max) = self.limits.max_loop_iterations {
            if self.usage.loop_iterations >= max {
                return Err(FirelangError::runtime(format!(
                    "loop iteration limit exceeded (max {max})"
                ))
                .into());
            }
        }
        self.usage.loop_iterations = self.usage.loop_iterations.saturating_add(1);
        Ok(())
    }

    fn enter_call(&mut self) -> Result<(), Box<dyn Error>> {
        self.check_wall_clock()?;
        if let Some(max) = self.limits.max_call_depth {
            if self.call_depth >= max {
                return Err(FirelangError::runtime(format!(
                    "call depth limit exceeded (max {max})"
                ))
                .into());
            }
        }
        self.call_depth = self.call_depth.saturating_add(1);
        self.usage.max_call_depth = self.usage.max_call_depth.max(self.call_depth);
        Ok(())
    }

    fn leave_call(&mut self) {
        self.call_depth = self.call_depth.saturating_sub(1);
    }

    /// 検証と depth 観測を 1 パスで行う。
    ///
    /// スカラー(数値・文字列・真偽値など)은子が無いため、走査せず
    /// そのまま depth 0 を返す。`value_depth` のような別の全走査は行わない
    /// ので、式評価のたびに値が二重に走査されることがない。
    fn check_value(&mut self, value: &Value) -> Result<(), Box<dyn Error>> {
        if !matches!(
            value,
            Value::Array(_) | Value::ArrayUnion(_) | Value::ArrayRemove(_) | Value::Map(_)
        ) {
            // スカラー値には子が無いので depth 0 相当。観測した最大値は
            // 既に 0 以上なので usage を更新する必要はない。
            return Ok(());
        }
        let observed_depth = value::validate_value_limits(
            value,
            self.limits.max_value_depth,
            self.limits.max_value_nodes,
        )?;
        self.usage.max_value_depth = self.usage.max_value_depth.max(observed_depth);
        Ok(())
    }

    fn eval_stmt(&mut self, stmt: &Stmt) -> Result<ExecFlow, Box<dyn Error>> {
        self.consume_step()?;
        match stmt {
            Stmt::Expr(e) => Ok(ExecFlow::Value(self.eval_expr(e)?)),
            Stmt::Assign { name, value } => {
                // ビルトイン関数名と同じ名前の変数定義(例: set = 5)は許可する。
                // 呼び出し時(Expr::Call)はビルトインディスパッチが常に優先され、
                // 変数参照時(Expr::Var)は変数テーブルから読み出されるため構文上区別できる。
                // 一方で fn 定義(Stmt::Def)は定義してもビルトインに隠されて呼び出せないため
                // 禁止している。
                if self.var_exists(name) {
                    return Err(format!("reassignment is not allowed: {name}").into());
                }
                let v = self.eval_expr(value)?;
                self.set_var(name, v.clone())?;
                Ok(ExecFlow::Value(v))
            }
            Stmt::Def { name, params, body } => {
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
            Stmt::Return(e) => Ok(ExecFlow::Return(match e {
                Some(e) => self.eval_expr(e)?,
                None => Value::Nil,
            })),
            Stmt::If {
                cond,
                then_body,
                else_body,
            } => {
                let body = if self.eval_expr(cond)?.truthy() {
                    then_body
                } else {
                    else_body
                };
                self.eval_block(body)
            }
            Stmt::While { cond, body } => {
                let mut last = Value::Nil;
                loop {
                    self.check_wall_clock()?;
                    if !self.eval_expr(cond)?.truthy() {
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

    fn eval_block(&mut self, stmts: &[Stmt]) -> Result<ExecFlow, Box<dyn Error>> {
        let mut last = Value::Nil;
        for stmt in stmts {
            match self.eval_stmt(stmt)? {
                ExecFlow::Value(v) => last = v,
                ExecFlow::Return(v) => return Ok(ExecFlow::Return(v)),
            }
        }
        Ok(ExecFlow::Value(last))
    }

    /// `exec` をステージング用 ctx に差し替えてブロックを評価し、元の ctx に
    /// 戻してステージされた writes を返す。ブロックの成否にかかわらず復元する。
    fn run_staged_block(
        &mut self,
        ctx: ExecCtx,
        body: &[Stmt],
    ) -> (Result<ExecFlow, Box<dyn Error>>, Vec<WriteOp>) {
        let prev = std::mem::replace(&mut self.exec, ctx);
        let flow = self.eval_block(body);
        let writes = match std::mem::replace(&mut self.exec, prev) {
            ExecCtx::Batch { writes } | ExecCtx::Transaction { writes } => writes,
            ExecCtx::Normal => unreachable!("exec ctx changed inside batch/transaction"),
        };
        (flow, writes)
    }

    fn forbid_query_in_batch_or_transaction(&self) -> Result<(), Box<dyn Error>> {
        match self.exec {
            ExecCtx::Batch { .. } => Err("queries are not allowed inside batch".into()),
            ExecCtx::Transaction { .. } => Err("queries are not allowed inside transaction".into()),
            ExecCtx::Normal => Ok(()),
        }
    }

    fn dispatch_write(&mut self, op: WriteOp) -> Result<(), Box<dyn Error>> {
        match &mut self.exec {
            ExecCtx::Normal => {
                self.check_wall_clock()?;
                let result = self.fs.apply(vec![op]).map_err(backend_err);
                self.check_wall_clock()?;
                result
            }
            ExecCtx::Batch { writes } | ExecCtx::Transaction { writes } => {
                writes.push(op);
                Ok(())
            }
        }
    }

    fn eval_expr(&mut self, expr: &Expr) -> Result<Value, Box<dyn Error>> {
        let value = match expr {
            Expr::Int(i) => Value::Int(*i),
            Expr::Str(s) => Value::Str(s.clone()),
            Expr::StrInterp(pieces) => {
                let mut out = String::new();
                for piece in pieces {
                    match piece {
                        StrPiece::Lit(s) => out.push_str(s),
                        StrPiece::Expr(e) => {
                            let v = self.eval_expr(e)?;
                            out.push_str(&v.to_string());
                        }
                    }
                }
                Value::Str(out)
            }
            Expr::Bool(b) => Value::Bool(*b),
            Expr::Nil => Value::Nil,
            Expr::Var(name) => self
                .get_var(name)
                .cloned()
                .ok_or_else(|| -> Box<dyn Error> {
                    format!("undefined variable: {name}").into()
                })?,
            Expr::Binary { left, op, right } => {
                // `&&` / `||` は短絡評価し、Ruby と同じくオペランドそのものを返す。
                match op {
                    BinOp::And => {
                        let l = self.eval_expr(left)?;
                        if !l.truthy() {
                            l
                        } else {
                            self.eval_expr(right)?
                        }
                    }
                    BinOp::Or => {
                        let l = self.eval_expr(left)?;
                        if l.truthy() {
                            l
                        } else {
                            self.eval_expr(right)?
                        }
                    }
                    _ => eval_binary(self.eval_expr(left)?, *op, self.eval_expr(right)?)?,
                }
            }
            Expr::Unary { op, expr } => {
                let v = self.eval_expr(expr)?;
                match op {
                    UnOp::Not => Value::Bool(!v.truthy()),
                    UnOp::Neg => match v {
                        Value::Int(i) => Value::Int(i.checked_neg().ok_or("integer overflow")?),
                        _ => return Err("unsupported unary - operand".into()),
                    },
                }
            }
            Expr::Call {
                receiver,
                name,
                args,
                kwargs,
                block,
            } => {
                // Count the call before evaluating arguments.  This makes a
                // deeply nested call expression consume depth as well as a
                // recursive user function.
                self.enter_call()?;
                let mut argv = vec![];
                let mut result = Ok(());
                for arg in args {
                    match self.eval_expr(arg) {
                        Ok(value) => argv.push(value),
                        Err(e) => {
                            result = Err(e);
                            break;
                        }
                    }
                }
                let value = if let Err(e) = result {
                    Err(e)
                } else {
                    let mut kw = BTreeMap::new();
                    let mut result = Ok(());
                    for (k, vexpr) in kwargs {
                        match self.eval_expr(vexpr) {
                            Ok(value) => {
                                kw.insert(k.clone(), value);
                            }
                            Err(e) => {
                                result = Err(e);
                                break;
                            }
                        }
                    }
                    if let Err(e) = result {
                        Err(e)
                    } else {
                        self.eval_call(receiver.as_deref(), name, argv, kw, block.as_ref())
                    }
                };
                self.leave_call();
                value?
            }
            Expr::Map(entries) => {
                let mut out = BTreeMap::new();
                for (k, expr) in entries {
                    out.insert(k.clone(), self.eval_expr(expr)?);
                }
                Value::Map(out)
            }
            Expr::Array(items) => {
                let mut out = vec![];
                for item in items {
                    out.push(self.eval_expr(item)?);
                }
                Value::Array(out)
            }
            Expr::Index { base, index } => {
                let base = self.eval_expr(base)?;
                let index = self.eval_expr(index)?;
                eval_index(base, index)?
            }
        };
        self.check_value(&value)?;
        Ok(value)
    }

    fn eval_call(
        &mut self,
        receiver: Option<&Expr>,
        name: &str,
        args: Vec<Value>,
        kwargs: BTreeMap<String, Value>,
        block: Option<&Block>,
    ) -> Result<Value, Box<dyn Error>> {
        if let Some(r) = receiver {
            let rec = self.eval_expr(r)?;
            if !kwargs.is_empty() {
                return Err("keyword args are not supported for method calls yet".into());
            }
            return self.eval_method_call(rec, name, args, block);
        }

        // ビルトインはテーブル駆動で引く。ユーザー定義より優先されるため、
        // 同名の fn 定義は `Stmt::Def` 側でエラーにする(is_builtin_name)。
        if let Some(found) = lookup_builtin(name) {
            return found(self, args, kwargs, block);
        }

        let Some(Value::Func(func)) = self.get_var(name).cloned() else {
            return Err(format!("undefined function: {name}").into());
        };
        let bound = bind_function_args(&func.params, args, kwargs)?;
        // 関数はグローバルスコープと仮引数のみを見える(静的スコープ)。
        // 呼び出し元のローカル変数・呼び出された関数自身のローカルは隠す。
        let mut saved = self.scopes.split_off(1);
        self.scopes.push(HashMap::new());
        for (param, value) in bound {
            self.set_var(&param, value)?;
        }
        let res = self.eval_block(&func.body);
        self.scopes.pop();
        self.scopes.append(&mut saved);
        Ok(match res? {
            ExecFlow::Value(v) | ExecFlow::Return(v) => v,
        })
    }

    fn eval_method_call(
        &mut self,
        receiver: Value,
        name: &str,
        args: Vec<Value>,
        block: Option<&Block>,
    ) -> Result<Value, Box<dyn Error>> {
        match receiver {
            Value::CollectionRef(collection) if name == "doc" => {
                let id = expect_string(args.first())?;
                if id.is_empty() || id.contains('/') {
                    return Err(format!("invalid document id: {id}").into());
                }
                Ok(Value::DocRef(DocRef {
                    path: format!("{}/{}", collection.path, id),
                }))
            }
            Value::CollectionRef(collection) => {
                self.eval_query_method(collection.to_query(), name, args)
            }
            Value::CollectionGroupRef(group) => {
                self.eval_query_method(group.to_query(), name, args)
            }
            Value::QueryRef(query) => self.eval_query_method(query, name, args),
            Value::Array(items) => self.eval_array_method(items, name, args, block),
            Value::Timestamp(ts) => match name {
                "to_unix" => Ok(Value::Int(ts.seconds)),
                "to_rfc3339" => Ok(Value::Str(ts.to_rfc3339()?)),
                "add_seconds" => Ok(Value::Timestamp(Timestamp {
                    seconds: ts.seconds + expect_int(args.first())?,
                })),
                _ => Err(format!("unknown timestamp method: {name}").into()),
            },
            Value::Map(map) => {
                // Map レシーバは `m.key` 形式のキーアクセス(Ruby の OpenStruct 風)のみ。
                // 引数を取るメソッドは持たないので、Args がある場合はエラーにする。
                if !args.is_empty() {
                    return Err(format!("unknown map method: {name}").into());
                }
                Ok(map.get(name).cloned().unwrap_or(Value::Nil))
            }
            _ => Err("unsupported method receiver".into()),
        }
    }

    fn eval_array_method(
        &mut self,
        items: Vec<Value>,
        name: &str,
        args: Vec<Value>,
        block: Option<&Block>,
    ) -> Result<Value, Box<dyn Error>> {
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
                    self.call_block(block, vec![item])?;
                }
                Ok(Value::Nil)
            }
            "map" => {
                let Some(block) = block else {
                    return Err("map requires { ... } block".into());
                };
                check_block_arity(block, 1)?;
                let mut out = vec![];
                for item in items {
                    self.consume_loop_iteration()?;
                    out.push(self.call_block(block, vec![item])?);
                }
                Ok(Value::Array(out))
            }
            "len" => Ok(Value::Int(items.len() as i64)),
            _ => Err(format!("unknown array method: {name}").into()),
        }
    }

    fn call_block(&mut self, block: &Block, args: Vec<Value>) -> Result<Value, Box<dyn Error>> {
        check_block_arity(block, args.len())?;
        self.scopes.push(HashMap::new());
        for (param, value) in block.params.iter().cloned().zip(args) {
            self.set_var(&param, value)?;
        }
        let res = self.eval_block(&block.body);
        self.scopes.pop();
        Ok(res?.into_value())
    }

    fn run_query_with_budget(
        &mut self,
        query: &QueryRef,
    ) -> Result<QueryCandidates, Box<dyn Error>> {
        self.check_wall_clock()?;
        // The bounded hook is the resource boundary. The length check below
        // remains as a defense for legacy backends that use the trait's
        // compatibility fallback.
        //
        // A backend that refuses to materialize more than `max` returns an
        // error instead of a partial vector, so record the budget that was hit
        // before propagating: budget consumption must stay observable even on
        // the failure path.  Only the dedicated marker counts as a budget
        // failure; a genuine backend error must not be recorded as one.
        let max_results = self.limits.max_query_results;
        let result = match max_results {
            Some(max) => self.fs.run_query_bounded(query, max),
            None => self.fs.run_query(query),
        };
        let rows = match result {
            Ok(rows) => rows,
            Err(e) => {
                if let Some(limit) = e.downcast_ref::<QueryResultLimitExceeded>() {
                    self.usage.max_query_results =
                        self.usage.max_query_results.max(limit.max_results);
                    return Err(FirelangError::runtime(limit.to_string()).into());
                }
                return Err(backend_err(e));
            }
        };
        self.check_wall_clock()?;

        let count = rows.len();
        if let Some(max) = max_results {
            if count > max {
                self.usage.max_query_results = self.usage.max_query_results.max(max);
                return Err(FirelangError::runtime(format!(
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

    fn eval_query_method(
        &mut self,
        mut query: QueryRef,
        name: &str,
        args: Vec<Value>,
    ) -> Result<Value, Box<dyn Error>> {
        match name {
            "where" => {
                query.filters.push(QueryFilter {
                    field: expect_string(args.first())?.to_string(),
                    op: QueryOp::parse(expect_string(args.get(1))?)?,
                    value: args.get(2).cloned().ok_or("where requires value")?,
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
                let field = expect_string(args.first())?.to_string();
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
                let rows = self.run_query_with_budget(&unpaged)?;
                let count = i64::try_from(rows.len()).map_err(|_| {
                    "query result is too large to represent as an integer".to_string()
                })?;
                Ok(Value::Int(count))
            }
            "get" => {
                self.forbid_query_in_batch_or_transaction()?;
                let rows = self.run_query_with_budget(&query)?;
                let result = Value::Array(apply_query_shape(rows, &query));
                // `run_query_with_budget` checks each document field, but the
                // runtime value returned to the script also includes the row
                // maps, their `__name__` entries, and the enclosing array.
                // Check that aggregate before exposing it.
                self.check_value(&result)?;
                Ok(result)
            }
            _ => Err(format!("unknown query method: {name}").into()),
        }
    }

    fn get_var(&self, name: &str) -> Option<&Value> {
        for scope in self.scopes.iter().rev() {
            if let Some(v) = scope.get(name) {
                return Some(v);
            }
        }
        None
    }

    fn set_var(&mut self, name: &str, value: Value) -> Result<(), Box<dyn Error>> {
        if self.scopes.last().expect("scope").contains_key(name) {
            return Err(format!("reassignment is not allowed: {name}").into());
        }
        self.scopes
            .last_mut()
            .expect("scope")
            .insert(name.to_string(), value);
        Ok(())
    }

    fn var_exists(&self, name: &str) -> bool {
        self.scopes.iter().rev().any(|s| s.contains_key(name))
    }
}

/// 予算を診断文に出すための表記。
///
/// ミリ秒固定だと 500µs のような sub-millisecond 予算が "max 0ms" と
/// 誤表示され、単位も分からない。`Duration` の `Debug` 表現は設定した
/// 単位に合わせて出るため、予算と診断文がずれない。
fn format_duration(duration: Duration) -> String {
    format!("{duration:?}")
}

fn bind_function_args(
    params: &[String],
    positional: Vec<Value>,
    mut kwargs: BTreeMap<String, Value>,
) -> Result<Vec<(String, Value)>, Box<dyn Error>> {
    if positional.len() > params.len() {
        return Err("too many positional args".into());
    }
    let mut out = vec![];
    let mut idx = 0;
    for param in params {
        if idx < positional.len() {
            out.push((param.clone(), positional[idx].clone()));
            idx += 1;
            continue;
        }
        if let Some(v) = kwargs.remove(param) {
            out.push((param.clone(), v));
            continue;
        }
        return Err(format!("missing argument: {param}").into());
    }
    if !kwargs.is_empty() {
        let keys: Vec<String> = kwargs.keys().cloned().collect();
        return Err(format!("unknown keyword arguments: {}", keys.join(", ")).into());
    }
    Ok(out)
}

/// ビルトイン関数名の一覧。`lookup_builtin` のテーブルと対応する。
/// 同名の `fn` 定義は `cannot redefine builtin: <name>` エラーになる。
const BUILTIN_NAMES: &[&str] = &[
    "puts",
    "doc",
    "col",
    "col_group",
    "array",
    "len",
    "now",
    "timestamp",
    "server_timestamp",
    "array_union",
    "array_remove",
    "delete_field",
    "increment",
    "get",
    "set",
    "update",
    "delete",
    "batch",
    "transaction",
];

type Builtin<F> = fn(
    &mut Runtime<F>,
    Vec<Value>,
    BTreeMap<String, Value>,
    Option<&Block>,
) -> Result<Value, Box<dyn Error>>;

/// ビルトインの対応テーブル。
fn builtin_entries<F: Firestore>() -> [(&'static str, Builtin<F>); 19] {
    [
        ("puts", builtin_puts),
        ("doc", builtin_doc),
        ("col", builtin_col),
        ("col_group", builtin_col_group),
        ("array", builtin_array),
        ("len", builtin_len),
        ("now", builtin_now),
        ("timestamp", builtin_timestamp),
        ("server_timestamp", builtin_server_timestamp),
        ("array_union", builtin_array_union),
        ("array_remove", builtin_array_remove),
        ("delete_field", builtin_delete_field),
        ("increment", builtin_increment),
        ("get", builtin_get),
        ("set", builtin_set),
        ("update", builtin_update),
        ("delete", builtin_delete),
        ("batch", builtin_batch),
        ("transaction", builtin_transaction),
    ]
}

/// ビルトインのテーブル引き。
fn lookup_builtin<F: Firestore>(name: &str) -> Option<Builtin<F>> {
    builtin_entries::<F>()
        .into_iter()
        .find(|(n, _)| *n == name)
        .map(|(_, f)| f)
}

fn is_builtin_name(name: &str) -> bool {
    BUILTIN_NAMES.contains(&name)
}

fn builtin_puts<F: Firestore>(
    _rt: &mut Runtime<F>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<Value, Box<dyn Error>> {
    for v in args {
        println!("{v}");
    }
    Ok(Value::Nil)
}

fn builtin_doc<F: Firestore>(
    _rt: &mut Runtime<F>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<Value, Box<dyn Error>> {
    let path = expect_string(args.first())?;
    validate_doc_path(path)?;
    Ok(Value::DocRef(DocRef {
        path: path.to_string(),
    }))
}

fn builtin_col<F: Firestore>(
    _rt: &mut Runtime<F>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<Value, Box<dyn Error>> {
    let path = expect_string(args.first())?;
    validate_collection_path(path)?;
    Ok(Value::CollectionRef(CollectionRef {
        path: path.to_string(),
    }))
}

fn builtin_col_group<F: Firestore>(
    _rt: &mut Runtime<F>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<Value, Box<dyn Error>> {
    let id = expect_string(args.first())?;
    if id.is_empty() || id.contains('/') {
        return Err(format!("invalid collection group id: {id}").into());
    }
    Ok(Value::CollectionGroupRef(CollectionGroupRef {
        id: id.to_string(),
    }))
}

fn builtin_array<F: Firestore>(
    _rt: &mut Runtime<F>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<Value, Box<dyn Error>> {
    Ok(Value::Array(args))
}

fn builtin_len<F: Firestore>(
    _rt: &mut Runtime<F>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<Value, Box<dyn Error>> {
    match args.first() {
        Some(Value::Array(a)) => Ok(Value::Int(a.len() as i64)),
        Some(Value::Map(m)) => Ok(Value::Int(m.len() as i64)),
        Some(Value::Str(s)) => Ok(Value::Int(s.chars().count() as i64)),
        _ => Err("len expects array, map, or string".into()),
    }
}

fn builtin_now<F: Firestore>(
    _rt: &mut Runtime<F>,
    _args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<Value, Box<dyn Error>> {
    Ok(Value::Timestamp(Timestamp::now()?))
}

fn builtin_timestamp<F: Firestore>(
    _rt: &mut Runtime<F>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<Value, Box<dyn Error>> {
    Ok(Value::Timestamp(Timestamp {
        seconds: parse_timestamp_arg(args.first())?,
    }))
}

fn builtin_server_timestamp<F: Firestore>(
    _rt: &mut Runtime<F>,
    _args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<Value, Box<dyn Error>> {
    Ok(Value::ServerTimestamp)
}

fn builtin_array_union<F: Firestore>(
    _rt: &mut Runtime<F>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<Value, Box<dyn Error>> {
    Ok(Value::ArrayUnion(expect_array(args.first())?.clone()))
}

fn builtin_array_remove<F: Firestore>(
    _rt: &mut Runtime<F>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<Value, Box<dyn Error>> {
    Ok(Value::ArrayRemove(expect_array(args.first())?.clone()))
}

fn builtin_delete_field<F: Firestore>(
    _rt: &mut Runtime<F>,
    _args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<Value, Box<dyn Error>> {
    Ok(Value::DeleteField)
}

fn builtin_increment<F: Firestore>(
    _rt: &mut Runtime<F>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<Value, Box<dyn Error>> {
    Ok(Value::Increment(expect_int(args.first())?))
}

fn builtin_get<F: Firestore>(
    rt: &mut Runtime<F>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<Value, Box<dyn Error>> {
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
            .fs
            .transaction_get(path)
            .map_err(backend_err)?
            .unwrap_or(Value::Nil),
        ExecCtx::Normal => rt.fs.get(path).map_err(backend_err)?.unwrap_or(Value::Nil),
    };
    rt.check_wall_clock()?;
    Ok(v)
}

fn builtin_set<F: Firestore>(
    rt: &mut Runtime<F>,
    args: Vec<Value>,
    kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<Value, Box<dyn Error>> {
    let path = expect_doc_path(args.first())?.to_string();
    let data = expect_map(args.get(1))?.clone();
    let merge = match (args.get(2), kwargs.get("merge")) {
        (_, Some(Value::Bool(v))) => *v,
        (Some(Value::Bool(v)), None) => *v,
        (None, None) => false,
        _ => return Err("set merge must be bool".into()),
    };
    rt.dispatch_write(WriteOp::Set { path, data, merge })?;
    Ok(Value::Nil)
}

fn builtin_update<F: Firestore>(
    rt: &mut Runtime<F>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<Value, Box<dyn Error>> {
    let path = expect_doc_path(args.first())?.to_string();
    let data = expect_map(args.get(1))?.clone();
    rt.dispatch_write(WriteOp::Update { path, data })?;
    Ok(Value::Nil)
}

fn builtin_delete<F: Firestore>(
    rt: &mut Runtime<F>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<Value, Box<dyn Error>> {
    let path = expect_doc_path(args.first())?.to_string();
    rt.dispatch_write(WriteOp::Delete { path })?;
    Ok(Value::Nil)
}

fn builtin_batch<F: Firestore>(
    rt: &mut Runtime<F>,
    _args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    block: Option<&Block>,
) -> Result<Value, Box<dyn Error>> {
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
    rt.fs.apply(writes).map_err(backend_err)?;
    Ok(flow.into_value())
}

fn builtin_transaction<F: Firestore>(
    rt: &mut Runtime<F>,
    _args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    block: Option<&Block>,
) -> Result<Value, Box<dyn Error>> {
    let Some(stmts) = block else {
        return Err("transaction requires { ... } block".into());
    };
    if !stmts.params.is_empty() {
        return Err("transaction block does not take parameters".into());
    }
    if !matches!(rt.exec, ExecCtx::Normal) {
        return Err("transaction cannot be nested inside batch/transaction".into());
    }
    rt.check_wall_clock()?;
    rt.fs.begin_transaction().map_err(backend_err)?;
    // `begin_transaction` は遅いことがあるため、budget 超過を begin 後に
    // 検出したら必ず abort する。さもないと server transaction が開き続け、
    // 以降の `begin_transaction` が "already in transaction" で失敗する。
    if let Err(e) = rt.check_wall_clock() {
        let _ = rt.fs.abort_transaction();
        return Err(e);
    }
    let (flow, writes) = rt.run_staged_block(ExecCtx::Transaction { writes: vec![] }, &stmts.body);
    match flow {
        Ok(flow) => {
            // commit 前の budget 超過でも transaction を開いたままにしない。
            if let Err(e) = rt.check_wall_clock() {
                let _ = rt.fs.abort_transaction();
                return Err(e);
            }
            if let Err(e) = rt.fs.commit_transaction(writes) {
                let _ = rt.fs.abort_transaction();
                return Err(backend_err(e));
            }
            // commit 後も wall-clock を再検査しない。batch と同じく、
            // 永続化済みの操作を timeout として返すと再試行が二重書き込みに
            // つながる。commit 自体の deadline は backend timeout が担保する。
            Ok(flow.into_value())
        }
        Err(e) => {
            // Rollback は best-effort: 元のブロックエラーを優先し、
            // rollback 自体の失敗は無視する(サーバ側タイムアウトが
            // 最終的な後ろ盾になる)。
            let _ = rt.fs.abort_transaction();
            Err(e)
        }
    }
}

fn parse_timestamp_arg(v: Option<&Value>) -> Result<i64, Box<dyn Error>> {
    match v {
        Some(Value::Int(i)) => Ok(*i),
        Some(Value::Str(s)) => parse_rfc3339_to_unix(s),
        _ => Err("timestamp(...) expects unix seconds int or RFC3339 string".into()),
    }
}

fn parse_rfc3339_to_unix(s: &str) -> Result<i64, Box<dyn Error>> {
    let time = s
        .split_once(['T', 't'])
        .map(|(_, time)| time)
        .ok_or("invalid RFC3339 timestamp")?;
    let time_without_zone = time
        .trim_end_matches(['Z', 'z'])
        .split(['+', '-'])
        .next()
        .unwrap_or(time);
    if time_without_zone.contains('.') {
        return Err("fractional seconds are not supported".into());
    }
    Ok(chrono::DateTime::parse_from_rfc3339(s)
        .map_err(|e| format!("invalid RFC3339 timestamp: {e}"))?
        .timestamp())
}

fn expect_string(v: Option<&Value>) -> Result<&str, Box<dyn Error>> {
    match v {
        Some(Value::Str(s)) => Ok(s),
        _ => Err("expected string argument".into()),
    }
}

fn expect_int(v: Option<&Value>) -> Result<i64, Box<dyn Error>> {
    match v {
        Some(Value::Int(i)) => Ok(*i),
        _ => Err("expected int argument".into()),
    }
}

fn expect_map(v: Option<&Value>) -> Result<&BTreeMap<String, Value>, Box<dyn Error>> {
    match v {
        Some(Value::Map(m)) => Ok(m),
        _ => Err("expected map argument".into()),
    }
}

fn expect_array(v: Option<&Value>) -> Result<&Vec<Value>, Box<dyn Error>> {
    match v {
        Some(Value::Array(a)) => Ok(a),
        _ => Err("expected array argument".into()),
    }
}

fn expect_doc_path(v: Option<&Value>) -> Result<&str, Box<dyn Error>> {
    match v {
        Some(Value::Str(s)) => {
            validate_doc_path(s)?;
            Ok(s)
        }
        Some(Value::DocRef(d)) => Ok(&d.path),
        _ => Err("expected document path string or doc object".into()),
    }
}

/// ドキュメントパスは偶数個・コレクションパスは奇数個の非空セグメントで
/// 構成されていなければならない(`users/1`、`teams/t1/members`)。
/// runtime 層で検証するため、in-memory / emulator のどちらのバックエンド
/// でも同じエラーになる。
fn validate_doc_path(path: &str) -> Result<(), Box<dyn Error>> {
    validate_path(path, false)
}

fn validate_collection_path(path: &str) -> Result<(), Box<dyn Error>> {
    validate_path(path, true)
}

fn validate_path(path: &str, collection: bool) -> Result<(), Box<dyn Error>> {
    let segments: Vec<&str> = path.split('/').collect();
    if segments.iter().any(|s| s.is_empty()) {
        return Err(format!("invalid path: {path} (empty path segment)").into());
    }
    let parity_ok = segments.len() % 2 == if collection { 1 } else { 0 };
    if !parity_ok {
        let kind = if collection { "collection" } else { "document" };
        let expected = if collection { "odd" } else { "even" };
        return Err(format!(
            "invalid {kind} path: {path} (expected an {expected} number of segments)"
        )
        .into());
    }
    Ok(())
}

fn check_block_arity(block: &Block, yielded: usize) -> Result<(), Box<dyn Error>> {
    if block.params.len() != yielded {
        return Err(format!(
            "block expects {} argument(s), got {}",
            block.params.len(),
            yielded
        )
        .into());
    }
    Ok(())
}

/// バックエンドが返した候補(相対パス・フィールド)に `__name__` を付け、
/// 順序・offset・limit を適用する。両バックエンド共通の決定性はここで
/// 保証する(spec §7)。`order_by` 対象を持たないドキュメントは除外する。
fn apply_query_shape(
    mut rows: Vec<(String, BTreeMap<String, Value>)>,
    query: &QueryRef,
) -> Vec<Value> {
    if query.order_by.is_empty() {
        rows.sort_by(|a, b| a.0.cmp(&b.0));
    } else {
        rows.retain(|(path, doc)| {
            query
                .order_by
                .iter()
                .all(|(f, _)| get_field_value(path, doc, f).is_some())
        });
        rows.sort_by(|a, b| {
            for (field, dir) in &query.order_by {
                let val_a = get_field_value(&a.0, &a.1, field).unwrap();
                let val_b = get_field_value(&b.0, &b.1, field).unwrap();
                let ord = val_a.cmp_for_order(&val_b);
                let ord = match dir {
                    Direction::Asc => ord,
                    Direction::Desc => ord.reverse(),
                };
                if ord != std::cmp::Ordering::Equal {
                    return ord;
                }
            }
            a.0.cmp(&b.0)
        });
    }
    let skip = query.offset.unwrap_or(0).min(rows.len());
    let mut values: Vec<Value> = rows
        .into_iter()
        .skip(skip)
        .map(|(path, doc)| wrap_doc(&path, &doc))
        .collect();
    if let Some(l) = query.limit {
        values.truncate(l.min(values.len()));
    }
    values
}

fn wrap_doc(path: &str, doc: &BTreeMap<String, Value>) -> Value {
    let mut out = doc.clone();
    out.insert("__name__".to_string(), Value::Str(path.to_string()));
    Value::Map(out)
}

fn get_field_value(path: &str, doc: &BTreeMap<String, Value>, field: &str) -> Option<Value> {
    if field == "__name__" {
        Some(Value::Str(path.to_string()))
    } else {
        let segments: Vec<&str> = field.split('.').collect();
        lookup_field_path(Some(doc), &segments).cloned()
    }
}

fn lookup_field_path<'a>(
    map: Option<&'a BTreeMap<String, Value>>,
    segments: &[&str],
) -> Option<&'a Value> {
    let mut cur = map?;
    for (i, seg) in segments.iter().enumerate() {
        let v = cur.get(*seg)?;
        if i == segments.len() - 1 {
            return Some(v);
        }
        match v {
            Value::Map(m) => cur = m,
            _ => return None,
        }
    }
    None
}

fn eval_index(base: Value, index: Value) -> Result<Value, Box<dyn Error>> {
    match (base, index) {
        (Value::Map(map), Value::Str(key)) => Ok(map.get(&key).cloned().unwrap_or(Value::Nil)),
        (Value::Array(items), Value::Int(i)) => {
            if i < 0 {
                return Ok(Value::Nil);
            }
            Ok(items.get(i as usize).cloned().unwrap_or(Value::Nil))
        }
        (Value::Map(_), _) => Err("map index must be a string".into()),
        (Value::Array(_), _) => Err("array index must be an int".into()),
        _ => Err("unsupported index receiver".into()),
    }
}

fn eval_binary(l: Value, op: BinOp, r: Value) -> Result<Value, Box<dyn Error>> {
    Ok(match op {
        BinOp::Add => match (l, r) {
            (Value::Int(a), Value::Int(b)) => int_binop(a, b, |a, b| a.checked_add(b))?,
            (Value::Str(a), Value::Str(b)) => Value::Str(format!("{a}{b}")),
            _ => return Err("unsupported + operands".into()),
        },
        BinOp::Sub => match (l, r) {
            (Value::Int(a), Value::Int(b)) => int_binop(a, b, |a, b| a.checked_sub(b))?,
            (Value::Timestamp(a), Value::Timestamp(b)) => {
                int_binop(a.seconds, b.seconds, |a, b| a.checked_sub(b))?
            }
            _ => return Err("unsupported - operands".into()),
        },
        BinOp::Mul => match (l, r) {
            (Value::Int(a), Value::Int(b)) => int_binop(a, b, |a, b| a.checked_mul(b))?,
            _ => return Err("unsupported * operands".into()),
        },
        BinOp::Div => match (l, r) {
            (Value::Int(a), Value::Int(b)) => {
                if b == 0 {
                    return Err("division by zero".into());
                }
                int_binop(a, b, |a, b| a.checked_div(b))?
            }
            _ => return Err("unsupported / operands".into()),
        },
        BinOp::Eq => Value::Bool(l == r),
        BinOp::Ne => Value::Bool(l != r),
        BinOp::Gt => match (l, r) {
            (Value::Int(a), Value::Int(b)) => Value::Bool(a > b),
            (Value::Timestamp(a), Value::Timestamp(b)) => Value::Bool(a.seconds > b.seconds),
            _ => return Err("unsupported > operands".into()),
        },
        BinOp::Lt => match (l, r) {
            (Value::Int(a), Value::Int(b)) => Value::Bool(a < b),
            (Value::Timestamp(a), Value::Timestamp(b)) => Value::Bool(a.seconds < b.seconds),
            _ => return Err("unsupported < operands".into()),
        },
        BinOp::Ge => match (l, r) {
            (Value::Int(a), Value::Int(b)) => Value::Bool(a >= b),
            (Value::Timestamp(a), Value::Timestamp(b)) => Value::Bool(a.seconds >= b.seconds),
            _ => return Err("unsupported >= operands".into()),
        },
        BinOp::Le => match (l, r) {
            (Value::Int(a), Value::Int(b)) => Value::Bool(a <= b),
            (Value::Timestamp(a), Value::Timestamp(b)) => Value::Bool(a.seconds <= b.seconds),
            _ => return Err("unsupported <= operands".into()),
        },
        // `eval_expr` 側で短絡評価するため、ここに来るのは両辺評価済みの場合のみ。
        // Ruby と同じくオペランドそのものを返す。
        BinOp::And => {
            if !l.truthy() {
                l
            } else {
                r
            }
        }
        BinOp::Or => {
            if l.truthy() {
                l
            } else {
                r
            }
        }
    })
}

fn int_binop(a: i64, b: i64, f: impl Fn(i64, i64) -> Option<i64>) -> Result<Value, Box<dyn Error>> {
    f(a, b)
        .map(Value::Int)
        .ok_or_else(|| "integer overflow".into())
}

fn backend_err(e: Box<dyn Error>) -> Box<dyn Error> {
    if e.downcast_ref::<FirelangError>().is_some() {
        e
    } else {
        FirelangError::backend(e.to_string()).into()
    }
}

/// eval 失敗時にトップレベル文の位置を付与する。既に span を持つ
/// FirelangError (lex/parse/backendで付与済み)はそのまま通し、
/// span なしの FirelangError と String 系エラーは Runtime として包む。
fn attach_span(e: Box<dyn Error>, span: Option<Span>) -> Box<dyn Error> {
    let Some(span) = span else {
        return wrap_as_runtime(e);
    };
    match e.downcast::<FirelangError>() {
        Ok(mut ferr) => {
            if ferr.span.is_none() {
                ferr.span = Some(span);
            }
            // `Box<FirelangError>` -> `Box<dyn Error>` は coerce する。
            // `.into()` は `Box<FirelangError>` 自体を `Error` として
            // 二重に Box 化してしまい downcast が効かなくなるため使わない。
            ferr as Box<dyn Error>
        }
        Err(e) => {
            // String 等の非 typed エラーは Runtime として包む。
            FirelangError::runtime(e.to_string()).with_span(span).into()
        }
    }
}

fn wrap_as_runtime(e: Box<dyn Error>) -> Box<dyn Error> {
    if e.downcast_ref::<FirelangError>().is_some() {
        e
    } else {
        FirelangError::runtime(e.to_string()).into()
    }
}

enum ExecFlow {
    Value(Value),
    Return(Value),
}

impl ExecFlow {
    fn into_value(self) -> Value {
        match self {
            ExecFlow::Value(v) | ExecFlow::Return(v) => v,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::firestore::InMemoryFirestore;

    #[test]
    fn execution_state_is_cleared_after_run() {
        // 正常終了後に再入 guard が残っていると、2 回目以降の run が
        // すべて「already executing」で失敗してしまう。
        let mut rt = Runtime::new(InMemoryFirestore::default());
        rt.run("1 + 1").unwrap();
        assert!(!rt.execution_active);
        rt.run("2 + 2").unwrap();
    }

    #[test]
    fn execution_state_is_cleared_after_a_failed_run() {
        let mut rt = Runtime::new(InMemoryFirestore::default());
        assert!(rt.run("(").is_err());
        assert!(!rt.execution_active);
        rt.run("1").unwrap();
    }

    #[test]
    fn reentrant_execution_is_rejected() {
        // 再入を許すと start_execution が実行中の usage をリセットし、
        // step / wall-clock の budget が実質無効になる。
        let mut rt = Runtime::new(InMemoryFirestore::default());
        rt.execution_active = true;
        let err = rt.run("1").unwrap_err();
        assert!(format!("{err}").contains("already executing"), "{err}");
    }

    #[test]
    fn builtin_table_matches_name_list() {
        // テーブルと名前一覧の対応がずれると、同名 fn 定義の検査が抜ける。
        let mut names = BUILTIN_NAMES.to_vec();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), BUILTIN_NAMES.len(), "duplicate builtin names");
        for (name, _) in builtin_entries::<InMemoryFirestore>() {
            assert!(
                is_builtin_name(name),
                "{name} is implemented but missing from BUILTIN_NAMES"
            );
        }
        for name in BUILTIN_NAMES {
            assert!(
                lookup_builtin::<InMemoryFirestore>(name).is_some(),
                "{name} is listed but has no implementation"
            );
            assert!(is_builtin_name(name));
        }
        assert!(!is_builtin_name("undefined_fn"));
        assert!(lookup_builtin::<InMemoryFirestore>("undefined_fn").is_none());
    }

    struct BoundedQueryFirestore;

    impl Firestore for BoundedQueryFirestore {
        fn get(&self, _path: &str) -> Result<Option<Value>, Box<dyn Error>> {
            Ok(None)
        }

        fn apply(&mut self, _writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>> {
            Ok(())
        }

        fn run_query(
            &self,
            _query: &QueryRef,
        ) -> Result<crate::firestore::QueryCandidates, Box<dyn Error>> {
            panic!("runtime must use the bounded query path")
        }

        fn run_query_bounded(
            &self,
            _query: &QueryRef,
            _max_results: usize,
        ) -> Result<crate::firestore::QueryCandidates, Box<dyn Error>> {
            Ok(vec![])
        }

        fn begin_transaction(&mut self) -> Result<(), Box<dyn Error>> {
            Ok(())
        }

        fn transaction_get(&self, _path: &str) -> Result<Option<Value>, Box<dyn Error>> {
            Ok(None)
        }

        fn commit_transaction(&mut self, _writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>> {
            Ok(())
        }

        fn abort_transaction(&mut self) -> Result<(), Box<dyn Error>> {
            Ok(())
        }
    }

    struct FailingCommitFirestore {
        in_tx: bool,
        aborted: bool,
        timeout: Option<Duration>,
    }

    impl Firestore for FailingCommitFirestore {
        fn get(&self, _path: &str) -> Result<Option<Value>, Box<dyn Error>> {
            Ok(None)
        }
        fn apply(&mut self, _writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>> {
            Ok(())
        }
        fn run_query(
            &self,
            _query: &QueryRef,
        ) -> Result<crate::firestore::QueryCandidates, Box<dyn Error>> {
            Ok(vec![])
        }
        fn run_query_bounded(
            &self,
            _query: &QueryRef,
            _max_results: usize,
        ) -> Result<crate::firestore::QueryCandidates, Box<dyn Error>> {
            Ok(vec![])
        }
        fn set_timeout(&mut self, timeout: Option<Duration>) {
            self.timeout = timeout;
        }
        fn supports_backend_timeout(&self) -> bool {
            true
        }
        fn begin_transaction(&mut self) -> Result<(), Box<dyn Error>> {
            if self.in_tx {
                return Err("already in transaction".into());
            }
            self.in_tx = true;
            Ok(())
        }
        fn transaction_get(&self, _path: &str) -> Result<Option<Value>, Box<dyn Error>> {
            Ok(None)
        }
        fn commit_transaction(&mut self, _writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>> {
            Err("commit failed".into())
        }
        fn abort_transaction(&mut self) -> Result<(), Box<dyn Error>> {
            self.in_tx = false;
            self.aborted = true;
            Ok(())
        }
    }

    #[test]
    fn runtime_uses_bounded_query_hook_before_unbounded_query() {
        let mut runtime = Runtime::new(BoundedQueryFirestore);
        let result = runtime.run("col(\"users\").get()");
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn transaction_aborts_on_commit_failure() {
        let fs = FailingCommitFirestore {
            in_tx: false,
            aborted: false,
            timeout: None,
        };
        let mut runtime = Runtime::new(fs);
        let res = runtime.eval_program(&[Stmt::Expr(Expr::Call {
            receiver: None,
            name: "transaction".into(),
            args: vec![],
            kwargs: vec![],
            block: Some(Block {
                params: vec![],
                body: vec![],
            }),
        })]);
        assert!(res.is_err());
        assert!(runtime.fs.aborted);
        assert!(!runtime.fs.in_tx);
    }

    #[test]
    fn runtime_propagates_and_updates_backend_timeout() {
        let limits = RuntimeLimits::default().with_backend_timeout(Duration::from_millis(37));
        let mut runtime = Runtime::with_limits(
            FailingCommitFirestore {
                in_tx: false,
                aborted: false,
                timeout: None,
            },
            limits,
        );
        assert_eq!(runtime.fs.timeout, Some(Duration::from_millis(37)));
        assert!(runtime.backend_timeout_supported());

        runtime.set_limits(RuntimeLimits::unlimited());
        assert_eq!(runtime.fs.timeout, None);
    }
}
