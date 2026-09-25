//! Firestore backend abstraction.

mod emulator;
mod memory;

pub use emulator::FirestoreEmulator;
pub use memory::InMemoryFirestore;

use std::collections::BTreeMap;
use std::error::Error;

use crate::value::{QueryFilter, QueryOp, QueryRef, Value};

/// クエリフィルタ値の共有検証。in-memory / emulator のクエリ実行前に
/// 両バックエンドが呼び、同じエラーを同じメッセージで返す。
/// - 文字列フィルタ値は 1,500 UTF-8 バイト未満(Firestore 準拠。`in` 等の
///   配列要素・`array_contains` の要素も含む)
/// - `in` / `not-in` / `array_contains_any` の値は Array で要素は最大 10 個
pub(crate) fn validate_query_filters(filters: &[QueryFilter]) -> Result<(), Box<dyn Error>> {
    for f in filters {
        if let Value::Str(s) = &f.value {
            if s.len() >= 1500 {
                return Err(
                    format!("value for {} is too large to be used in a query", f.field).into(),
                );
            }
        }
        match f.op {
            QueryOp::In | QueryOp::NotIn | QueryOp::ArrayContainsAny => {
                let op_name = match f.op {
                    QueryOp::In => "in",
                    QueryOp::NotIn => "not-in",
                    _ => "array_contains_any",
                };
                let Value::Array(items) = &f.value else {
                    return Err(format!("{op_name} filter requires an array value").into());
                };
                if items.len() > 10 {
                    return Err(format!("{op_name} filters support at most 10 elements").into());
                }
                for item in items {
                    check_element_size(&f.field, item)?;
                }
            }
            QueryOp::ArrayContains => {
                check_element_size(&f.field, &f.value)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn check_element_size(field: &str, v: &Value) -> Result<(), Box<dyn Error>> {
    match v {
        Value::Str(s) if s.len() >= 1500 => {
            Err(format!("value for {field} is too large to be used in a query").into())
        }
        Value::Array(items) => items.iter().try_for_each(|x| check_element_size(field, x)),
        _ => Ok(()),
    }
}

/// Apply the firelang query semantics to one decoded document.
///
/// The emulator intentionally re-evaluates every filter after fetching
/// candidates.  In particular, Firestore's `!=`/`not-in` and its handling of
/// null values are not identical to firelang's structural semantics.  Keeping
/// this matcher shared prevents the two backends from drifting again.
pub(crate) fn matches_filters_with_name(
    doc: &BTreeMap<String, Value>,
    path: &str,
    filters: &[QueryFilter],
) -> bool {
    filters.iter().all(|f| {
        if f.field == "__name__" {
            return matches_document_name_filter(path, f);
        }
        // `!=` / `in` / `not-in` require the field to exist.  `order_by`'s
        // presence check is applied later by the runtime.
        let actual = lookup_field_path_split(Some(doc), &f.field);
        match f.op {
            QueryOp::Eq => actual == Some(&f.value),
            QueryOp::Ne => actual.is_some_and(|v| v != &f.value),
            QueryOp::Gt => compare_values(actual, &f.value, |o| o.is_gt()),
            QueryOp::Ge => compare_values(actual, &f.value, |o| o.is_ge()),
            QueryOp::Lt => compare_values(actual, &f.value, |o| o.is_lt()),
            QueryOp::Le => compare_values(actual, &f.value, |o| o.is_le()),
            QueryOp::In => match (&f.value, actual) {
                (Value::Array(list), Some(v)) => list.contains(v),
                _ => false,
            },
            QueryOp::NotIn => match (&f.value, actual) {
                (Value::Array(list), Some(v)) => {
                    if list.contains(&Value::Nil) {
                        false
                    } else {
                        !list.contains(v)
                    }
                }
                _ => false,
            },
            QueryOp::ArrayContains => match actual {
                Some(Value::Array(arr)) => arr.contains(&f.value),
                _ => false,
            },
            QueryOp::ArrayContainsAny => match (&f.value, actual) {
                (Value::Array(list), Some(Value::Array(arr))) => {
                    arr.iter().any(|x| list.contains(x))
                }
                _ => false,
            },
        }
    })
}

fn matches_document_name_filter(path: &str, filter: &QueryFilter) -> bool {
    let is_path = |value: &Value| matches!(value, Value::Str(actual) if actual == path);
    match filter.op {
        QueryOp::Eq => is_path(&filter.value),
        QueryOp::Ne => !is_path(&filter.value),
        QueryOp::In => match &filter.value {
            Value::Array(values) => values.iter().any(is_path),
            _ => false,
        },
        QueryOp::NotIn => match &filter.value {
            Value::Array(values) if !values.iter().any(|value| matches!(value, Value::Nil)) => {
                !values.iter().any(is_path)
            }
            _ => false,
        },
        QueryOp::ArrayContains | QueryOp::ArrayContainsAny => false,
        QueryOp::Gt | QueryOp::Ge | QueryOp::Lt | QueryOp::Le => false,
    }
}

/// Compare only same-typed scalar values.  Firestore's type ordering is not
/// part of firelang's query contract (spec §13), so a different type never
/// satisfies a range predicate.
fn compare_values(a: Option<&Value>, b: &Value, pred: impl Fn(std::cmp::Ordering) -> bool) -> bool {
    match (a, b) {
        (Some(Value::Int(x)), Value::Int(y)) => pred(x.cmp(y)),
        (Some(Value::Str(x)), Value::Str(y)) => pred(x.cmp(y)),
        (Some(Value::Bool(x)), Value::Bool(y)) => pred(x.cmp(y)),
        (Some(Value::Timestamp(x)), Value::Timestamp(y)) => pred(x.seconds.cmp(&y.seconds)),
        _ => false,
    }
}

/// Look up a dot-separated field path.  A missing path and a non-map
/// intermediate node are both treated as a missing field.
pub(crate) fn lookup_field_path<'a>(
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

/// Look up a field path without allocating a segment vector per document.
pub(crate) fn lookup_field_path_split<'a>(
    map: Option<&'a BTreeMap<String, Value>>,
    field: &str,
) -> Option<&'a Value> {
    let mut current = map?;
    let mut segments = field.split('.').peekable();
    while let Some(segment) = segments.next() {
        let value = current.get(segment)?;
        if segments.peek().is_none() {
            return Some(value);
        }
        match value {
            Value::Map(map) => current = map,
            _ => return None,
        }
    }
    None
}

pub(crate) fn paths_overlap(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    if let Some(rest) = b.strip_prefix(a) {
        if rest.starts_with('.') {
            return true;
        }
    }
    if let Some(rest) = a.strip_prefix(b) {
        if rest.starts_with('.') {
            return true;
        }
    }
    false
}

pub(crate) fn validate_no_overlapping_paths<'a>(
    paths: impl IntoIterator<Item = &'a str>,
) -> Result<(), Box<dyn Error>> {
    let mut vec: Vec<&str> = paths.into_iter().collect();
    vec.sort();
    for i in 0..vec.len() {
        for j in (i + 1)..vec.len() {
            if paths_overlap(vec[i], vec[j]) {
                return Err(format!("overlapping field paths: {} and {}", vec[i], vec[j]).into());
            }
        }
    }
    Ok(())
}

/// update / merge set のキーは `.` 区切りのフィールドパス(Firestore 準拠)。
/// 各セグメントは Firestore の単純フィールドパス文法
/// (`[A-Za-z_][A-Za-z0-9_]*`)に従うこと。バッククォートによるエスケープは
/// 非対応なので、バッククォートや空セグメントを含むキーはエラーにする。
pub(crate) fn validate_field_path(key: &str) -> Result<(), Box<dyn Error>> {
    for segment in key.split('.') {
        let valid = match segment.chars().next() {
            Some(c) => c.is_ascii_alphabetic() || c == '_',
            None => false,
        } && segment
            .chars()
            .skip(1)
            .all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            return Err(format!(
                "invalid field path: {key} (segment {segment:?} must match [A-Za-z_][A-Za-z0-9_]*)"
            )
            .into());
        }
    }
    Ok(())
}

