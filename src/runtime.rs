use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::ast::{BinOp, Expr, Stmt};
use crate::{lexer, parser};

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Str(String),
    Bool(bool),
    Nil,
    Array(Vec<Value>),
    Map(BTreeMap<String, Value>),
    Timestamp(Timestamp),
    ServerTimestamp,
    ArrayUnion(Vec<Value>),
    ArrayRemove(Vec<Value>),
    DocRef(DocRef),
    CollectionRef(CollectionRef),
    CollectionGroupRef(CollectionGroupRef),
    QueryRef(QueryRef),
    Func(UserFunc),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Timestamp {
    pub seconds: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DocRef {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CollectionRef {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CollectionGroupRef {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueryRef {
    pub source: QuerySource,
    pub filters: Vec<QueryFilter>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum QuerySource {
    Collection(String),
    CollectionGroup(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueryFilter {
    pub field: String,
    pub op: String,
    pub value: Value,
}

#[derive(Debug, Clone)]
pub enum WriteOp {
    Set {
        path: String,
        data: BTreeMap<String, Value>,
        merge: bool,
    },
    Update {
        path: String,
        data: BTreeMap<String, Value>,
    },
    Delete {
        path: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecCtx {
    Normal,
    Batch,
    Transaction { wrote: bool },
}

#[derive(Debug, Clone, PartialEq)]
pub struct UserFunc {
    pub params: Vec<String>,
    pub body: Vec<Stmt>,
}

pub trait Firestore {
    fn get(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>>;
    fn set(
        &mut self,
        path: &str,
        value: BTreeMap<String, Value>,
        merge: bool,
    ) -> Result<(), Box<dyn Error>>;
    fn update(&mut self, path: &str, value: BTreeMap<String, Value>) -> Result<(), Box<dyn Error>>;
    fn delete(&mut self, path: &str) -> Result<(), Box<dyn Error>>;
    fn query_collection(
        &self,
        path: &str,
        filters: &[QueryFilter],
        limit: Option<usize>,
    ) -> Result<Vec<Value>, Box<dyn Error>>;
    fn query_collection_group(
        &self,
        id: &str,
        filters: &[QueryFilter],
        limit: Option<usize>,
    ) -> Result<Vec<Value>, Box<dyn Error>>;

    fn begin_batch(&mut self) -> Result<(), Box<dyn Error>>;
    fn batch_stage(&mut self, op: WriteOp) -> Result<(), Box<dyn Error>>;
    fn commit_batch(&mut self) -> Result<(), Box<dyn Error>>;
    fn abort_batch(&mut self);

    fn begin_transaction(&mut self) -> Result<(), Box<dyn Error>>;
    fn get_in_transaction(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>>;
    fn transaction_stage(&mut self, op: WriteOp) -> Result<(), Box<dyn Error>>;
    fn commit_transaction(&mut self) -> Result<(), Box<dyn Error>>;
    fn abort_transaction(&mut self);
}

enum InMemoryMode {
    Batch(Vec<WriteOp>),
    Transaction {
        snapshot: HashMap<String, BTreeMap<String, Value>>,
        writes: Vec<WriteOp>,
    },
}

#[derive(Default)]
pub struct InMemoryFirestore {
    docs: HashMap<String, BTreeMap<String, Value>>,
    mode: Option<InMemoryMode>,
}

impl Firestore for InMemoryFirestore {
    fn get(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>> {
        if self.mode.is_some() {
            return Err("internal error: get while in batch/transaction".into());
        }
        Ok(self.docs.get(path).cloned().map(Value::Map))
    }

    fn set(
        &mut self,
        path: &str,
        value: BTreeMap<String, Value>,
        merge: bool,
    ) -> Result<(), Box<dyn Error>> {
        if self.mode.is_some() {
            return Err("internal error: set while in batch/transaction".into());
        }
        apply_write_to_docs(
            &mut self.docs,
            WriteOp::Set {
                path: path.to_string(),
                data: value,
                merge,
            },
        )
    }

    fn update(&mut self, path: &str, value: BTreeMap<String, Value>) -> Result<(), Box<dyn Error>> {
        if self.mode.is_some() {
            return Err("internal error: update while in batch/transaction".into());
        }
        apply_write_to_docs(
            &mut self.docs,
            WriteOp::Update {
                path: path.to_string(),
                data: value,
            },
        )
    }

    fn delete(&mut self, path: &str) -> Result<(), Box<dyn Error>> {
        if self.mode.is_some() {
            return Err("internal error: delete while in batch/transaction".into());
        }
        apply_write_to_docs(
            &mut self.docs,
            WriteOp::Delete {
                path: path.to_string(),
            },
        )
    }

    fn query_collection(
        &self,
        path: &str,
        filters: &[QueryFilter],
        limit: Option<usize>,
    ) -> Result<Vec<Value>, Box<dyn Error>> {
        let mut matches: Vec<(&String, &BTreeMap<String, Value>)> = vec![];
        let prefix = format!("{path}/");
        for (doc_path, doc) in &self.docs {
            if !doc_path.starts_with(&prefix) {
                continue;
            }
            if doc_path[prefix.len()..].contains('/') {
                continue;
            }
            if matches_filters(doc, filters) {
                matches.push((doc_path, doc));
            }
        }
        matches.sort_by(|a, b| a.0.cmp(b.0));
        Ok(take_matches(matches, limit))
    }

    fn query_collection_group(
        &self,
        id: &str,
        filters: &[QueryFilter],
        limit: Option<usize>,
    ) -> Result<Vec<Value>, Box<dyn Error>> {
        let mut matches: Vec<(&String, &BTreeMap<String, Value>)> = vec![];
        for (doc_path, doc) in &self.docs {
            let segments: Vec<&str> = doc_path.split('/').collect();
            if segments.len() < 2 {
                continue;
            }
            let mut matched_group = false;
            for idx in (0..segments.len() - 1).step_by(2) {
                if segments[idx] == id {
                    matched_group = true;
                    break;
                }
            }
            if !matched_group {
                continue;
            }
            if matches_filters(doc, filters) {
                matches.push((doc_path, doc));
            }
        }
        matches.sort_by(|a, b| a.0.cmp(b.0));
        Ok(take_matches(matches, limit))
    }

    fn begin_batch(&mut self) -> Result<(), Box<dyn Error>> {
        if self.mode.is_some() {
            return Err("already in batch or transaction".into());
        }
        self.mode = Some(InMemoryMode::Batch(vec![]));
        Ok(())
    }

    fn batch_stage(&mut self, op: WriteOp) -> Result<(), Box<dyn Error>> {
        match &mut self.mode {
            Some(InMemoryMode::Batch(ops)) => {
                ops.push(op);
                Ok(())
            }
            _ => Err("not in batch".into()),
        }
    }

    fn commit_batch(&mut self) -> Result<(), Box<dyn Error>> {
        if let Some(InMemoryMode::Batch(ops)) = self.mode.take() {
            let mut new_docs = self.docs.clone();
            for op in ops {
                apply_write_to_docs(&mut new_docs, op)?;
            }
            self.docs = new_docs;
            Ok(())
        } else {
            Err("not in batch".into())
        }
    }

    fn abort_batch(&mut self) {
        if matches!(self.mode, Some(InMemoryMode::Batch(_))) {
            self.mode = None;
        }
    }

    fn begin_transaction(&mut self) -> Result<(), Box<dyn Error>> {
        if self.mode.is_some() {
            return Err("already in batch or transaction".into());
        }
        self.mode = Some(InMemoryMode::Transaction {
            snapshot: self.docs.clone(),
            writes: vec![],
        });
        Ok(())
    }

    fn get_in_transaction(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>> {
        match &self.mode {
            Some(InMemoryMode::Transaction { snapshot, .. }) => {
                Ok(snapshot.get(path).cloned().map(Value::Map))
            }
            _ => Err("not in transaction".into()),
        }
    }

    fn transaction_stage(&mut self, op: WriteOp) -> Result<(), Box<dyn Error>> {
        match &mut self.mode {
            Some(InMemoryMode::Transaction { writes, .. }) => {
                writes.push(op);
                Ok(())
            }
            _ => Err("not in transaction".into()),
        }
    }

    fn commit_transaction(&mut self) -> Result<(), Box<dyn Error>> {
        if let Some(InMemoryMode::Transaction { snapshot, writes }) = self.mode.take() {
            let mut new_docs = snapshot;
            for op in writes {
                apply_write_to_docs(&mut new_docs, op)?;
            }
            self.docs = new_docs;
            Ok(())
        } else {
            Err("not in transaction".into())
        }
    }

    fn abort_transaction(&mut self) {
        if matches!(self.mode, Some(InMemoryMode::Transaction { .. })) {
            self.mode = None;
        }
    }
}

fn apply_write_to_docs(
    docs: &mut HashMap<String, BTreeMap<String, Value>>,
    op: WriteOp,
) -> Result<(), Box<dyn Error>> {
    match op {
        WriteOp::Set {
            path,
            mut data,
            merge,
        } => {
            if merge {
                let current = docs.entry(path).or_default();
                apply_update_map(current, &mut data)?;
            } else {
                resolve_sentinel_map(&mut data, None)?;
                docs.insert(path, data);
            }
            Ok(())
        }
        WriteOp::Update { path, mut data } => {
            let current = docs
                .get_mut(&path)
                .ok_or_else(|| format!("document not found: {path}"))?;
            apply_update_map(current, &mut data)?;
            Ok(())
        }
        WriteOp::Delete { path } => {
            docs.remove(&path);
            Ok(())
        }
    }
}

fn wrap_doc(path: &str, doc: &BTreeMap<String, Value>) -> Value {
    let mut out = doc.clone();
    out.insert("__name__".to_string(), Value::Str(path.to_string()));
    Value::Map(out)
}

fn take_matches(
    sorted: Vec<(&String, &BTreeMap<String, Value>)>,
    limit: Option<usize>,
) -> Vec<Value> {
    let n = limit.map_or(sorted.len(), |l| l.min(sorted.len()));
    sorted
        .into_iter()
        .take(n)
        .map(|(p, d)| wrap_doc(p, d))
        .collect()
}

fn matches_filters(doc: &BTreeMap<String, Value>, filters: &[QueryFilter]) -> bool {
    filters.iter().all(|f| {
        let Some(actual) = doc.get(&f.field) else {
            return false;
        };
        match f.op.as_str() {
            "==" => actual == &f.value,
            ">" => compare_int(actual, &f.value, |a, b| a > b),
            ">=" => compare_int(actual, &f.value, |a, b| a >= b),
            "<" => compare_int(actual, &f.value, |a, b| a < b),
            "<=" => compare_int(actual, &f.value, |a, b| a <= b),
            _ => false,
        }
    })
}

fn compare_int(a: &Value, b: &Value, pred: impl Fn(i64, i64) -> bool) -> bool {
    matches!((a, b), (Value::Int(x), Value::Int(y)) if pred(*x, *y))
}

fn apply_update_map(
    current: &mut BTreeMap<String, Value>,
    updates: &mut BTreeMap<String, Value>,
) -> Result<(), Box<dyn Error>> {
    resolve_sentinel_map(updates, Some(current))?;
    for (k, v) in updates.iter() {
        current.insert(k.clone(), v.clone());
    }
    Ok(())
}

fn resolve_sentinel_map(
    map: &mut BTreeMap<String, Value>,
    current: Option<&BTreeMap<String, Value>>,
) -> Result<(), Box<dyn Error>> {
    for (k, v) in map.clone() {
        let resolved = match v {
            Value::ArrayUnion(values) => {
                let mut base = match current.and_then(|m| m.get(&k)) {
                    Some(Value::Array(arr)) => arr.clone(),
                    _ => vec![],
                };
                for item in values {
                    if !base.contains(&item) {
                        base.push(item);
                    }
                }
                Value::Array(base)
            }
            Value::ArrayRemove(values) => {
                let base = match current.and_then(|m| m.get(&k)) {
                    Some(Value::Array(arr)) => arr.clone(),
                    _ => vec![],
                };
                Value::Array(base.into_iter().filter(|v| !values.contains(v)).collect())
            }
            Value::ServerTimestamp => Value::Timestamp(Timestamp {
                seconds: current_unix_seconds()?,
            }),
            other => other,
        };
        map.insert(k, resolved);
    }
    Ok(())
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
                let body = if truthy(&self.eval_expr(cond)?) {
                    then_body
                } else {
                    else_body
                };
                self.eval_block(body)
            }
            Stmt::While { cond, body } => {
                let mut last = Value::Nil;
                while truthy(&self.eval_expr(cond)?) {
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
            ExecCtx::Batch => Err("queries are not allowed inside batch".into()),
            ExecCtx::Transaction { .. } => Err("queries are not allowed inside transaction".into()),
            ExecCtx::Normal => Ok(()),
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
                    println!("{}", render(&v));
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
                seconds: current_unix_seconds()?,
            })),
            "timestamp" => Ok(Value::Timestamp(Timestamp {
                seconds: parse_timestamp_arg(args.first())?,
            })),
            "server_timestamp" => Ok(Value::ServerTimestamp),
            "array_union" => Ok(Value::ArrayUnion(expect_array(args.first())?.clone())),
            "array_remove" => Ok(Value::ArrayRemove(expect_array(args.first())?.clone())),
            "get" => {
                let path = expect_doc_path(args.first())?;
                let v = match self.exec {
                    ExecCtx::Batch => return Err("get is not allowed inside batch".into()),
                    ExecCtx::Transaction { wrote: true } => {
                        return Err(
                            "get is not allowed after the first write in a transaction".into()
                        );
                    }
                    ExecCtx::Transaction { wrote: false } => {
                        self.fs.get_in_transaction(path)?.unwrap_or(Value::Nil)
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
                match &mut self.exec {
                    ExecCtx::Normal => self.fs.set(&path, data, merge)?,
                    ExecCtx::Batch => self.fs.batch_stage(WriteOp::Set { path, data, merge })?,
                    ExecCtx::Transaction { wrote } => {
                        self.fs
                            .transaction_stage(WriteOp::Set { path, data, merge })?;
                        *wrote = true;
                    }
                }
                Ok(Value::Nil)
            }
            "update" => {
                let path = expect_doc_path(args.first())?;
                let data = expect_map(args.get(1))?.clone();
                let path = path.to_string();
                match &mut self.exec {
                    ExecCtx::Normal => self.fs.update(&path, data)?,
                    ExecCtx::Batch => self.fs.batch_stage(WriteOp::Update { path, data })?,
                    ExecCtx::Transaction { wrote } => {
                        self.fs.transaction_stage(WriteOp::Update { path, data })?;
                        *wrote = true;
                    }
                }
                Ok(Value::Nil)
            }
            "delete" => {
                let path = expect_doc_path(args.first())?;
                let path = path.to_string();
                match &mut self.exec {
                    ExecCtx::Normal => self.fs.delete(&path)?,
                    ExecCtx::Batch => self.fs.batch_stage(WriteOp::Delete { path })?,
                    ExecCtx::Transaction { wrote } => {
                        self.fs.transaction_stage(WriteOp::Delete { path })?;
                        *wrote = true;
                    }
                }
                Ok(Value::Nil)
            }
            "batch" => {
                let Some(stmts) = block else {
                    return Err("batch requires { ... } block".into());
                };
                self.fs.begin_batch()?;
                self.exec = ExecCtx::Batch;
                let result = match self.eval_block(stmts) {
                    Ok(flow) => self.fs.commit_batch().map(|()| match flow {
                        ExecFlow::Value(v) | ExecFlow::Return(v) => v,
                    }),
                    Err(e) => {
                        self.fs.abort_batch();
                        Err(e)
                    }
                };
                self.exec = ExecCtx::Normal;
                result
            }
            "transaction" => {
                let Some(stmts) = block else {
                    return Err("transaction requires { ... } block".into());
                };
                self.fs.begin_transaction()?;
                self.exec = ExecCtx::Transaction { wrote: false };
                let result = match self.eval_block(stmts) {
                    Ok(flow) => self.fs.commit_transaction().map(|()| match flow {
                        ExecFlow::Value(v) | ExecFlow::Return(v) => v,
                    }),
                    Err(e) => {
                        self.fs.abort_transaction();
                        Err(e)
                    }
                };
                self.exec = ExecCtx::Normal;
                result
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
            Value::CollectionRef(collection) => match name {
                "doc" => Ok(Value::DocRef(DocRef {
                    path: format!("{}/{}", collection.path, expect_string(args.first())?),
                })),
                "where" => Ok(Value::QueryRef(QueryRef {
                    source: QuerySource::Collection(collection.path),
                    filters: vec![QueryFilter {
                        field: expect_string(args.first())?.to_string(),
                        op: expect_string(args.get(1))?.to_string(),
                        value: args.get(2).cloned().ok_or("where requires value")?,
                    }],
                    limit: None,
                })),
                "get" => {
                    self.forbid_query_in_batch_or_transaction()?;
                    let docs = self.fs.query_collection(&collection.path, &[], None)?;
                    Ok(Value::Array(docs))
                }
                "limit" => {
                    self.forbid_query_in_batch_or_transaction()?;
                    let n = expect_int(args.first())? as usize;
                    Ok(Value::QueryRef(QueryRef {
                        source: QuerySource::Collection(collection.path),
                        filters: vec![],
                        limit: Some(n),
                    }))
                }
                _ => Err(format!("unknown collection method: {name}").into()),
            },
            Value::CollectionGroupRef(group) => match name {
                "where" => Ok(Value::QueryRef(QueryRef {
                    source: QuerySource::CollectionGroup(group.id),
                    filters: vec![QueryFilter {
                        field: expect_string(args.first())?.to_string(),
                        op: expect_string(args.get(1))?.to_string(),
                        value: args.get(2).cloned().ok_or("where requires value")?,
                    }],
                    limit: None,
                })),
                "get" => {
                    self.forbid_query_in_batch_or_transaction()?;
                    let docs = self.fs.query_collection_group(&group.id, &[], None)?;
                    Ok(Value::Array(docs))
                }
                "limit" => {
                    self.forbid_query_in_batch_or_transaction()?;
                    let n = expect_int(args.first())? as usize;
                    Ok(Value::QueryRef(QueryRef {
                        source: QuerySource::CollectionGroup(group.id),
                        filters: vec![],
                        limit: Some(n),
                    }))
                }
                _ => Err(format!("unknown collection_group method: {name}").into()),
            },
            Value::QueryRef(mut query) => match name {
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
                    let docs = match &query.source {
                        QuerySource::Collection(path) => {
                            self.fs
                                .query_collection(path, &query.filters, query.limit)?
                        }
                        QuerySource::CollectionGroup(id) => {
                            self.fs
                                .query_collection_group(id, &query.filters, query.limit)?
                        }
                    };
                    Ok(Value::Array(docs))
                }
                _ => Err(format!("unknown query method: {name}").into()),
            },
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
                out.push_str(&render(&value));
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

pub(crate) fn current_unix_seconds() -> Result<i64, Box<dyn Error>> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64)
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

fn truthy(v: &Value) -> bool {
    !matches!(v, Value::Bool(false) | Value::Nil)
}

fn render(v: &Value) -> String {
    match v {
        Value::Int(i) => i.to_string(),
        Value::Str(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Nil => "nil".to_string(),
        Value::Array(a) => format!("{:?}", a),
        Value::Map(m) => format!("{:?}", m),
        Value::Timestamp(ts) => format!("Timestamp({})", ts.seconds),
        Value::ServerTimestamp => "ServerTimestamp".to_string(),
        Value::ArrayUnion(v) => format!("ArrayUnion({v:?})"),
        Value::ArrayRemove(v) => format!("ArrayRemove({v:?})"),
        Value::DocRef(d) => format!("#<DocRef path={}>", d.path),
        Value::CollectionRef(c) => format!("#<CollectionRef path={}>", c.path),
        Value::CollectionGroupRef(cg) => format!("#<CollectionGroupRef id={}>", cg.id),
        Value::QueryRef(_) => "#<QueryRef>".to_string(),
        Value::Func(_) => "<func>".to_string(),
    }
}

enum ExecFlow {
    Value(Value),
    Return(Value),
}
