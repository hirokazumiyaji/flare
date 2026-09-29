use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::error::Error;

use crate::ast::{BinOp, Block, Expr, Stmt, StrPiece, UnOp};
use crate::error::{FirelangError, Span};
use crate::firestore::{Firestore, WriteOp};
use crate::value::{
    CollectionGroupRef, CollectionRef, Direction, DocRef, QueryFilter, QueryOp, QueryRef,
    Timestamp, UserFunc, Value,
};

enum ExecCtx {
    Normal,
    Batch { writes: Vec<WriteOp> },
    Transaction { writes: Vec<WriteOp> },
}

pub struct Runtime<F: Firestore> {
    scopes: Vec<HashMap<String, Value>>,
    fs: F,
    exec: ExecCtx,
    /// Set when a best-effort rollback failed, so the backend still holds the
    /// transaction open. The next `transaction` block retries the rollback
    /// before beginning a new one; otherwise a single failed Rollback RPC
    /// would make every later `transaction` fail with "already in
    /// transaction" and nothing in a script could clear the state.
    pending_rollback: bool,
}

impl<F: Firestore> Runtime<F> {
    pub fn new(fs: F) -> Self {
        Self {
            scopes: vec![HashMap::new()],
            fs,
            exec: ExecCtx::Normal,
            pending_rollback: false,
        }
    }

    pub fn eval_program(&mut self, stmts: &[Stmt]) -> Result<Value, Box<dyn Error>> {
        self.eval_block(stmts).map(ExecFlow::into_value)
    }

    /// トップレベル文の開始位置付きで評価し、失敗した文の位置を
    /// runtime/backend エラーに付与する。既に span を持つ lex/parse エラーは
    /// そのまま通す。
    pub fn eval_program_with_spans(
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
        let spanned = crate::lexer::lex_spanned(source)?;
        let mut parser = crate::parser::Parser::from_spanned(spanned);
        let (program, spans) = parser.parse_program_with_spans()?;
        self.eval_program_with_spans(&program, &spans)
    }

