//! Firestore backend abstraction.

mod emulator;
mod memory;

pub use emulator::FirestoreEmulator;
pub use memory::InMemoryFirestore;

use std::cmp::Ordering;
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

/// `where` の最終判定。in-memory と emulator がこの関数を共有し、
/// Firestore server の型順序や Opaque 値の扱いに依存しない。
///
/// 比較演算子は `Value::cmp_for_where` が規定する同型比較だけを受け付ける。
/// `order_by` の全順序 (`Value::cmp_for_order`) とは別の契約である。
pub(crate) fn matches_filters(
    path: &str,
    doc: &BTreeMap<String, Value>,
    filters: &[QueryFilter],
) -> bool {
    filters.iter().all(|filter| {
        // `__name__` は保存された field ではなく相対ドキュメントパス。
        if filter.field == "__name__" {
            return matches_document_name_filter(path, filter);
        }
        let actual = query_field_value(doc, &filter.field);
        match filter.op {
            QueryOp::Eq => actual == Some(&filter.value),
            QueryOp::Ne => actual.is_some_and(|value| value != &filter.value),
            QueryOp::Gt => compare_where_values(actual, &filter.value, Ordering::is_gt),
            QueryOp::Ge => compare_where_values(actual, &filter.value, Ordering::is_ge),
            QueryOp::Lt => compare_where_values(actual, &filter.value, Ordering::is_lt),
            QueryOp::Le => compare_where_values(actual, &filter.value, Ordering::is_le),
            QueryOp::In => match (&filter.value, actual) {
                (Value::Array(list), Some(value)) => list.contains(value),
                _ => false,
            },
            QueryOp::NotIn => match (&filter.value, actual) {
                (Value::Array(list), Some(value))
                    if !list.iter().any(|item| matches!(item, Value::Nil)) =>
                {
                    !list.contains(value)
                }
                _ => false,
            },
            QueryOp::ArrayContains => match actual {
                Some(Value::Array(items)) => items.contains(&filter.value),
                _ => false,
            },
            QueryOp::ArrayContainsAny => match (&filter.value, actual) {
                (Value::Array(candidates), Some(Value::Array(items))) => {
                    items.iter().any(|item| candidates.contains(item))
                }
                _ => false,
            },
        }
    })
}

fn query_field_value<'a>(doc: &'a BTreeMap<String, Value>, field: &str) -> Option<&'a Value> {
    let mut current = doc;
    let mut segments = field.split('.').peekable();
    while let Some(segment) = segments.next() {
        let value = current.get(segment)?;
        if segments.peek().is_none() {
            return Some(value);
        }
        current = match value {
            Value::Map(inner) => inner,
            _ => return None,
        };
    }
    None
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

fn compare_where_values(
    actual: Option<&Value>,
    filter: &Value,
    predicate: fn(std::cmp::Ordering) -> bool,
) -> bool {
    actual
        .and_then(|value| value.cmp_for_where(filter))
        .is_some_and(predicate)
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
