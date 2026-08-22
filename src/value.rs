use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::ast::Stmt;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryOp {
    Eq,
    Gt,
    Ge,
    Lt,
    Le,
}

impl QueryOp {
    pub fn parse(s: &str) -> Result<Self, Box<dyn Error>> {
        Ok(match s {
            "==" => Self::Eq,
            ">" => Self::Gt,
            ">=" => Self::Ge,
            "<" => Self::Lt,
            "<=" => Self::Le,
            other => return Err(format!("unsupported query op: {other}").into()),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueryFilter {
    pub field: String,
    pub op: QueryOp,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UserFunc {
    pub params: Vec<String>,
    pub body: Vec<Stmt>,
}

impl Timestamp {
    pub fn now() -> Result<Self, Box<dyn Error>> {
        let seconds = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
        Ok(Self { seconds })
    }
}

impl Value {
    pub fn truthy(&self) -> bool {
        !matches!(self, Value::Bool(false) | Value::Nil)
    }
}

impl CollectionRef {
    pub fn to_query(&self) -> QueryRef {
        QueryRef {
            source: QuerySource::Collection(self.path.clone()),
            filters: vec![],
            limit: None,
        }
    }
}

impl CollectionGroupRef {
    pub fn to_query(&self) -> QueryRef {
        QueryRef {
            source: QuerySource::CollectionGroup(self.id.clone()),
            filters: vec![],
            limit: None,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(i) => write!(f, "{i}"),
            Value::Str(s) => write!(f, "{s}"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Nil => write!(f, "nil"),
            Value::Array(a) => write!(f, "{a:?}"),
            Value::Map(m) => write!(f, "{m:?}"),
            Value::Timestamp(ts) => write!(f, "Timestamp({})", ts.seconds),
            Value::ServerTimestamp => write!(f, "ServerTimestamp"),
            Value::ArrayUnion(v) => write!(f, "ArrayUnion({v:?})"),
            Value::ArrayRemove(v) => write!(f, "ArrayRemove({v:?})"),
            Value::DocRef(d) => write!(f, "#<DocRef path={}>", d.path),
            Value::CollectionRef(c) => write!(f, "#<CollectionRef path={}>", c.path),
            Value::CollectionGroupRef(cg) => write!(f, "#<CollectionGroupRef id={}>", cg.id),
            Value::QueryRef(_) => write!(f, "#<QueryRef>"),
            Value::Func(_) => write!(f, "<func>"),
        }
    }
}
