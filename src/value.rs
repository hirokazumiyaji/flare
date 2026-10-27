use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::rc::Rc;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    pub params: Rc<[crate::ast::FuncParam]>,
    pub body: Rc<[Stmt]>,
}

impl UserFunc {
    pub fn new(
        params: impl Into<Rc<[crate::ast::FuncParam]>>,
        body: impl Into<Rc<[Stmt]>>,
    ) -> Self {
        Self {
            params: params.into(),
            body: body.into(),
        }
    }
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

/// Validate a leaf value: it counts as the one root node and may carry a
/// size-limited `Str`/`Bytes` payload.
pub(crate) fn check_leaf_value_limits(
    value: &Value,
    max_nodes: Option<usize>,
    max_string_bytes: Option<usize>,
    max_bytes_size: Option<usize>,
) -> Result<(), Box<dyn Error>> {
    let mut nodes = 0usize;
    check_value_node_budget(&mut nodes, max_nodes)?;
    check_value_size_leaf(value, max_string_bytes, max_bytes_size)
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
    if !value.is_composite() {
        check_leaf_value_limits(value, max_nodes, max_string_bytes, max_bytes_size)?;
        return Ok(0);
    }

    let mut stack = vec![(value, 0usize)];
    let mut nodes = 0usize;
    let mut observed_depth = 0usize;
    check_value_node_budget(&mut nodes, max_nodes)?;
    check_value_size_leaf(value, max_string_bytes, max_bytes_size)?;

    while let Some((value, depth)) = stack.pop() {
        check_value_depth_budget(depth, max_depth)?;
        observed_depth = observed_depth.max(depth);
        // Function bodies are ASTs and are checked by the parser. Other
        // scalar/reference values are leaves from the data-value point of
        // view, so they add no children to walk.
        let Some(children) = value.children() else {
            continue;
        };
        let child_depth = depth.saturating_add(1);
        for item in children {
            // Bound each real child as it is enqueued: an empty container at
            // the limit is valid, so the depth check must not run for children
            // that do not exist.
            check_value_depth_budget(child_depth, max_depth)?;
            check_value_node_budget(&mut nodes, max_nodes)?;
            check_value_size_leaf(item, max_string_bytes, max_bytes_size)?;
            stack.push((item, child_depth));
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

/// Iterator over the direct children of a composite [`Value`].
///
/// A concrete enum keeps the hot budget walks allocation-free.
pub(crate) enum ValueChildren<'a> {
    Array(std::slice::Iter<'a, Value>),
    Map(std::collections::btree_map::Values<'a, String, Value>),
}

impl<'a> Iterator for ValueChildren<'a> {
    type Item = &'a Value;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Array(iter) => iter.next(),
            Self::Map(iter) => iter.next(),
        }
    }
}

impl Value {
    pub fn truthy(&self) -> bool {
        !matches!(self, Value::Bool(false) | Value::Nil)
    }

    /// Direct children of a composite value, or `None` for a leaf.
    ///
    /// This is the single source of truth for the container variants: the
    /// budget validator and every budget walk descend through this iterator,
    /// so a new container variant only has to be added here.
    pub(crate) fn children(&self) -> Option<ValueChildren<'_>> {
        match self {
            Value::Array(items) | Value::ArrayUnion(items) | Value::ArrayRemove(items) => {
                Some(ValueChildren::Array(items.iter()))
            }
            Value::Map(map) => Some(ValueChildren::Map(map.values())),
            _ => None,
        }
    }

    /// Whether this value is a container whose children count against the
    /// value budgets.
    pub fn is_composite(&self) -> bool {
        self.children().is_some()
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
            (Value::Array(x), Value::Array(y)) => x
                .iter()
                .zip(y.iter())
                .map(|(a, b)| a.cmp_for_order(b))
                .find(|o| o.is_ne())
                .unwrap_or_else(|| x.len().cmp(&y.len())),
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

/// Default nesting depth ceiling for [`Value::inspect`].
///
/// Compound values deeper than this limit are truncated with `[...]` or `{...}`
/// to prevent unbounded memory growth and stack exhaustion during formatting.
pub const DEFAULT_MAX_INSPECT_DEPTH: usize = 64;

enum InspectAction<'a> {
    Value(&'a Value, usize),
    OwnedValue(Box<Value>, usize),
    Str(&'static str),
    String(String),
}

fn format_leaf_value(val: &Value, out: &mut String) -> bool {
    use std::fmt::Write;
    match val {
        Value::Int(i) => {
            let _ = write!(out, "{i}");
            true
        }
        Value::Str(s) => {
            out.push_str(&inspect_str(s));
            true
        }
        Value::Bool(b) => {
            out.push_str(if *b { "true" } else { "false" });
            true
        }
        Value::Nil => {
            out.push_str("nil");
            true
        }
        Value::Timestamp(ts) => {
            let _ = write!(out, "Timestamp({})", ts.seconds);
            true
        }
        Value::Bytes(b) => {
            out.push_str(&inspect_bytes(b));
            true
        }
        Value::ServerTimestamp => {
            out.push_str("ServerTimestamp");
            true
        }
        Value::DeleteField => {
            out.push_str("DeleteField");
            true
        }
        Value::Increment(n) => {
            let _ = write!(out, "Increment({n})");
            true
        }
        Value::Opaque(s) => {
            out.push_str(s);
            true
        }
        Value::RedactedToken(name) => {
            let _ = write!(out, "<redacted:{name}>");
            true
        }
        Value::Module(module) => {
            out.push_str(module.name());
            true
        }
        Value::DocRef(d) => {
            let _ = write!(out, "#<DocRef path={}>", inspect_str(&d.path));
            true
        }
        Value::CollectionRef(c) => {
            let _ = write!(out, "#<CollectionRef path={}>", inspect_str(&c.path));
            true
        }
        Value::CollectionGroupRef(cg) => {
            let _ = write!(out, "#<CollectionGroupRef id={}>", inspect_str(&cg.id));
            true
        }
        Value::QueryRef(_) => {
            out.push_str("#<QueryRef>");
            true
        }
        Value::Func(_) => {
            out.push_str("<func>");
            true
        }
        _ => false,
    }
}

impl Value {
    /// Flare の型名を返す。`type_of(v)` および `v.type_of()` に使う。
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Int(_) => "Int",
            Self::Str(_) => "Str",
            Self::Bytes(_) => "Bytes",
            Self::Bool(_) => "Bool",
            Self::Nil => "Nil",
            Self::Array(_) => "Array",
            Self::Map(_) => "Map",
            Self::Timestamp(_) => "Timestamp",
            Self::ServerTimestamp => "ServerTimestamp",
            Self::ArrayUnion(_) => "ArrayUnion",
            Self::ArrayRemove(_) => "ArrayRemove",
            Self::DeleteField => "DeleteField",
            Self::Increment(_) => "Increment",
            Self::Opaque(_) => "Opaque",
            Self::AuthSession(_) => "AuthSession",
            Self::RedactedToken(_) => "RedactedToken",
            Self::Module(_) => "Module",
            Self::DocRef(_) => "DocRef",
            Self::CollectionRef(_) => "CollectionRef",
            Self::CollectionGroupRef(_) => "CollectionGroupRef",
            Self::QueryRef(_) => "QueryRef",
            Self::Func(_) => "Function",
        }
    }

    /// Ruby の `inspect` 風の開発者向け表現。`fl` の最終結果・複合値の
    /// `to_s` に使う。Str はダブルクォート付き、それ以外は素直な文字列表現。
    ///
    /// 再帰呼び出しではなく明示的なヒープスタックで走査するため、
    /// ネイティブスタックを消費せず、実行時設定された深いネスト値も切り捨てずに
    /// 安全にフォーマットできます。深さ制限を設けて省略表示したい場合は
    /// [`Value::inspect_with_depth`] を使用してください。
    pub fn inspect(&self) -> String {
        self.inspect_with_depth(usize::MAX)
    }

    /// `inspect` with a configurable nesting depth limit.
    ///
    /// Compound values (Array, Map, ArrayUnion, ArrayRemove) that reach `max_depth`
    /// truncate their contents to `[...]` or `{...}`. The traversal is iterative
    /// on the heap to prevent stack overflow even on deeply nested values.
    pub fn inspect_with_depth(&self, max_depth: usize) -> String {
        let mut out = String::new();
        let mut stack = vec![InspectAction::Value(self, 0)];

        while let Some(action) = stack.pop() {
            match action {
                InspectAction::Str(s) => out.push_str(s),
                InspectAction::String(s) => out.push_str(&s),
                InspectAction::Value(val, depth) => {
                    if format_leaf_value(val, &mut out) {
                        continue;
                    }
                    match val {
                        Value::Array(items) => {
                            if items.is_empty() {
                                out.push_str("[]");
                            } else if depth >= max_depth {
                                out.push_str("[...]");
                            } else {
                                out.push('[');
                                stack.push(InspectAction::Str("]"));
                                for (i, item) in items.iter().enumerate().rev() {
                                    if i + 1 < items.len() {
                                        stack.push(InspectAction::Str(", "));
                                    }
                                    stack.push(InspectAction::Value(item, depth + 1));
                                }
                            }
                        }
                        Value::Map(m) => {
                            if m.is_empty() {
                                out.push_str("{}");
                            } else if depth >= max_depth {
                                out.push_str("{...}");
                            } else {
                                out.push('{');
                                stack.push(InspectAction::Str("}"));
                                for (i, (k, v)) in m.iter().enumerate().rev() {
                                    if i + 1 < m.len() {
                                        stack.push(InspectAction::Str(", "));
                                    }
                                    stack.push(InspectAction::Value(v, depth + 1));
                                    stack.push(InspectAction::Str(" => "));
                                    stack.push(InspectAction::String(inspect_str(k)));
                                }
                            }
                        }
                        Value::ArrayUnion(items) => {
                            if items.is_empty() {
                                out.push_str("ArrayUnion([])");
                            } else if depth >= max_depth {
                                out.push_str("ArrayUnion([...])");
                            } else {
                                out.push_str("ArrayUnion([");
                                stack.push(InspectAction::Str("])"));
                                for (i, item) in items.iter().enumerate().rev() {
                                    if i + 1 < items.len() {
                                        stack.push(InspectAction::Str(", "));
                                    }
                                    stack.push(InspectAction::Value(item, depth + 1));
                                }
                            }
                        }
                        Value::ArrayRemove(items) => {
                            if items.is_empty() {
                                out.push_str("ArrayRemove([])");
                            } else if depth >= max_depth {
                                out.push_str("ArrayRemove([...])");
                            } else {
                                out.push_str("ArrayRemove([");
                                stack.push(InspectAction::Str("])"));
                                for (i, item) in items.iter().enumerate().rev() {
                                    if i + 1 < items.len() {
                                        stack.push(InspectAction::Str(", "));
                                    }
                                    stack.push(InspectAction::Value(item, depth + 1));
                                }
                            }
                        }
                        Value::AuthSession(session) => {
                            out.push_str("AuthSession(user=");
                            stack.push(InspectAction::String(format!(
                                ", id_token=<redacted>, refresh_token=<redacted>, expires_at={})",
                                session.expires_at
                            )));
                            stack.push(InspectAction::OwnedValue(
                                Box::new(session.user.to_value()),
                                depth + 1,
                            ));
                        }
                        _ => unreachable!(),
                    }
                }
                InspectAction::OwnedValue(val, depth) => {
                    if format_leaf_value(&val, &mut out) {
                        continue;
                    }
                    match *val {
                        Value::Array(items) => {
                            if items.is_empty() {
                                out.push_str("[]");
                            } else if depth >= max_depth {
                                out.push_str("[...]");
                            } else {
                                out.push('[');
                                stack.push(InspectAction::Str("]"));
                                let len = items.len();
                                for (i, item) in items.into_iter().enumerate().rev() {
                                    if i + 1 < len {
                                        stack.push(InspectAction::Str(", "));
                                    }
                                    stack
                                        .push(InspectAction::OwnedValue(Box::new(item), depth + 1));
                                }
                            }
                        }
                        Value::Map(m) => {
                            if m.is_empty() {
                                out.push_str("{}");
                            } else if depth >= max_depth {
                                out.push_str("{...}");
                            } else {
                                out.push('{');
                                stack.push(InspectAction::Str("}"));
                                let len = m.len();
                                for (i, (k, v)) in m.into_iter().enumerate().rev() {
                                    if i + 1 < len {
                                        stack.push(InspectAction::Str(", "));
                                    }
                                    stack.push(InspectAction::OwnedValue(Box::new(v), depth + 1));
                                    stack.push(InspectAction::Str(" => "));
                                    stack.push(InspectAction::String(inspect_str(&k)));
                                }
                            }
                        }
                        Value::ArrayUnion(items) => {
                            if items.is_empty() {
                                out.push_str("ArrayUnion([])");
                            } else if depth >= max_depth {
                                out.push_str("ArrayUnion([...])");
                            } else {
                                out.push_str("ArrayUnion([");
                                stack.push(InspectAction::Str("])"));
                                let len = items.len();
                                for (i, item) in items.into_iter().enumerate().rev() {
                                    if i + 1 < len {
                                        stack.push(InspectAction::Str(", "));
                                    }
                                    stack
                                        .push(InspectAction::OwnedValue(Box::new(item), depth + 1));
                                }
                            }
                        }
                        Value::ArrayRemove(items) => {
                            if items.is_empty() {
                                out.push_str("ArrayRemove([])");
                            } else if depth >= max_depth {
                                out.push_str("ArrayRemove([...])");
                            } else {
                                out.push_str("ArrayRemove([");
                                stack.push(InspectAction::Str("])"));
                                let len = items.len();
                                for (i, item) in items.into_iter().enumerate().rev() {
                                    if i + 1 < len {
                                        stack.push(InspectAction::Str(", "));
                                    }
                                    stack
                                        .push(InspectAction::OwnedValue(Box::new(item), depth + 1));
                                }
                            }
                        }
                        Value::AuthSession(session) => {
                            out.push_str("AuthSession(user=");
                            stack.push(InspectAction::String(format!(
                                ", id_token=<redacted>, refresh_token=<redacted>, expires_at={})",
                                session.expires_at
                            )));
                            stack.push(InspectAction::OwnedValue(
                                Box::new(session.user.to_value()),
                                depth + 1,
                            ));
                        }
                        _ => unreachable!(),
                    }
                }
            }
        }

        out
    }
}

/// Drop a `Value` iteratively using an explicit heap stack, avoiding native stack
/// overflow when dropping deeply nested structures.
pub fn drop_value_iteratively(value: Value) {
    let mut stack = vec![value];
    while let Some(val) = stack.pop() {
        match val {
            Value::Array(mut items) => {
                while let Some(child) = items.pop() {
                    stack.push(child);
                }
            }
            Value::Map(mut m) => {
                while let Some((_, child)) = m.pop_first() {
                    stack.push(child);
                }
            }
            Value::ArrayUnion(mut items) | Value::ArrayRemove(mut items) => {
                while let Some(child) = items.pop() {
                    stack.push(child);
                }
            }
            Value::QueryRef(mut q) => {
                for filter in std::mem::take(&mut q.filters) {
                    stack.push(filter.value);
                }
            }
            _ => {}
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn inspect_truncates_at_max_depth() {
        let nested_array =
            Value::Array(vec![Value::Array(vec![Value::Array(vec![Value::Int(1)])])]);
        assert_eq!(nested_array.inspect_with_depth(2), "[[[...]]]");
        assert_eq!(nested_array.inspect_with_depth(1), "[[...]]");
        assert_eq!(nested_array.inspect_with_depth(0), "[...]");
        assert_eq!(nested_array.inspect_with_depth(3), "[[[1]]]");

        let mut inner_map = BTreeMap::new();
        inner_map.insert("b".to_string(), Value::Int(2));
        let mut outer_map = BTreeMap::new();
        outer_map.insert("a".to_string(), Value::Map(inner_map));
        let map_val = Value::Map(outer_map);
        assert_eq!(map_val.inspect_with_depth(1), "{\"a\" => {...}}");
        assert_eq!(map_val.inspect_with_depth(2), "{\"a\" => {\"b\" => 2}}");

        let union_val = Value::ArrayUnion(vec![Value::Array(vec![Value::Int(1)])]);
        assert_eq!(union_val.inspect_with_depth(1), "ArrayUnion([[...]])");

        let remove_val = Value::ArrayRemove(vec![Value::Array(vec![Value::Int(1)])]);
        assert_eq!(remove_val.inspect_with_depth(1), "ArrayRemove([[...]])");
    }

    #[test]
    fn inspect_empty_containers_are_not_truncated() {
        let empty_arr = Value::Array(vec![Value::Array(vec![])]);
        assert_eq!(empty_arr.inspect_with_depth(1), "[[]]");

        let mut outer_map = BTreeMap::new();
        outer_map.insert("a".to_string(), Value::Map(BTreeMap::new()));
        let empty_map = Value::Map(outer_map);
        assert_eq!(empty_map.inspect_with_depth(1), "{\"a\" => {}}");
    }

    #[test]
    fn inspect_does_not_truncate_accepted_values_beyond_default_depth() {
        // A value 70 levels deep (allowed when runtime limit is configured higher or unlimited)
        // should not be truncated by inspect(), keeping full-output behavior.
        let mut val = Value::Int(1);
        for _ in 0..70 {
            val = Value::Array(vec![val]);
        }
        let inspected = val.inspect();
        assert!(!inspected.contains("[...]"));
        assert!(inspected.contains("1"));
        assert_eq!(inspected.len(), 141); // 70 * '[' + '1' + 70 * ']'

        // But inspect_with_depth can still truncate when explicitly requested.
        let truncated = val.inspect_with_depth(DEFAULT_MAX_INSPECT_DEPTH);
        assert!(truncated.contains("[...]"));
    }

    #[test]
    fn deeply_nested_values_inspect_and_drop_without_stack_overflow() {
        // Construct deeply nested values (10,000 levels deep) which would
        // overflow the native thread stack if inspect() or drop were recursive.
        let mut deep_array = Value::Int(42);
        for _ in 0..10_000 {
            deep_array = Value::Array(vec![deep_array]);
        }

        let inspected = deep_array.inspect();
        assert!(inspected.starts_with("[[[[[[["));
        assert!(inspected.ends_with("]]]]]]]"));
        assert!(inspected.contains("42"));
        assert!(!inspected.contains("[...]"));

        let truncated = deep_array.inspect_with_depth(DEFAULT_MAX_INSPECT_DEPTH);
        assert!(truncated.contains("[...]"));

        drop_value_iteratively(deep_array);

        let mut deep_map = Value::Int(100);
        for i in 0..10_000 {
            let mut m = BTreeMap::new();
            m.insert(format!("k{i}"), deep_map);
            deep_map = Value::Map(m);
        }

        let map_inspected = deep_map.inspect();
        assert!(map_inspected.contains("100"));
        assert!(!map_inspected.contains("{...}"));

        let truncated_map = deep_map.inspect_with_depth(DEFAULT_MAX_INSPECT_DEPTH);
        assert!(truncated_map.contains("{...}"));

        drop_value_iteratively(deep_map);

        let mut deep_in_query = Value::Int(1);
        for _ in 0..10_000 {
            deep_in_query = Value::Array(vec![deep_in_query]);
        }
        let query_val = Value::QueryRef(QueryRef {
            source: QuerySource::Collection("users".into()),
            filters: vec![QueryFilter {
                field: "tags".into(),
                op: QueryOp::In,
                value: deep_in_query,
            }],
            order_by: vec![],
            offset: None,
            limit: None,
        });
        assert_eq!(query_val.inspect(), "#<QueryRef>");
        drop_value_iteratively(query_val);
    }
}