/// フィールドパスに沿って `map` に値を挿入する。中間の Map は必要なら作成し、
/// 中間が Map でなければエラーにする。
pub(crate) fn insert_at_field_path(
    map: &mut BTreeMap<String, Value>,
    segments: &[&str],
    value: Value,
) -> Result<(), Box<dyn Error>> {
    let Some((first, rest)) = segments.split_first() else {
        return Err("empty field path".into());
    };
    if rest.is_empty() {
        map.insert(first.to_string(), value);
        return Ok(());
    }
    if !map.contains_key(*first) {
        map.insert(first.to_string(), Value::Map(BTreeMap::new()));
    }
    match map.get_mut(*first).expect("just inserted") {
        Value::Map(inner) => insert_at_field_path(inner, rest, value),
        _ => Err(format!("cannot update field path: field {first:?} is not a map").into()),
    }
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

/// クエリ候補: フィルタに一致した (相対ドキュメントパス, フィールド) の列。
/// 順序・offset/limit・`__name__` は未適用(runtime が付ける)。
pub type QueryCandidates = Vec<(String, BTreeMap<String, Value>)>;

pub trait Firestore {
    fn get(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>>;
    /// Apply writes atomically: either all writes persist or none do.
    fn apply(&mut self, writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>>;
    /// Returns filter-matching candidates as (relative path, document fields).
    /// Ordering, offset/limit, and `__name__` are NOT applied here; the runtime
    /// applies them uniformly for both backends (spec §7).
    fn run_query(&self, query: &QueryRef) -> Result<QueryCandidates, Box<dyn Error>>;

    fn begin_transaction(&mut self) -> Result<(), Box<dyn Error>>;
    fn transaction_get(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>>;
    fn commit_transaction(&mut self, writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>>;
    /// Rollback an open transaction. Idempotent: succeeds when no
    /// transaction is open. Failures are returned so callers can decide;
    /// `Runtime` ignores them to preserve the original block error.
    fn abort_transaction(&mut self) -> Result<(), Box<dyn Error>>;
}
