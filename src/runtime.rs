use std::collections::{BTreeMap, HashMap};
use std::error::Error;

use crate::ast::{BinOp, Block, Expr, Stmt, StrPiece, UnOp};
use crate::firestore::{Firestore, WriteOp};
use crate::value::{
    CollectionGroupRef, CollectionRef, DocRef, QueryFilter, QueryOp, QueryRef, Timestamp, UserFunc,
    Value,
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
}

impl<F: Firestore> Runtime<F> {
    pub fn new(fs: F) -> Self {
        Self {
            scopes: vec![HashMap::new()],
            fs,
            exec: ExecCtx::Normal,
        }
    }

    pub fn eval_program(&mut self, stmts: &[Stmt]) -> Result<Value, Box<dyn Error>> {
        let mut last = Value::Nil;
        for stmt in stmts {
            match self.eval_stmt(stmt)? {
                ExecFlow::Value(v) => last = v,
                ExecFlow::Return(v) => return Ok(v),
            }
        }
        Ok(last)
    }

    fn eval_stmt(&mut self, stmt: &Stmt) -> Result<ExecFlow, Box<dyn Error>> {
        match stmt {
            Stmt::Expr(e) => Ok(ExecFlow::Value(self.eval_expr(e)?)),
            Stmt::Assign { name, value } => {
                if self.var_exists(name) {
                    return Err(format!("reassignment is not allowed: {name}").into());
                }
                let v = self.eval_expr(value)?;
                self.set_var(name, v.clone())?;
                Ok(ExecFlow::Value(v))
            }
            Stmt::Def { name, params, body } => {
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
                while self.eval_expr(cond)?.truthy() {
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

    fn forbid_query_in_batch_or_transaction(&self) -> Result<(), Box<dyn Error>> {
        match self.exec {
            ExecCtx::Batch { .. } => Err("queries are not allowed inside batch".into()),
            ExecCtx::Transaction { .. } => Err("queries are not allowed inside transaction".into()),
            ExecCtx::Normal => Ok(()),
        }
    }

    fn dispatch_write(&mut self, op: WriteOp) -> Result<(), Box<dyn Error>> {
        match &mut self.exec {
            ExecCtx::Normal => self.fs.apply(vec![op]),
            ExecCtx::Batch { writes } | ExecCtx::Transaction { writes } => {
                writes.push(op);
                Ok(())
            }
        }
    }

    fn eval_expr(&mut self, expr: &Expr) -> Result<Value, Box<dyn Error>> {
        match expr {
            Expr::Int(i) => Ok(Value::Int(*i)),
            Expr::Str(s) => Ok(Value::Str(s.clone())),
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
                Ok(Value::Str(out))
            }
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Nil => Ok(Value::Nil),
            Expr::Var(name) => self
                .get_var(name)
                .cloned()
                .ok_or_else(|| format!("undefined variable: {name}").into()),
            Expr::Binary { left, op, right } => {
                // `&&` / `||` は短絡評価し、Ruby と同じくオペランドそのものを返す。
                match op {
                    BinOp::And => {
                        let l = self.eval_expr(left)?;
                        if !l.truthy() {
                            Ok(l)
                        } else {
                            self.eval_expr(right)
                        }
                    }
                    BinOp::Or => {
                        let l = self.eval_expr(left)?;
                        if l.truthy() {
                            Ok(l)
                        } else {
                            self.eval_expr(right)
                        }
                    }
                    _ => eval_binary(self.eval_expr(left)?, *op, self.eval_expr(right)?),
                }
            }
            Expr::Unary { op, expr } => {
                let v = self.eval_expr(expr)?;
                Ok(match op {
                    UnOp::Not => Value::Bool(!v.truthy()),
                    UnOp::Neg => match v {
                        Value::Int(i) => Value::Int(i.checked_neg().ok_or("integer overflow")?),
                        _ => return Err("unsupported unary - operand".into()),
                    },
                })
            }
            Expr::Call {
                receiver,
                name,
                args,
                kwargs,
                block,
            } => {
                let mut argv = vec![];
                for arg in args {
                    argv.push(self.eval_expr(arg)?);
                }
                let mut kw = BTreeMap::new();
                for (k, vexpr) in kwargs {
                    kw.insert(k.clone(), self.eval_expr(vexpr)?);
                }
                self.eval_call(receiver.as_deref(), name, argv, kw, block.as_ref())
            }
            Expr::Map(entries) => {
                let mut out = BTreeMap::new();
                for (k, expr) in entries {
                    out.insert(k.clone(), self.eval_expr(expr)?);
                }
                Ok(Value::Map(out))
            }
            Expr::Array(items) => {
                let mut out = vec![];
                for item in items {
                    out.push(self.eval_expr(item)?);
                }
                Ok(Value::Array(out))
            }
            Expr::Index { base, index } => {
                let base = self.eval_expr(base)?;
                let index = self.eval_expr(index)?;
                eval_index(base, index)
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
    ) -> Result<Value, Box<dyn Error>> {
        if let Some(r) = receiver {
            let rec = self.eval_expr(r)?;
            if !kwargs.is_empty() {
                return Err("keyword args are not supported for method calls yet".into());
            }
            return self.eval_method_call(rec, name, args, block);
        }

        match name {
            "puts" => {
                for v in args {
                    println!("{v}");
                }
                Ok(Value::Nil)
            }
            "doc" => {
                let path = expect_string(args.first())?;
                validate_doc_path(path)?;
                Ok(Value::DocRef(DocRef {
                    path: path.to_string(),
                }))
            }
            "col" => {
                let path = expect_string(args.first())?;
                validate_collection_path(path)?;
                Ok(Value::CollectionRef(CollectionRef {
                    path: path.to_string(),
                }))
            }
            "col_group" => {
                let id = expect_string(args.first())?;
                if id.is_empty() || id.contains('/') {
                    return Err(format!("invalid collection group id: {id}").into());
                }
                Ok(Value::CollectionGroupRef(CollectionGroupRef {
                    id: id.to_string(),
                }))
            }
            "array" => Ok(Value::Array(args)),
            "len" => match args.first() {
                Some(Value::Array(a)) => Ok(Value::Int(a.len() as i64)),
                Some(Value::Map(m)) => Ok(Value::Int(m.len() as i64)),
                Some(Value::Str(s)) => Ok(Value::Int(s.chars().count() as i64)),
                _ => Err("len expects array, map, or string".into()),
            },

            "now" => Ok(Value::Timestamp(Timestamp {
                seconds: Timestamp::now()?.seconds,
            })),
            "timestamp" => Ok(Value::Timestamp(Timestamp {
                seconds: parse_timestamp_arg(args.first())?,
            })),
            "server_timestamp" => Ok(Value::ServerTimestamp),
            "array_union" => Ok(Value::ArrayUnion(expect_array(args.first())?.clone())),
            "array_remove" => Ok(Value::ArrayRemove(expect_array(args.first())?.clone())),
            "get" => {
                let path = expect_doc_path(args.first())?;
                let v = match &self.exec {
                    ExecCtx::Batch { .. } => {
                        return Err("get is not allowed inside batch".into());
                    }
                    ExecCtx::Transaction { writes } if !writes.is_empty() => {
                        return Err(
                            "get is not allowed after the first write in a transaction".into()
                        );
                    }
                    ExecCtx::Transaction { .. } => {
                        self.fs.transaction_get(path)?.unwrap_or(Value::Nil)
                    }
                    ExecCtx::Normal => self.fs.get(path)?.unwrap_or(Value::Nil),
                };
                Ok(v)
            }
            "set" => {
                let path = expect_doc_path(args.first())?;
                let data = expect_map(args.get(1))?.clone();
                let merge = match (args.get(2), kwargs.get("merge")) {
                    (_, Some(Value::Bool(v))) => *v,
                    (Some(Value::Bool(v)), None) => *v,
                    (None, None) => false,
                    _ => return Err("set merge must be bool".into()),
                };
                let path = path.to_string();
                self.dispatch_write(WriteOp::Set { path, data, merge })?;
                Ok(Value::Nil)
            }
            "update" => {
                let path = expect_doc_path(args.first())?;
                let data = expect_map(args.get(1))?.clone();
                let path = path.to_string();
                self.dispatch_write(WriteOp::Update { path, data })?;
                Ok(Value::Nil)
            }
            "delete" => {
                let path = expect_doc_path(args.first())?;
                let path = path.to_string();
                self.dispatch_write(WriteOp::Delete { path })?;
                Ok(Value::Nil)
            }
            "batch" => {
                let Some(stmts) = block else {
                    return Err("batch requires { ... } block".into());
                };
                if !stmts.params.is_empty() {
                    return Err("batch block does not take parameters".into());
                }
                if !matches!(self.exec, ExecCtx::Normal) {
                    return Err("batch cannot be nested inside batch/transaction".into());
                }
                self.exec = ExecCtx::Batch { writes: vec![] };
                let flow = self.eval_block(&stmts.body);
                let ExecCtx::Batch { writes } = std::mem::replace(&mut self.exec, ExecCtx::Normal)
                else {
                    unreachable!("exec ctx changed inside batch");
                };
                let flow = flow?;
                self.fs.apply(writes)?;
                Ok(flow.into_value())
            }
            "transaction" => {
                let Some(stmts) = block else {
                    return Err("transaction requires { ... } block".into());
                };
                if !stmts.params.is_empty() {
                    return Err("transaction block does not take parameters".into());
                }
                if !matches!(self.exec, ExecCtx::Normal) {
                    return Err("transaction cannot be nested inside batch/transaction".into());
                }
                self.fs.begin_transaction()?;
                self.exec = ExecCtx::Transaction { writes: vec![] };
                let flow = self.eval_block(&stmts.body);
                let ExecCtx::Transaction { writes } =
                    std::mem::replace(&mut self.exec, ExecCtx::Normal)
                else {
                    unreachable!("exec ctx changed inside transaction");
                };
                match flow {
                    Ok(flow) => {
                        self.fs.commit_transaction(writes)?;
                        Ok(flow.into_value())
                    }
                    Err(e) => {
                        self.fs.abort_transaction();
                        Err(e)
                    }
                }
            }
            _ => {
                let Some(Value::Func(func)) = self.get_var(name).cloned() else {
                    return Err(format!("undefined function: {name}").into());
                };
                let bound = bind_function_args(&func.params, args, kwargs)?;
                if func.params.len() != bound.len() {
                    return Err(format!("arg length mismatch for {name}").into());
                }
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
        }
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
            "get" => {
                self.forbid_query_in_batch_or_transaction()?;
                Ok(Value::Array(self.fs.run_query(&query)?))
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

fn parse_timestamp_arg(v: Option<&Value>) -> Result<i64, Box<dyn Error>> {
    match v {
        Some(Value::Int(i)) => Ok(*i),
        Some(Value::Str(s)) => parse_rfc3339_to_unix(s),
        _ => Err("timestamp(...) expects unix seconds int or RFC3339 string".into()),
    }
}

fn parse_rfc3339_to_unix(s: &str) -> Result<i64, Box<dyn Error>> {
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
            _ => return Err("unsupported > operands".into()),
        },
        BinOp::Lt => match (l, r) {
            (Value::Int(a), Value::Int(b)) => Value::Bool(a < b),
            _ => return Err("unsupported < operands".into()),
        },
        BinOp::Ge => match (l, r) {
            (Value::Int(a), Value::Int(b)) => Value::Bool(a >= b),
            _ => return Err("unsupported >= operands".into()),
        },
        BinOp::Le => match (l, r) {
            (Value::Int(a), Value::Int(b)) => Value::Bool(a <= b),
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