    fn eval_stmt(&mut self, stmt: &Stmt) -> Result<ExecFlow, Box<dyn Error>> {
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
                    let cond = match self.eval_expr(cond)? {
                        ExecFlow::Value(v) => v,
                        flow @ ExecFlow::Return(_) => return Ok(flow),
                    };
                    if !cond.truthy() {
                        break;
                    }
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

    /// `exec` をステージング用 ctx に差し替え、子スコープでブロックを評価する。
    /// ブロックの成否にかかわらず元の ctx とスコープへ戻し、ステージされた
    /// writes を返す。
    fn run_staged_block(
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

    fn forbid_query_in_batch_or_transaction(&self) -> Result<(), Box<dyn Error>> {
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
    fn abort_transaction_best_effort(&mut self) {
        if self.fs.abort_transaction().is_err() {
            // The backend kept the transaction open. Remember it: a script
            // has no other way to reach the backend and clear the state.
            self.pending_rollback = true;
        }
    }

    /// Retry a rollback that previously failed. Called before every
    /// `begin_transaction`. If the retry also fails, `begin_transaction`
    /// surfaces its own "already in transaction" error, which is at least an
    /// honest report of a backend that is still busy.
    fn retry_pending_rollback(&mut self) {
        if !self.pending_rollback {
            return;
        }
        if self.fs.abort_transaction().is_ok() {
            self.pending_rollback = false;
        }
    }

    fn dispatch_write(&mut self, op: WriteOp) -> Result<(), Box<dyn Error>> {
        match &mut self.exec {
            ExecCtx::Normal => self.fs.apply(vec![op]).map_err(backend_err),
            ExecCtx::Batch { writes } | ExecCtx::Transaction { writes } => {
                writes.push(op);
                Ok(())
            }
        }
    }

    fn eval_expr(&mut self, expr: &Expr) -> Result<ExecFlow, Box<dyn Error>> {
        match expr {
            Expr::Int(i) => Ok(ExecFlow::Value(Value::Int(*i))),
            Expr::Str(s) => Ok(ExecFlow::Value(Value::Str(s.clone()))),
            Expr::StrInterp(pieces) => {
                let mut out = String::new();
                for piece in pieces {
                    match piece {
                        StrPiece::Lit(s) => out.push_str(s),
                        StrPiece::Expr(e) => {
                            let v = match self.eval_expr(e)? {
                                ExecFlow::Value(v) => v,
                                flow @ ExecFlow::Return(_) => return Ok(flow),
                            };
                            out.push_str(&v.to_string());
                        }
                    }
                }
                Ok(ExecFlow::Value(Value::Str(out)))
            }
            Expr::Bool(b) => Ok(ExecFlow::Value(Value::Bool(*b))),
            Expr::Nil => Ok(ExecFlow::Value(Value::Nil)),
            Expr::Var(name) => self
                .get_var(name)
                .cloned()
                .map(ExecFlow::Value)
                .ok_or_else(|| format!("undefined variable: {name}").into()),
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
            Expr::Call {
                receiver,
                name,
                args,
                kwargs,
                block,
            } => {
                // Validate the call shape before evaluating argument
                // expressions.  Invalid calls must not be able to perform
                // writes (or other side effects) before their contract error
                // is reported.  Keyword values are irrelevant for this
                // pre-check; only their names and arity are needed.
                validate_unique_keywords(kwargs)?;
                if let Some(receiver_expr) = receiver {
                    // Keyword arguments are not part of any method contract.
                    // Reject them before evaluating the receiver, which may
                    // itself contain a staged or immediate write.
                    if !kwargs.is_empty() {
                        return Err(unknown_keyword_names(kwargs));
                    }
                    if let Some(block) = block.as_ref() {
                        if name == "each" || name == "map" {
                            check_block_arity(block, 1)?;
                        } else {
                            return Err(format!("method {name} does not take a block").into());
                        }
                    }
                    let receiver_value = match self.eval_expr(receiver_expr)? {
                        ExecFlow::Value(v) => v,
                        flow @ ExecFlow::Return(_) => return Ok(flow),
                    };
                    let keyword_names = keyword_names(kwargs);
                    validate_method_call(
                        &receiver_value,
                        name,
                        args.len(),
                        &keyword_names,
                        block.as_ref(),
                    )?;
                    let mut argv = vec![];
                    for arg in args {
                        match self.eval_expr(arg)? {
                            ExecFlow::Value(v) => argv.push(v),
                            flow @ ExecFlow::Return(_) => return Ok(flow),
                        }
                    }
                    let mut kw = BTreeMap::new();
                    for (k, vexpr) in kwargs {
                        match self.eval_expr(vexpr)? {
                            ExecFlow::Value(v) => {
                                kw.insert(k.clone(), v);
                            }
                            flow @ ExecFlow::Return(_) => return Ok(flow),
                        }
                    }
                    return self.eval_method_call_unchecked(
                        receiver_value,
                        name,
                        argv,
                        kw,
                        block.as_ref(),
                    );
                }

                if lookup_builtin::<F>(name).is_some() {
                    let keyword_names = keyword_names(kwargs);
                    validate_call_contract(
                        name,
                        args.len(),
                        &keyword_names,
                        block.as_ref(),
                        builtin_contract(name),
                    )?;
                } else {
                    let Some(Value::Func(func)) = self.get_var(name).cloned() else {
                        return Err(format!("undefined function: {name}").into());
                    };
                    validate_user_call_contract(
                        name,
                        &func.params,
                        args.len(),
                        kwargs,
                        block.as_ref(),
                    )?;
                }

                let mut argv = vec![];
                for arg in args {
                    match self.eval_expr(arg)? {
                        ExecFlow::Value(v) => argv.push(v),
                        flow @ ExecFlow::Return(_) => return Ok(flow),
                    }
                }
                let mut kw = BTreeMap::new();
                for (k, vexpr) in kwargs {
                    match self.eval_expr(vexpr)? {
                        ExecFlow::Value(v) => {
                            kw.insert(k.clone(), v);
                        }
                        flow @ ExecFlow::Return(_) => return Ok(flow),
                    }
                }
                self.eval_call(None, name, argv, kw, block.as_ref())
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
        }
    }

    fn eval_call(
        &mut self,
        receiver: Option<&Expr>,
        name: &str,
        args: Vec<Value>,
        kwargs: BTreeMap<String, Value>,
        block: Option<&Block>,
    ) -> Result<ExecFlow, Box<dyn Error>> {
        if let Some(r) = receiver {
            let rec = match self.eval_expr(r)? {
                ExecFlow::Value(v) => v,
                flow @ ExecFlow::Return(_) => return Ok(flow),
            };
            return self.eval_method_call(rec, name, args, kwargs, block);
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

    fn eval_method_call(
        &mut self,
        receiver: Value,
        name: &str,
        args: Vec<Value>,
        kwargs: BTreeMap<String, Value>,
        block: Option<&Block>,
    ) -> Result<ExecFlow, Box<dyn Error>> {
        validate_method_call(&receiver, name, args.len(), &kwargs, block)?;
        self.eval_method_call_unchecked(receiver, name, args, kwargs, block)
    }

    fn eval_method_call_unchecked(
        &mut self,
        receiver: Value,
        name: &str,
        args: Vec<Value>,
        _kwargs: BTreeMap<String, Value>,
        block: Option<&Block>,
    ) -> Result<ExecFlow, Box<dyn Error>> {
        match receiver {
            Value::CollectionRef(collection) if name == "doc" => {
                let id = expect_string(args.first())?;
                if id.is_empty() || id.contains('/') {
                    return Err(format!("invalid document id: {id}").into());
                }
                Ok(ExecFlow::Value(Value::DocRef(DocRef {
                    path: format!("{}/{}", collection.path, id),
                })))
            }
            Value::CollectionRef(collection) => self
                .eval_query_method(collection.to_query(), name, args)
                .map(ExecFlow::Value),
            Value::CollectionGroupRef(group) => self
                .eval_query_method(group.to_query(), name, args)
                .map(ExecFlow::Value),
            Value::QueryRef(query) => self
                .eval_query_method(query, name, args)
                .map(ExecFlow::Value),
            Value::Array(items) => self.eval_array_method(items, name, args, block),
            Value::Timestamp(ts) => match name {
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
            Value::Map(map) => {
                // Map レシーバは `m.key` 形式のキーアクセス(Ruby の OpenStruct 風)のみ。
                // 引数を取るメソッドは持たないので、Args がある場合はエラーにする。
                if !args.is_empty() {
                    return Err(format!("unknown map method: {name}").into());
                }
                Ok(ExecFlow::Value(
                    map.get(name).cloned().unwrap_or(Value::Nil),
                ))
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

    fn call_block(&mut self, block: &Block, args: Vec<Value>) -> Result<ExecFlow, Box<dyn Error>> {
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
                Ok(Value::Int(
                    self.fs.run_query(&unpaged).map_err(backend_err)?.len() as i64,
                ))
            }
            "get" => {
                self.forbid_query_in_batch_or_transaction()?;
                let rows = self.fs.run_query(&query).map_err(backend_err)?;
                Ok(Value::Array(apply_query_shape(rows, &query)))
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

fn validate_unique_keywords(kwargs: &[(String, Expr)]) -> Result<(), Box<dyn Error>> {
    let mut seen = BTreeSet::<&str>::new();
    for (key, _) in kwargs {
        if !seen.insert(key.as_str()) {
            return Err(format!("duplicate keyword argument: {key}").into());
        }
    }
    Ok(())
}

/// Scope cleanup guard for function and block calls.
///
/// A function temporarily hides all caller-local scopes while keeping the
/// global scope visible. A block only adds one frame. In both cases the
/// original scope stack is restored on every exit path, including `?` returns
/// and panic unwinding.
struct ScopeGuard<'a, F: Firestore> {
    runtime: &'a mut Runtime<F>,
    // `Some` is a function frame and contains the caller-local suffix. `None`
    // is a block frame, which is removed from the end on drop.
    saved: Option<Vec<HashMap<String, Value>>>,
}

impl<'a, F: Firestore> ScopeGuard<'a, F> {
    fn function(runtime: &'a mut Runtime<F>) -> Self {
        let saved = runtime.scopes.split_off(1);
        runtime.scopes.push(HashMap::new());
        Self {
            runtime,
            saved: Some(saved),
        }
    }

    fn block(runtime: &'a mut Runtime<F>) -> Self {
        runtime.scopes.push(HashMap::new());
        Self {
            runtime,
            saved: None,
        }
    }
}

impl<F: Firestore> Drop for ScopeGuard<'_, F> {
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

fn validate_unique_params(params: &[String], kind: &str) -> Result<(), Box<dyn Error>> {
    let mut seen = HashSet::with_capacity(params.len());
    for param in params {
        if !seen.insert(param.as_str()) {
            return Err(format!("duplicate {kind} parameter: {param}").into());
        }
    }
    Ok(())
}

fn keyword_names(kwargs: &[(String, Expr)]) -> BTreeMap<String, Value> {
    kwargs
        .iter()
        .map(|(key, _)| (key.clone(), Value::Nil))
        .collect()
}

fn unknown_keyword_names(kwargs: &[(String, Expr)]) -> Box<dyn Error> {
    let keys: Vec<&str> = kwargs.iter().map(|(key, _)| key.as_str()).collect();
    format!("unknown keyword arguments: {}", keys.join(", ")).into()
}

fn validate_user_call_contract(
    name: &str,
    params: &[String],
    positional_len: usize,
    kwargs: &[(String, Expr)],
    block: Option<&Block>,
) -> Result<(), Box<dyn Error>> {
    if block.is_some() {
        return Err(format!("{name} does not take a block (block is not allowed)").into());
    }
    bind_function_args(
        params,
        vec![Value::Nil; positional_len],
        keyword_names(kwargs),
    )
    .map(|_| ())
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
            if kwargs.contains_key(param) {
                return Err(format!(
                    "duplicate argument {param}: specified both positionally and as a keyword"
                )
                .into());
            }
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

/// `bytes(...)` の引数エラー文言。3 箇所から使うので定数にまとめる。
const BYTES_USAGE: &str = "bytes expects one string or an array of integers from 0 to 255";
const BYTES_ELEMENT_ERR: &str = "bytes array elements must be integers from 0 to 255";

/// The positional part of a call contract.
#[derive(Clone, Copy)]
enum PositionalArity {
    Exact(usize),
    Range(usize, usize),
    Any,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockPolicy {
    Forbidden,
    Required,
}

/// Runtime validation shared by builtin functions and built-in methods.
///
/// `keywords` is intentionally small: the language's builtin API is
/// positional except for `set(..., merge: true)`.  `array` and `puts` use
/// `PositionalArity::Any` because their variable-arity behavior is part of
/// the API.
#[derive(Clone, Copy)]
struct CallContract {
    positional: PositionalArity,
    keywords: &'static [&'static str],
    block: BlockPolicy,
    /// Keywords that occupy the same slot as a positional argument.
    positional_keyword_conflicts: &'static [(&'static str, usize)],
    /// Optional compatibility/detail hint for a missing argument.
    missing_hint: Option<&'static str>,
}

impl CallContract {
    fn exact(positional: usize) -> Self {
        Self {
            positional: PositionalArity::Exact(positional),
            keywords: &[],
            block: BlockPolicy::Forbidden,
            positional_keyword_conflicts: &[],
            missing_hint: None,
        }
    }

    fn range(min: usize, max: usize) -> Self {
        Self {
            positional: PositionalArity::Range(min, max),
            ..Self::exact(0)
        }
    }

    fn any() -> Self {
        Self {
            positional: PositionalArity::Any,
            ..Self::exact(0)
        }
    }

    fn with_keywords(mut self, keywords: &'static [&'static str]) -> Self {
        self.keywords = keywords;
        self
    }

    fn with_positional_keyword_conflicts(
        mut self,
        conflicts: &'static [(&'static str, usize)],
    ) -> Self {
        self.positional_keyword_conflicts = conflicts;
        self
    }

    fn with_missing_hint(mut self, hint: &'static str) -> Self {
        self.missing_hint = Some(hint);
        self
    }

    fn requiring_block(mut self) -> Self {
        self.block = BlockPolicy::Required;
        self
    }
}

fn validate_call_contract(
    name: &str,
    positional_len: usize,
    kwargs: &BTreeMap<String, Value>,
    block: Option<&Block>,
    contract: CallContract,
) -> Result<(), Box<dyn Error>> {
    // Check this before arity/type checks so a call which supplies the same
    // option in two ways always gets the actionable duplicate-argument error.
    for (keyword, position) in contract.positional_keyword_conflicts {
        if positional_len > *position && kwargs.contains_key(*keyword) {
            return Err(format!(
                "duplicate argument {keyword}: specified both positionally and as a keyword"
            )
            .into());
        }
    }

    match contract.positional {
        PositionalArity::Any => {}
        PositionalArity::Exact(expected) => {
            if positional_len < expected {
                let hint = contract
                    .missing_hint
                    .map(|hint| format!(" ({hint})"))
                    .unwrap_or_default();
                return Err(format!(
                    "{name} is missing positional arguments: expected {expected}, got {positional_len}{hint}"
                )
                .into());
            }
            if positional_len > expected {
                return Err(too_many_positional_error(
                    name,
                    expected,
                    positional_len,
                    true,
                ));
            }
        }
        PositionalArity::Range(min, max) => {
            if positional_len < min {
                return Err(format!(
                    "{name} expects at least {min} positional arguments, got {positional_len}"
                )
                .into());
            }
            if positional_len > max {
                return Err(too_many_positional_error(name, max, positional_len, false));
            }
        }
    }

    let unknown: Vec<&str> = kwargs
        .keys()
        .filter(|key| {
            !contract
                .keywords
                .iter()
                .any(|allowed| allowed == &key.as_str())
        })
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        return Err(format!("unknown keyword arguments: {}", unknown.join(", ")).into());
    }

    match (contract.block, block) {
        (BlockPolicy::Forbidden, Some(_)) => {
            return Err(format!("{name} does not take a block (block is not allowed)").into());
        }
        (BlockPolicy::Required, None) => {
            return Err(format!("{name} requires {{ ... }} block").into());
        }
        _ => {}
    }

    Ok(())
}

fn too_many_positional_error(
    name: &str,
    expected: usize,
    actual: usize,
    exact: bool,
) -> Box<dyn Error> {
    if expected == 0 {
        // Keep the long-standing array-method wording while making the
        // contract failure explicit for new callers and diagnostics.
        format!(
            "{name} does not take arguments: too many positional arguments (expected 0, got {actual})"
        )
        .into()
    } else {
        let expectation = if exact { "exactly" } else { "at most" };
        format!(
            "too many positional arguments for {name} (expected {expectation} {expected}, got {actual})"
        )
        .into()
    }
}

fn unknown_keyword_arguments(kwargs: &BTreeMap<String, Value>) -> Box<dyn Error> {
    let keys: Vec<&str> = kwargs.keys().map(String::as_str).collect();
    format!("unknown keyword arguments: {}", keys.join(", ")).into()
}

/// Contracts for all global builtins.  This is intentionally kept separate
/// from the dispatch functions so the callable API is visible in one table.
fn builtin_contract(name: &str) -> CallContract {
    match name {
        "puts" | "array" => CallContract::any(),
        "doc" | "col" | "col_group" | "len" | "timestamp" | "bytes" | "array_union"
        | "array_remove" | "get" | "delete" => CallContract::exact(1),
        "increment" => CallContract::exact(1).with_missing_hint("expected int argument"),
        "update" => CallContract::exact(2),
        "now" | "server_timestamp" | "delete_field" => CallContract::exact(0),
        "set" => CallContract::range(2, 3)
            .with_keywords(&["merge"])
            .with_positional_keyword_conflicts(&[("merge", 2)]),
        "batch" | "transaction" => CallContract::exact(0).requiring_block(),
        _ => unreachable!("unknown builtin contract: {name}"),
    }
}

fn query_method_contract(name: &str) -> Option<CallContract> {
    match name {
        "where" => Some(CallContract::exact(3)),
        "order_by" => Some(CallContract::exact(2)),
        "limit" | "offset" => Some(CallContract::exact(1)),
        "count" | "get" => Some(CallContract::exact(0)),
        _ => None,
    }
}

fn array_method_contract(name: &str) -> Option<CallContract> {
    match name {
        "each" => Some(CallContract::exact(0).requiring_block()),
        "map" => Some(CallContract::exact(0).requiring_block()),
        "len" => Some(CallContract::exact(0)),
        _ => None,
    }
}

fn timestamp_method_contract(name: &str) -> Option<CallContract> {
    match name {
        "to_unix" | "to_rfc3339" => Some(CallContract::exact(0)),
        "add_seconds" => Some(CallContract::exact(1)),
        _ => None,
    }
}

fn method_contract(receiver: &Value, name: &str) -> Option<CallContract> {
    match receiver {
        Value::CollectionRef(_) if name == "doc" => Some(CallContract::exact(1)),
        Value::CollectionRef(_) | Value::CollectionGroupRef(_) | Value::QueryRef(_) => {
            query_method_contract(name)
        }
        Value::Array(_) => array_method_contract(name),
        Value::Timestamp(_) => timestamp_method_contract(name),
        // `m.key` is map key access rather than a fixed set of methods.  It
        // still has a strict zero-argument/no-block contract.
        Value::Map(_) => Some(CallContract::exact(0)),
        _ => None,
    }
}

fn method_call_label(receiver: &Value, name: &str) -> String {
    match receiver {
        Value::CollectionRef(_) if name == "doc" => format!("collection method {name}"),
        Value::CollectionRef(_) | Value::CollectionGroupRef(_) | Value::QueryRef(_) => {
            format!("query method {name}")
        }
        Value::Array(_) => format!("array method {name}"),
        Value::Timestamp(_) => format!("timestamp method {name}"),
        Value::Map(_) => format!("map key access {name}"),
        _ => format!("method {name}"),
    }
}

fn validate_method_call(
    receiver: &Value,
    name: &str,
    positional_len: usize,
    kwargs: &BTreeMap<String, Value>,
    block: Option<&Block>,
) -> Result<(), Box<dyn Error>> {
    if let Some(contract) = method_contract(receiver, name) {
        return validate_call_contract(
            &method_call_label(receiver, name),
            positional_len,
            kwargs,
            block,
            contract,
        );
    }

    // Even an unknown method must not silently accept keyword arguments or a
    // block.  The eventual dispatch error (unknown method / unsupported
    // receiver) is returned only after these contract-level mistakes.
    if !kwargs.is_empty() {
        return Err(unknown_keyword_arguments(kwargs));
    }
    if block.is_some() {
        return Err(format!(
            "{} does not take a block (block is not allowed)",
            method_call_label(receiver, name)
        )
        .into());
    }
    Err(match receiver {
        Value::CollectionRef(_) | Value::CollectionGroupRef(_) | Value::QueryRef(_) => {
            format!("unknown query method: {name}").into()
        }
        Value::Array(_) => format!("unknown array method: {name}").into(),
        Value::Timestamp(_) => format!("unknown timestamp method: {name}").into(),
        _ => "unsupported method receiver".into(),
    })
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
    "bytes",
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
) -> Result<ExecFlow, Box<dyn Error>>;

/// ビルトインの対応テーブル。
fn builtin_entries<F: Firestore>() -> [(&'static str, Builtin<F>); 20] {
    [
        ("puts", builtin_puts),
        ("doc", builtin_doc),
        ("col", builtin_col),
        ("col_group", builtin_col_group),
        ("array", builtin_array),
        ("len", builtin_len),
        ("now", builtin_now),
        ("timestamp", builtin_timestamp),
        ("bytes", builtin_bytes),
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
) -> Result<ExecFlow, Box<dyn Error>> {
    for v in args {
        println!("{v}");
    }
    Ok(ExecFlow::Value(Value::Nil))
}

fn builtin_doc<F: Firestore>(
    _rt: &mut Runtime<F>,
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

fn builtin_col<F: Firestore>(
    _rt: &mut Runtime<F>,
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

fn builtin_col_group<F: Firestore>(
    _rt: &mut Runtime<F>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let id = expect_string(args.first())?;
    if id.is_empty() || id.contains('/') {
        return Err(format!("invalid collection group id: {id}").into());
    }
    Ok(ExecFlow::Value(Value::CollectionGroupRef(
        CollectionGroupRef { id: id.to_string() },
    )))
}

fn builtin_array<F: Firestore>(
    _rt: &mut Runtime<F>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    Ok(ExecFlow::Value(Value::Array(args)))
}

fn builtin_len<F: Firestore>(
    _rt: &mut Runtime<F>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    match args.first() {
        Some(Value::Array(a)) => Ok(ExecFlow::Value(Value::Int(a.len() as i64))),
        Some(Value::Map(m)) => Ok(ExecFlow::Value(Value::Int(m.len() as i64))),
        Some(Value::Str(s)) => Ok(ExecFlow::Value(Value::Int(s.chars().count() as i64))),
        _ => Err("len expects array, map, or string".into()),
    }
}

fn builtin_now<F: Firestore>(
    _rt: &mut Runtime<F>,
    _args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    Ok(ExecFlow::Value(Value::Timestamp(Timestamp::now()?)))
}

fn builtin_timestamp<F: Firestore>(
    _rt: &mut Runtime<F>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    Ok(ExecFlow::Value(Value::Timestamp(Timestamp {
        seconds: parse_timestamp_arg(args.first())?,
    })))
}

fn builtin_bytes<F: Firestore>(
    _rt: &mut Runtime<F>,
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
        Some(Value::Str(s)) => s.into_bytes(),
        Some(Value::Array(items)) => items
            .into_iter()
            .map(|value| match value {
                Value::Int(byte) => u8::try_from(byte).map_err(|_| BYTES_ELEMENT_ERR.to_string()),
                _ => Err(BYTES_ELEMENT_ERR.to_string()),
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(BYTES_USAGE.into()),
    };
    Ok(ExecFlow::Value(Value::Bytes(bytes)))
}

fn builtin_server_timestamp<F: Firestore>(
    _rt: &mut Runtime<F>,
    _args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    Ok(ExecFlow::Value(Value::ServerTimestamp))
}

fn builtin_array_union<F: Firestore>(
    _rt: &mut Runtime<F>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    Ok(ExecFlow::Value(Value::ArrayUnion(
        expect_array(args.first())?.clone(),
    )))
}

fn builtin_array_remove<F: Firestore>(
    _rt: &mut Runtime<F>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    Ok(ExecFlow::Value(Value::ArrayRemove(
        expect_array(args.first())?.clone(),
    )))
}

fn builtin_delete_field<F: Firestore>(
    _rt: &mut Runtime<F>,
    _args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    Ok(ExecFlow::Value(Value::DeleteField))
}

fn builtin_increment<F: Firestore>(
    _rt: &mut Runtime<F>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    Ok(ExecFlow::Value(Value::Increment(expect_int(args.first())?)))
}

fn builtin_get<F: Firestore>(
    rt: &mut Runtime<F>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let path = expect_doc_path(args.first())?;
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
    Ok(ExecFlow::Value(v))
}

fn builtin_set<F: Firestore>(
    rt: &mut Runtime<F>,
    args: Vec<Value>,
    kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let path = expect_doc_path(args.first())?.to_string();
    let data = expect_map(args.get(1))?.clone();
    let merge = match (args.get(2), kwargs.get("merge")) {
        (_, Some(Value::Bool(v))) => *v,
        (Some(Value::Bool(v)), None) => *v,
        (None, None) => false,
        _ => return Err("set merge must be bool".into()),
    };
    rt.dispatch_write(WriteOp::Set { path, data, merge })?;
    Ok(ExecFlow::Value(Value::Nil))
}

fn builtin_update<F: Firestore>(
    rt: &mut Runtime<F>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let path = expect_doc_path(args.first())?.to_string();
    let data = expect_map(args.get(1))?.clone();
    rt.dispatch_write(WriteOp::Update { path, data })?;
    Ok(ExecFlow::Value(Value::Nil))
}

fn builtin_delete<F: Firestore>(
    rt: &mut Runtime<F>,
    args: Vec<Value>,
    _kwargs: BTreeMap<String, Value>,
    _block: Option<&Block>,
) -> Result<ExecFlow, Box<dyn Error>> {
    let path = expect_doc_path(args.first())?.to_string();
    rt.dispatch_write(WriteOp::Delete { path })?;
    Ok(ExecFlow::Value(Value::Nil))
}

fn builtin_batch<F: Firestore>(
    rt: &mut Runtime<F>,
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
    rt.fs.apply(writes).map_err(backend_err)?;
    Ok(flow)
}

fn builtin_transaction<F: Firestore>(
    rt: &mut Runtime<F>,
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
    rt.fs.begin_transaction().map_err(backend_err)?;
    let (flow, writes) = rt.run_staged_block(ExecCtx::Transaction { writes: vec![] }, &stmts.body);
    match flow {
        Ok(flow) => {
            if let Err(e) = rt.fs.commit_transaction(writes) {
                rt.abort_transaction_best_effort();
                return Err(backend_err(e));
            }
            Ok(flow)
        }
        Err(e) => {
            // Preserve the block error if rollback also fails.
            rt.abort_transaction_best_effort();
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

/// Convert an array index to the host index type. Generic over `T` so a test
/// can instantiate a narrower target width on a 64-bit host and exercise the
/// real `TryFrom` conversion rather than a test-only limit check. Production
/// always uses `usize`.
fn checked_array_index<T: TryFrom<i64>>(index: i64) -> Option<T> {
    T::try_from(index).ok()
}

fn eval_index(base: Value, index: Value) -> Result<Value, Box<dyn Error>> {
    match (base, index) {
        (Value::Map(map), Value::Str(key)) => Ok(map.get(&key).cloned().unwrap_or(Value::Nil)),
        (Value::Array(items), Value::Int(i)) => {
            let index = match checked_array_index::<usize>(i) {
                Some(index) => index,
                None => return Ok(Value::Nil),
            };
            Ok(items.get(index).cloned().unwrap_or(Value::Nil))
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
            (Value::Bytes(a), Value::Bytes(b)) => Value::Bool(a > b),
            _ => return Err("unsupported > operands".into()),
        },
        BinOp::Lt => match (l, r) {
            (Value::Int(a), Value::Int(b)) => Value::Bool(a < b),
            (Value::Timestamp(a), Value::Timestamp(b)) => Value::Bool(a.seconds < b.seconds),
            (Value::Bytes(a), Value::Bytes(b)) => Value::Bool(a < b),
            _ => return Err("unsupported < operands".into()),
        },
        BinOp::Ge => match (l, r) {
            (Value::Int(a), Value::Int(b)) => Value::Bool(a >= b),
            (Value::Timestamp(a), Value::Timestamp(b)) => Value::Bool(a.seconds >= b.seconds),
            (Value::Bytes(a), Value::Bytes(b)) => Value::Bool(a >= b),
            _ => return Err("unsupported >= operands".into()),
        },
        BinOp::Le => match (l, r) {
            (Value::Int(a), Value::Int(b)) => Value::Bool(a <= b),
            (Value::Timestamp(a), Value::Timestamp(b)) => Value::Bool(a.seconds <= b.seconds),
            (Value::Bytes(a), Value::Bytes(b)) => Value::Bool(a <= b),
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
    use std::panic::{catch_unwind, AssertUnwindSafe};

    #[test]
    fn array_index_conversion_respects_target_width() {
        // A 64-bit host would accept 2^32 as a usize index, so instantiate the
        // conversion at the narrower width to prove the narrowing is rejected
        // by a real `TryFrom` and not only by a host-specific bound.
        assert_eq!(
            checked_array_index::<u32>(4_294_967_295),
            Some(4_294_967_295)
        );
        assert_eq!(checked_array_index::<u32>(4_294_967_296), None);
        assert_eq!(checked_array_index::<u32>(4_294_967_297), None);
        assert_eq!(checked_array_index::<u32>(-1), None);
        assert_eq!(checked_array_index::<u16>(65_536), None);
        assert_eq!(checked_array_index::<usize>(0), Some(0));
        // The host width still accepts the full i64 range that fits it.
        assert_eq!(
            checked_array_index::<usize>(i64::MAX),
            Some(i64::MAX as usize)
        );
        assert_eq!(checked_array_index::<usize>(-1), None);
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

    struct FailingCommitFirestore {
        in_tx: bool,
        aborted: bool,
        abort_calls: usize,
        abort_failures_remaining: usize,
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
            self.abort_calls += 1;
            if !self.in_tx {
                return Ok(());
            }
            if self.abort_failures_remaining > 0 {
                self.abort_failures_remaining -= 1;
                return Err("abort failed".into());
            }
            self.in_tx = false;
            self.aborted = true;
            Ok(())
        }
    }

    fn empty_transaction_stmt() -> Stmt {
        Stmt::Expr(Expr::Call {
            receiver: None,
            name: "transaction".into(),
            args: vec![],
            kwargs: vec![],
            block: Some(Block {
                params: vec![],
                body: vec![],
            }),
        })
    }

    fn failing_commit_runtime(abort_failures_remaining: usize) -> Runtime<FailingCommitFirestore> {
        Runtime::new(FailingCommitFirestore {
            in_tx: false,
            aborted: false,
            abort_calls: 0,
            abort_failures_remaining,
        })
    }

    #[test]
    fn transaction_aborts_on_commit_failure() {
        let mut runtime = failing_commit_runtime(0);
        let res = runtime.eval_program(&[empty_transaction_stmt()]);
        assert!(res.is_err());
        assert_eq!(runtime.fs.abort_calls, 1);
        assert!(runtime.fs.aborted);
        assert!(!runtime.fs.in_tx);
    }

    #[test]
    fn transaction_keeps_commit_error_when_abort_also_fails() {
        let mut runtime = failing_commit_runtime(1);
        let err = runtime
            .eval_program(&[empty_transaction_stmt()])
            .unwrap_err();
        assert_eq!(err.to_string(), "commit failed");
        assert_eq!(runtime.fs.abort_calls, 1);
        // A failed rollback leaves the transaction available for retry.
        assert!(runtime.fs.in_tx);
    }

    #[test]
    fn transaction_keeps_block_error_when_abort_also_fails() {
        let mut runtime = failing_commit_runtime(1);
        let err = runtime.run("transaction { undefined_var }").unwrap_err();
        assert!(err
            .to_string()
            .contains("undefined variable: undefined_var"));
        assert_eq!(runtime.fs.abort_calls, 1);
        assert!(runtime.fs.in_tx);
    }

    #[test]
    fn next_transaction_retries_a_failed_rollback() {
        // Only the first rollback fails, so the retry before the next
        // `transaction` block succeeds and the block reaches commit instead of
        // failing with "already in transaction".
        let mut runtime = failing_commit_runtime(1);
        assert!(runtime.eval_program(&[empty_transaction_stmt()]).is_err());
        assert!(runtime.fs.in_tx);
        assert!(runtime.pending_rollback);

        // The fake commit always fails, so this run still errors — but with
        // the commit error, which proves the stale transaction was cleared
        // before `begin_transaction`.
        let err = runtime
            .eval_program(&[empty_transaction_stmt()])
            .unwrap_err();
        assert_eq!(err.to_string(), "commit failed");
        // Abort calls: the failed transaction's rollback, the retry before the
        // new begin, and the rollback after this commit failed too.
        assert_eq!(runtime.fs.abort_calls, 3);
        assert!(!runtime.fs.in_tx);
    }

    #[test]
    fn transaction_reports_backend_still_busy_when_rollback_keeps_failing() {
        // Every rollback fails, so the retry cannot clear the state. The next
        // block must still surface an honest error instead of succeeding.
        let mut runtime = failing_commit_runtime(usize::MAX);
        assert!(runtime.eval_program(&[empty_transaction_stmt()]).is_err());
        let err = runtime
            .eval_program(&[empty_transaction_stmt()])
            .unwrap_err();
        assert!(err.to_string().contains("already in transaction"), "{err}");
        assert!(runtime.pending_rollback);
    }

    #[test]
    fn transaction_abort_can_be_retried() {
        let mut runtime = failing_commit_runtime(1);
        runtime.fs.begin_transaction().unwrap();
        assert_eq!(
            runtime.fs.abort_transaction().unwrap_err().to_string(),
            "abort failed"
        );
        assert!(runtime.fs.in_tx);
        runtime.fs.abort_transaction().unwrap();
        assert!(!runtime.fs.in_tx);
        runtime.fs.begin_transaction().unwrap();
    }

    struct PanicQueryFirestore;

    impl Firestore for PanicQueryFirestore {
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
            panic!("query panic")
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

    #[test]
    fn scope_guards_restore_after_binding_error() {
        let mut runtime = Runtime::new(InMemoryFirestore::default());
        runtime.scopes.push(std::collections::HashMap::from([(
            "caller".to_string(),
            Value::Int(7),
        )]));

        {
            let scope = ScopeGuard::function(&mut runtime);
            scope.runtime.set_var("param", Value::Int(1)).unwrap();
            assert!(scope.runtime.set_var("param", Value::Int(2)).is_err());
        }
        assert_eq!(runtime.get_var("caller"), Some(&Value::Int(7)));
        assert!(runtime.get_var("param").is_none());

        {
            let scope = ScopeGuard::block(&mut runtime);
            scope.runtime.set_var("item", Value::Int(1)).unwrap();
            assert!(scope.runtime.set_var("item", Value::Int(2)).is_err());
        }
        assert_eq!(runtime.get_var("caller"), Some(&Value::Int(7)));
        assert!(runtime.get_var("item").is_none());
        runtime.scopes.pop();
    }

    #[test]
    fn scope_guards_restore_after_panic_unwind() {
        let mut runtime = Runtime::new(PanicQueryFirestore);

        let result = catch_unwind(AssertUnwindSafe(|| {
            runtime.run("fn f(param) { col(\"things\").get() }\nf(1)")
        }));
        assert!(result.is_err());
        assert!(runtime.run("param").is_err());

        let result = catch_unwind(AssertUnwindSafe(|| {
            runtime.run("[1].map { |item| col(\"things\").get() }")
        }));
        assert!(result.is_err());
        assert!(runtime.run("item").is_err());
    }
}
