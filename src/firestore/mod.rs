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
        check_value_size(&f.field, &f.value)?;
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

/// 順序比較に使う filter 値の上限(Firestore 準拠の 1,500 バイト)。
///
/// `Str` だけでなく `Bytes` にも同じ上限を課す。Firestore の順序比較は
/// 比較対象の先頭しか見ないため、上限なしの値を渡すと emulator と
/// in-memory の結果がずれる(igo memcmp は全文を見る)。
/// 順序比較に使う filter 値の上限(Firestore 準拠の 1,500 バイト)。
///
/// `Str` だけでなく `Bytes` にも同じ上限を課す。Firestore の順序比較は
/// 比較対象の先頭しか見ないため、上限なしの値を渡すと emulator と
/// in-memory の結果がずれる(in-memory は全文比較するため)。
const MAX_FILTER_VALUE_BYTES: usize = 1500;

fn check_value_size(field: &str, v: &Value) -> Result<(), Box<dyn Error>> {
    let too_large = matches!(v, Value::Str(s) if s.len() >= MAX_FILTER_VALUE_BYTES)
        || matches!(v, Value::Bytes(b) if b.len() >= MAX_FILTER_VALUE_BYTES);
    if too_large {
        return Err(format!("value for {field} is too large to be used in a query").into());
    }
    match v {
        Value::Array(items) => items.iter().try_for_each(|x| check_value_size(field, x)),
        _ => Ok(()),
    }
}

fn check_element_size(field: &str, v: &Value) -> Result<(), Box<dyn Error>> {
    check_value_size(field, v)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(op: QueryOp, value: Value) -> QueryFilter {
        QueryFilter {
            field: "b".to_string(),
            op,
            value,
        }
    }

    #[test]
    fn filter_values_are_bounded_for_str_and_bytes() {
        let big_bytes = Value::Bytes(vec![0u8; MAX_FILTER_VALUE_BYTES]);
        // ちょうど 1,500 バイトの Str / Bytes はどちらも却下する。
        let err = validate_query_filters(&[filter(QueryOp::Eq, Value::Str("x".repeat(1500)))])
            .unwrap_err();
        assert!(err.to_string().contains("too large"), "{err}");
        let err = validate_query_filters(&[filter(QueryOp::Eq, big_bytes)]).unwrap_err();
        assert!(err.to_string().contains("too large"), "{err}");
    }

    #[test]
    fn filter_values_below_the_bound_are_accepted() {
        let just_under = Value::Bytes(vec![0u8; MAX_FILTER_VALUE_BYTES - 1]);
        validate_query_filters(&[filter(QueryOp::Eq, just_under)]).unwrap();
    }

    #[test]
    fn element_size_check_applies_inside_in_operands() {
        // `in` の配列要素も同じ上限の対象になる。
        let operand = Value::Array(vec![
            Value::Int(1),
            Value::Bytes(vec![0u8; MAX_FILTER_VALUE_BYTES]),
        ]);
        let err = validate_query_filters(&[filter(QueryOp::In, operand)]).unwrap_err();
        assert!(err.to_string().contains("too large"), "{err}");
    }
}
