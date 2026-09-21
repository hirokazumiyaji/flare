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

impl Value {
    /// Ruby の `inspect` 風の開発者向け表現。`fl` の最終結果・複合値の
    /// `to_s` に使う。Str はダブルクォート付き、それ以外は素直な文字列表現。
    pub fn inspect(&self) -> String {
        match self {
            Value::Int(i) => i.to_string(),
            Value::Str(s) => inspect_str(s),
            Value::Bool(b) => b.to_string(),
            Value::Nil => "nil".to_string(),
            Value::Array(items) => {
                let inner: Vec<String> = items.iter().map(|v| v.inspect()).collect();
                format!("[{}]", inner.join(", "))
            }
            Value::Map(m) => {
                let inner: Vec<String> = m
                    .iter()
                    .map(|(k, v)| format!("{} => {}", inspect_str(k), v.inspect()))
                    .collect();
                format!("{{{}}}", inner.join(", "))
            }
            Value::Timestamp(ts) => format!("Timestamp({})", ts.seconds),
            Value::ServerTimestamp => "ServerTimestamp".to_string(),
            Value::ArrayUnion(v) => {
                let inner: Vec<String> = v.iter().map(|x| x.inspect()).collect();
                format!("ArrayUnion([{}])", inner.join(", "))
            }
            Value::ArrayRemove(v) => {
                let inner: Vec<String> = v.iter().map(|x| x.inspect()).collect();
                format!("ArrayRemove([{}])", inner.join(", "))
            }
            Value::DocRef(d) => format!("#<DocRef path={}>", inspect_str(&d.path)),
            Value::CollectionRef(c) => format!("#<CollectionRef path={}>", inspect_str(&c.path)),
            Value::CollectionGroupRef(cg) => format!("#<CollectionGroupRef id={}>", inspect_str(&cg.id)),
            Value::QueryRef(_) => "#<QueryRef>".to_string(),
            Value::Func(_) => "<func>".to_string(),
        }
    }
}

fn inspect_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other if other.is_control() => out.push_str(&format!("\\u{{{:04x}}}", other as u32)),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

impl fmt::Display for Value {
    /// Ruby の `to_s` 風: Str は素で出し、それ以外は `inspect` と同じ。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Str(s) => write!(f, "{s}"),
            other => write!(f, "{}", other.inspect()),
        }
    }
}
