use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::ast::Stmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Str(String),
    /// UTF-8 ではないデータも失わず保持するバイナリ値。
    /// `bytes([0, 255])` で生成し、Storage の内容として往返する。
    Bytes(Vec<u8>),
    Bool(bool),
    Nil,
    Array(Vec<Value>),
    Map(BTreeMap<String, Value>),
    Timestamp(Timestamp),
    ServerTimestamp,
    ArrayUnion(Vec<Value>),
    ArrayRemove(Vec<Value>),
    DeleteField,
    Increment(i64),
    /// emulator が読んだ flare 外の型(double / geo_point)の表示専用値。
    /// スクリプトからは生成できず、書き戻しもできない。
    Opaque(String),
    /// A local Firebase Auth session.  This is intentionally a distinct value
    /// from a Map so token fields cannot be accidentally interpolated by the
    /// ordinary `inspect`/`to_s` paths.
    AuthSession(crate::auth::AuthSession),
    /// A token projected out of an AuthSession for script field access.  The
    /// raw token is never represented as a script Value.
    RedactedToken(&'static str),
    Module(BuiltinModule),
    DocRef(DocRef),
    CollectionRef(CollectionRef),
    CollectionGroupRef(CollectionGroupRef),
    QueryRef(QueryRef),
    Func(UserFunc),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinModule {
    Firestore,
    Auth,
    Storage,
    IO,
    File,
    JSON,
    CSV,
    YAML,
}

impl BuiltinModule {
    pub fn name(self) -> &'static str {
        match self {
            Self::Firestore => "Firestore",
            Self::Auth => "Auth",
            Self::Storage => "Storage",
            Self::IO => "IO",
            Self::File => "File",
            Self::JSON => "JSON",
            Self::CSV => "CSV",
            Self::YAML => "YAML",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        match name {
            "Firestore" => Some(Self::Firestore),
            "Auth" => Some(Self::Auth),
            "Storage" => Some(Self::Storage),
            "IO" => Some(Self::IO),
            "File" => Some(Self::File),
            "JSON" => Some(Self::JSON),
            "CSV" => Some(Self::CSV),
            "YAML" => Some(Self::YAML),
            _ => None,
        }
    }
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
    pub order_by: Vec<(String, Direction)>,
    pub offset: Option<usize>,
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
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    In,
    NotIn,
    ArrayContains,
    ArrayContainsAny,
}

impl QueryOp {
    pub fn parse(s: &str) -> Result<Self, Box<dyn Error>> {
        Ok(match s {
            "==" => Self::Eq,
            "!=" => Self::Ne,
            ">" => Self::Gt,
            ">=" => Self::Ge,
            "<" => Self::Lt,
            "<=" => Self::Le,
            "in" => Self::In,
            "not-in" => Self::NotIn,
            "array_contains" => Self::ArrayContains,
            "array_contains_any" => Self::ArrayContainsAny,
            other => return Err(format!("unsupported query op: {other}").into()),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Asc,
    Desc,
}

impl Direction {
    pub fn parse(s: &str) -> Result<Self, Box<dyn Error>> {
        Ok(match s {
            "asc" => Self::Asc,
            "desc" => Self::Desc,
            other => {
                return Err(format!(
                    "order_by direction must be \"asc\" or \"desc\", got {other:?}"
                )
                .into())
            }
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

/// Validate a runtime value without recursively walking its Rust data
/// structure.  Runtime values can come from a backend, so checking only
/// parser-generated literals is not sufficient.  The root has depth zero;
/// nested arrays/maps increase the depth by one.
pub fn validate_value_depth(value: &Value, max_depth: usize) -> Result<(), Box<dyn Error>> {
    validate_value_limits(value, Some(max_depth), None).map(|_| ())
}

/// Reject a value that nests deeper than `max_depth` allows.
///
/// The limit is inclusive: a value at exactly `max_depth` is valid, which is
/// why the caller checks the depth of a child *when the child exists* rather
/// than predicting it from the parent.
fn check_value_depth_budget(depth: usize, max_depth: Option<usize>) -> Result<(), Box<dyn Error>> {
    match max_depth {
        Some(max) if depth > max => Err(format!("value nesting limit exceeded (max {max})").into()),
        _ => Ok(()),
    }
}

/// Count one node against the node budget.
///
/// Counting happens on the way *in*: the traversal pushes children onto its
/// stack as it goes, so a backend-supplied value that is far wider than the
/// budget must not be able to allocate traversal memory proportional to the
/// whole value before the limit fires.
fn check_value_node_budget(
    nodes: &mut usize,
    max_nodes: Option<usize>,
) -> Result<(), Box<dyn Error>> {
    *nodes = nodes.saturating_add(1);
    match max_nodes {
        Some(max) if *nodes > max => Err(format!("value node limit exceeded (max {max})").into()),
        _ => Ok(()),
    }
}

/// Validate both nesting and total node count of a runtime value in a single
/// pass and return the observed maximum depth.
///
/// The root is depth 0 and counts as one node, so `max_depth = 0` accepts only
/// a value that has no children (a leaf, an empty array, or an empty map).
pub fn validate_value_limits(
    value: &Value,
    max_depth: Option<usize>,
    max_nodes: Option<usize>,
) -> Result<usize, Box<dyn Error>> {
    validate_value_limits_with_sizes(value, max_depth, max_nodes, None, None)
}

fn check_value_size_leaf(
    val: &Value,
    max_string_bytes: Option<usize>,
    max_bytes_size: Option<usize>,
) -> Result<(), Box<dyn Error>> {
    match val {
        Value::Str(s) => {
            if let Some(max) = max_string_bytes {
                if s.len() > max {
                    return Err(format!("string size limit exceeded (max {max})").into());
                }
            }
        }
        Value::Bytes(b) => {
            if let Some(max) = max_bytes_size {
                if b.len() > max {
                    return Err(format!("bytes size limit exceeded (max {max})").into());
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// Validate nesting, total node count, and individual string/bytes sizes of a runtime
/// value in a single bounded pass.
pub fn validate_value_limits_with_sizes(
    value: &Value,
    max_depth: Option<usize>,
    max_nodes: Option<usize>,
    max_string_bytes: Option<usize>,
    max_bytes_size: Option<usize>,
) -> Result<usize, Box<dyn Error>> {
    let mut stack = vec![(value, 0usize)];
    let mut nodes = 0usize;
    let mut observed_depth = 0usize;
    check_value_node_budget(&mut nodes, max_nodes)?;
    check_value_size_leaf(value, max_string_bytes, max_bytes_size)?;

    while let Some((value, depth)) = stack.pop() {
        check_value_depth_budget(depth, max_depth)?;
        observed_depth = observed_depth.max(depth);
        let child_depth = depth.saturating_add(1);

        match value {
            Value::Array(items) | Value::ArrayUnion(items) | Value::ArrayRemove(items) => {
                for item in items {
                    // Bound each real child as it is enqueued: an empty
                    // container at the limit is valid, so the depth check must
                    // not run for children that do not exist.
                    check_value_depth_budget(child_depth, max_depth)?;
                    check_value_node_budget(&mut nodes, max_nodes)?;
                    check_value_size_leaf(item, max_string_bytes, max_bytes_size)?;
                    stack.push((item, child_depth));
                }
            }
            Value::Map(map) => {
                for item in map.values() {
                    check_value_depth_budget(child_depth, max_depth)?;
                    check_value_node_budget(&mut nodes, max_nodes)?;
                    check_value_size_leaf(item, max_string_bytes, max_bytes_size)?;
                    stack.push((item, child_depth));
                }
            }
            // Function bodies are ASTs and are checked by the parser.  Other
            // scalar/reference values are leaves from the data-value point of
            // view.
            _ => {}
        }
    }
    Ok(observed_depth)
}

impl Timestamp {
    pub fn now() -> Result<Self, Box<dyn Error>> {
        let seconds = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
        Ok(Self { seconds })
    }

    /// Unix 秒を RFC3339 (UTC, `+00:00`) に整形する。範囲外はエラー。
    pub fn to_rfc3339(&self) -> Result<String, Box<dyn Error>> {
        use chrono::{Datelike, TimeZone};
        chrono::Utc
            .timestamp_opt(self.seconds, 0)
            .single()
            .and_then(|dt| (0..=9999).contains(&dt.year()).then(|| dt.to_rfc3339()))
            .ok_or_else(|| "timestamp out of range".into())
    }
}

impl Value {
    pub fn truthy(&self) -> bool {
        !matches!(self, Value::Bool(false) | Value::Nil)
    }

    /// クエリ `order_by` 用の全順序。同型は自然順(Int 数値順 / Str 辞書順 /
    /// Bool `false < true` / Timestamp 秒 / Bytes の辞書順)、異型は Nil <
    /// Bool < Int < Timestamp < Str < Bytes < Array < Map < その他の
    /// 型ランクで比べる。両バックエンドの決定性を同じコードで保証するために
    /// 使う(spec §7)。
    pub fn cmp_for_order(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (Value::Int(x), Value::Int(y)) => x.cmp(y),
            (Value::Str(x), Value::Str(y)) => x.cmp(y),
            (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
            (Value::Timestamp(x), Value::Timestamp(y)) => x.seconds.cmp(&y.seconds),
            (Value::Bytes(x), Value::Bytes(y)) => x.cmp(y),
            _ => type_rank(self).cmp(&type_rank(other)),
        }
    }

    /// クエリ `where` の大小比較。同じ順序関係を持つ型だけが比較可能。
    ///
    /// `order_by` の全順序とは別の契約である。Nil / Array / Map は
    /// `where` の大小比較では順序付けられず、異型も比較結果にならない。
    /// emulator の Firestore 型順序に依存しないための共通実装。
    pub fn cmp_for_where(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match (self, other) {
            (Value::Int(x), Value::Int(y)) => Some(x.cmp(y)),
            (Value::Str(x), Value::Str(y)) => Some(x.cmp(y)),
            (Value::Bool(x), Value::Bool(y)) => Some(x.cmp(y)),
            (Value::Timestamp(x), Value::Timestamp(y)) => Some(x.seconds.cmp(&y.seconds)),
            (Value::Bytes(x), Value::Bytes(y)) => Some(x.cmp(y)),
            _ => None,
        }
    }
}

fn type_rank(v: &Value) -> u8 {
    match v {
        Value::Nil => 0,
        Value::Bool(_) => 1,
        Value::Int(_) => 2,
        Value::Timestamp(_) => 3,
        Value::Str(_) => 4,
        Value::Bytes(_) => 5,
        Value::Array(_) => 6,
        Value::Map(_) => 7,
        _ => 8,
    }
}

impl CollectionRef {
    pub fn to_query(&self) -> QueryRef {
        QueryRef {
            source: QuerySource::Collection(self.path.clone()),
            filters: vec![],
            order_by: vec![],
            offset: None,
            limit: None,
        }
    }
}

impl CollectionGroupRef {
    pub fn to_query(&self) -> QueryRef {
        QueryRef {
            source: QuerySource::CollectionGroup(self.id.clone()),
            filters: vec![],
            order_by: vec![],
            offset: None,
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
            Value::Bytes(bytes) => inspect_bytes(bytes),
            Value::ServerTimestamp => "ServerTimestamp".to_string(),
            Value::ArrayUnion(v) => {
                let inner: Vec<String> = v.iter().map(|x| x.inspect()).collect();
                format!("ArrayUnion([{}])", inner.join(", "))
            }
            Value::ArrayRemove(v) => {
                let inner: Vec<String> = v.iter().map(|x| x.inspect()).collect();
                format!("ArrayRemove([{}])", inner.join(", "))
            }
            Value::DeleteField => "DeleteField".to_string(),
            Value::Increment(n) => format!("Increment({n})"),
            Value::Opaque(s) => s.clone(),
            Value::AuthSession(session) => format!(
                "AuthSession(user={}, id_token=<redacted>, refresh_token=<redacted>, expires_at={})",
                session.user.to_value().inspect(),
                session.expires_at
            ),
            Value::RedactedToken(name) => format!("<redacted:{name}>"),
            Value::Module(module) => module.name().to_string(),
            Value::DocRef(d) => format!("#<DocRef path={}>", inspect_str(&d.path)),
            Value::CollectionRef(c) => format!("#<CollectionRef path={}>", inspect_str(&c.path)),
            Value::CollectionGroupRef(cg) => {
                format!("#<CollectionGroupRef id={}>", inspect_str(&cg.id))
            }
            Value::QueryRef(_) => "#<QueryRef>".to_string(),
            Value::Func(_) => "<func>".to_string(),
        }
    }
}

const BYTES_INSPECT_LIMIT: usize = 32;

fn inspect_bytes(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let shown = bytes.len().min(BYTES_INSPECT_LIMIT);
    let mut out = String::with_capacity(7 + shown.saturating_mul(2));
    out.push_str("Bytes(");
    // `format!` で 1 バイトごとに使い捨ての String を確保するのを避けて、
    // 既存のバッファへ直接書き込む(`inspect()` は毎回の文字列表示で動く)。
    for byte in &bytes[..shown] {
        // String への書き込みは失敗しない。
        let _ = write!(out, "{byte:02x}");
    }
    // `shown = min(len, LIMIT)` なので常に `len >= shown` であり、
    // `checked_sub` の None 分支には到達しない。
    let omitted = bytes.len() - shown;
    if omitted > 0 {
        let _ = write!(out, ", {omitted} more bytes");
    }
    out.push(')');
    out
}

fn inspect_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '#' if chars.peek() == Some(&'{') => out.push_str("\\#"),
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
