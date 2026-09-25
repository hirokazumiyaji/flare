//! Firestore backend abstraction.

#[cfg(feature = "emulator")]
mod emulator;
mod memory;

#[cfg(feature = "emulator")]
pub use emulator::{EmulatorConfig, EmulatorConnectionError, FirestoreEmulator};
pub use memory::InMemoryFirestore;

use std::collections::BTreeMap;
use std::error::Error;

use crate::value::{QueryFilter, QueryOp, QueryRef, Value};

const MAX_QUERY_STRING_BYTES: usize = 1500;

/// クエリフィルタ値の共有検証。in-memory / emulator のクエリ実行前に
/// 両バックエンドが呼び、同じエラーを同じメッセージで返す。
/// - 文字列フィルタ値は 1,500 UTF-8 バイト未満(Firestore 準拠。Map / Array
///   の中にネストされた文字列も含めて再帰的に検証する)
/// - `in` / `not-in` / `array_contains_any` の値は Array で要素は最大 10 個
pub(crate) fn validate_query_filters(filters: &[QueryFilter]) -> Result<(), Box<dyn Error>> {
    for f in filters {
        check_query_value_size(&f.field, &f.value)?;
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
            }
            _ => {}
        }
    }
    Ok(())
}

/// 値の中の文字列を葉としてたどり、各文字列がクエリの上限未満であることを
/// 確認する。Firestore の制限は文字数ではなく UTF-8 バイト数で判定される。
fn check_query_value_size(field: &str, value: &Value) -> Result<(), Box<dyn Error>> {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Value::Str(s) if s.len() >= MAX_QUERY_STRING_BYTES => {
                return Err(format!("value for {field} is too large to be used in a query").into());
            }
            Value::Array(items) => pending.extend(items.iter()),
            Value::Map(fields) => pending.extend(fields.values()),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map_array(value: &str) -> Value {
        Value::Map(BTreeMap::from([(
            "outer".to_string(),
            Value::Array(vec![Value::Map(BTreeMap::from([(
                "inner".to_string(),
                Value::Str(value.to_string()),
            )]))]),
        )]))
    }

    fn array_map(value: &str) -> Value {
        Value::Array(vec![Value::Map(BTreeMap::from([(
            "outer".to_string(),
            Value::Map(BTreeMap::from([(
                "inner".to_string(),
                Value::Str(value.to_string()),
            )])),
        )]))])
    }

    #[test]
    fn query_filter_string_limit_is_recursive_and_byte_based() {
        let valid = "界".repeat(499) + "ab";
        let invalid = "界".repeat(500);
        assert_eq!(valid.len(), 1499);
        assert_eq!(invalid.len(), 1500);

        for make_value in [map_array as fn(&str) -> Value, array_map] {
            assert!(validate_query_filters(&[QueryFilter {
                field: "payload".to_string(),
                op: QueryOp::Eq,
                value: make_value(&valid),
            }])
            .is_ok());

            let err = validate_query_filters(&[QueryFilter {
                field: "payload".to_string(),
                op: QueryOp::Eq,
                value: make_value(&invalid),
            }])
            .unwrap_err();
            assert_eq!(
                err.to_string(),
                "value for payload is too large to be used in a query"
            );
        }
    }
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
    /// Commit writes using the current transaction. A failed preparation or
    /// commit must leave the transaction available to `abort_transaction`;
    /// successful commit closes it.
    fn commit_transaction(&mut self, writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>>;
    /// Rollback an open transaction. Idempotent: succeeds when no
    /// transaction is open. A failed rollback must leave the transaction
    /// available for a retry; a successful rollback closes it. Failures are
    /// returned so callers can decide; `Runtime` ignores them to preserve
    /// the original block or commit error.
    fn abort_transaction(&mut self) -> Result<(), Box<dyn Error>>;
}
