use std::collections::{BTreeMap, HashMap};
use std::error::Error;

use crate::ast::{BinOp, Expr, Stmt};
use crate::firestore::{Firestore, WriteOp};
use crate::value::{
    CollectionGroupRef, CollectionRef, DocRef, QueryFilter, QueryRef, QuerySource, Timestamp,
    UserFunc, Value,
};
use crate::{lexer, parser};

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
            Expr::Str(s) => Ok(Value::Str(self.eval_interpolated_string(s)?)),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Nil => Ok(Value::Nil),
            Expr::Var(name) => self
                .get_var(name)
                .cloned()
                .ok_or_else(|| format!("undefined variable: {name}").into()),
            Expr::Binary { left, op, right } => {
                eval_binary(self.eval_expr(left)?, *op, self.eval_expr(right)?)
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
        }
    }

    fn eval_call(
        &mut self,
        receiver: Option<&Expr>,
        name: &str,
        args: Vec<Value>,
        kwargs: BTreeMap<String, Value>,
        block: Option<&Vec<Stmt>>,
    ) -> Result<Value, Box<dyn Error>> {
        if let Some(r) = receiver {
            let rec = self.eval_expr(r)?;
            if !kwargs.is_empty() {
                return Err("keyword args are not supported for method calls yet".into());
            }
            return self.eval_method_call(rec, name, args);
        }

        match name {
            "puts" => {
                for v in args {
                    println!("{v}");
                }
                Ok(Value::Nil)
            }
            "doc" => Ok(Value::DocRef(DocRef {
                path: expect_string(args.first())?.to_string(),
            })),
            "collection" => Ok(Value::CollectionRef(CollectionRef {
                path: expect_string(args.first())?.to_string(),
            })),
            "collection_group" => Ok(Value::CollectionGroupRef(CollectionGroupRef {
                id: expect_string(args.first())?.to_string(),
            })),
            "array" => Ok(Value::Array(args)),

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
                if !matches!(self.exec, ExecCtx::Normal) {
                    return Err("batch cannot be nested inside batch/transaction".into());
                }
                self.exec = ExecCtx::Batch { writes: vec![] };
                let flow = self.eval_block(stmts);
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
                if !matches!(self.exec, ExecCtx::Normal) {
                    return Err("transaction cannot be nested inside batch/transaction".into());
                }
                self.fs.begin_transaction()?;
                self.exec = ExecCtx::Transaction { writes: vec![] };
                let flow = self.eval_block(stmts);
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
                self.scopes.push(HashMap::new());
                for (param, value) in bound {
                    self.set_var(&param, value)?;
                }
                let res = self.eval_block(&func.body)?;
                self.scopes.pop();
                Ok(match res {
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
    ) -> Result<Value, Box<dyn Error>> {
        match receiver {
            Value::CollectionRef(collection) if name == "doc" => Ok(Value::DocRef(DocRef {
                path: format!("{}/{}", collection.path, expect_string(args.first())?),
            })),
            Value::CollectionRef(collection) => {
                self.eval_query_method(collection.to_query(), name, args)
            }
            Value::CollectionGroupRef(group) => {
                self.eval_query_method(group.to_query(), name, args)
            }
            Value::QueryRef(query) => self.eval_query_method(query, name, args),
            Value::Timestamp(ts) => match name {
                "to_unix" => Ok(Value::Int(ts.seconds)),
                "add_seconds" => Ok(Value::Timestamp(Timestamp {
                    seconds: ts.seconds + expect_int(args.first())?,
                })),
                _ => Err(format!("unknown timestamp method: {name}").into()),
            },
            _ => Err("unsupported method receiver".into()),
        }
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
                    op: expect_string(args.get(1))?.to_string(),
                    value: args.get(2).cloned().ok_or("where requires value")?,
                });
                Ok(Value::QueryRef(query))
            }
            "limit" => {
                query.limit = Some(expect_int(args.first())? as usize);
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

    fn eval_interpolated_string(&mut self, raw: &str) -> Result<String, Box<dyn Error>> {
        if !raw.contains("#{") {
            return Ok(raw.to_string());
        }
        let mut out = String::new();
        let chars: Vec<char> = raw.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '#' && i + 1 < chars.len() && chars[i + 1] == '{' {
                i += 2;
                let mut depth = 1;
                let mut expr = String::new();
                while i < chars.len() && depth > 0 {
                    match chars[i] {
                        '{' => {
                            depth += 1;
                            expr.push('{');
                        }
                        '}' => {
                            depth -= 1;
                            if depth > 0 {
                                expr.push('}');
                            }
                        }
                        ch => expr.push(ch),
                    }
                    i += 1;
                }
                if depth != 0 {
                    return Err("unterminated string interpolation".into());
                }
                let value = self.eval_embedded_expr(expr.trim())?;
                out.push_str(&value.to_string());
            } else {
                out.push(chars[i]);
                i += 1;
            }
        }
        Ok(out)
    }

    fn eval_embedded_expr(&mut self, expr_src: &str) -> Result<Value, Box<dyn Error>> {
        let tokens = lexer::lex(expr_src)?;
        let mut p = parser::Parser::new(tokens);
        let program = p.parse_program()?;
        if program.len() != 1 {
            return Err("interpolation must contain a single expression".into());
        }
        match &program[0] {
            Stmt::Expr(expr) => self.eval_expr(expr),
            _ => Err("interpolation must contain an expression".into()),
        }
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
        Some(Value::Str(s)) => Ok(s),
        Some(Value::DocRef(d)) => Ok(&d.path),
        _ => Err("expected document path string or doc object".into()),
    }
}

fn eval_binary(l: Value, op: BinOp, r: Value) -> Result<Value, Box<dyn Error>> {
    Ok(match op {
        BinOp::Add => match (l, r) {
            (Value::Int(a), Value::Int(b)) => Value::Int(a + b),
            (Value::Str(a), Value::Str(b)) => Value::Str(format!("{a}{b}")),
            _ => return Err("unsupported + operands".into()),
        },
        BinOp::Sub => match (l, r) {
            (Value::Int(a), Value::Int(b)) => Value::Int(a - b),
            _ => return Err("unsupported - operands".into()),
        },
        BinOp::Mul => match (l, r) {
            (Value::Int(a), Value::Int(b)) => Value::Int(a * b),
            _ => return Err("unsupported * operands".into()),
        },
        BinOp::Div => match (l, r) {
            (Value::Int(a), Value::Int(b)) => {
                if b == 0 {
                    return Err("division by zero".into());
                }
                Value::Int(a / b)
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
    })
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
