//! Firestore backend abstraction.

mod emulator;
mod memory;

pub use emulator::FirestoreEmulator;
pub use memory::InMemoryFirestore;

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::error::Error;

use crate::value::{Direction, QueryFilter, QueryOp, QueryRef, Value};

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
/// `run_query` が返す列には順序・offset/limit・`__name__` を適用しない。
pub type QueryCandidates = Vec<(String, BTreeMap<String, Value>)>;

/// クエリの決定的な順序とページングを候補へ適用する。
/// バックエンドが page 全体を処理できない場合の安全なフォールバックでも使う。
pub(crate) fn apply_query_shape(mut rows: QueryCandidates, query: &QueryRef) -> QueryCandidates {
    if query.order_by.is_empty() {
        rows.sort_by(|a, b| a.0.cmp(&b.0));
    } else {
        rows.retain(|(path, doc)| candidate_has_order_fields(path, doc, query));
        rows.sort_by(|a, b| compare_query_candidates(&a.0, &a.1, &b.0, &b.1, query));
    }
    let skip = query.offset.unwrap_or(0);
    let mut rows = rows.into_iter().skip(skip).collect::<Vec<_>>();
    if let Some(limit) = query.limit {
        rows.truncate(limit);
    }
    rows
}

pub(crate) fn page_borrowed_candidates<'a>(
    candidates: impl Iterator<Item = (&'a String, &'a BTreeMap<String, Value>)>,
    query: &QueryRef,
) -> Vec<(&'a String, &'a BTreeMap<String, Value>)> {
    let mut rows: Vec<_> = candidates.collect();
    if query.order_by.is_empty() {
        rows.sort_by(|a, b| a.0.cmp(b.0));
    } else {
        rows.retain(|(path, doc)| candidate_has_order_fields(path, doc, query));
        rows.sort_by(|a, b| compare_query_candidates(a.0, a.1, b.0, b.1, query));
    }
    let skip = query.offset.unwrap_or(0);
    let limit = query.limit.unwrap_or(usize::MAX);
    rows.into_iter().skip(skip).take(limit).collect()
}

/// 注文対象を持つ候補だけを順序付け・ページング対象へ残す。
pub(crate) fn candidate_has_order_fields(
    _path: &str,
    doc: &BTreeMap<String, Value>,
    query: &QueryRef,
) -> bool {
    query
        .order_by
        .iter()
        .all(|(field, _)| field == "__name__" || query_field_value(doc, field).is_some())
}

/// 共通比較器で 2 候補を比較する。
/// 値が同じ場合は常に相対ドキュメントパスの昇順で決める。
pub(crate) fn compare_query_candidates(
    a_path: &str,
    a_doc: &BTreeMap<String, Value>,
    b_path: &str,
    b_doc: &BTreeMap<String, Value>,
    query: &QueryRef,
) -> Ordering {
    for (field, direction) in &query.order_by {
        let mut ordering = if field == "__name__" {
            a_path.cmp(b_path)
        } else {
            let (Some(a), Some(b)) = (
                query_field_value(a_doc, field),
                query_field_value(b_doc, field),
            ) else {
                debug_assert!(
                    false,
                    "candidates missing order fields are filtered before sorting"
                );
                return a_path.cmp(b_path);
            };
            a.cmp_for_order(b)
        };
        if *direction == Direction::Desc {
            ordering = ordering.reverse();
        }
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    a_path.cmp(b_path)
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

pub trait Firestore {
    fn get(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>>;
    /// Apply writes atomically: either all writes persist or none do.
    fn apply(&mut self, writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>>;
    /// Returns filter-matching candidates as (relative path, document fields).
    /// Ordering, offset, and limit are NOT applied here. This method remains the
    /// unshaped compatibility path for Firestore implementations.
    fn run_query(&self, query: &QueryRef) -> Result<QueryCandidates, Box<dyn Error>>;

    /// Returns only the documents selected by the query's order, offset, and
    /// limit. Backends may implement this without cloning/materializing full
    /// document payloads for rows outside the page. The default is a safe client
    /// fallback over [`Firestore::run_query`].
    fn run_query_page(&self, query: &QueryRef) -> Result<QueryCandidates, Box<dyn Error>> {
        Ok(apply_query_shape(self.run_query(query)?, query))
    }

    /// Counts all documents matching the filters, ignoring order, offset, and
    /// limit. Backends may count without returning document payloads. The
    /// default is a safe fallback over [`Firestore::run_query`].
    fn count_query(&self, query: &QueryRef) -> Result<i64, Box<dyn Error>> {
        Ok(self.run_query(query)?.len() as i64)
    }

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
    use crate::value::QuerySource;

    struct FallbackFirestore;

    impl Firestore for FallbackFirestore {
        fn get(&self, _path: &str) -> Result<Option<Value>, Box<dyn Error>> {
            Ok(None)
        }

        fn apply(&mut self, _writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>> {
            Ok(())
        }

        fn run_query(&self, _query: &QueryRef) -> Result<QueryCandidates, Box<dyn Error>> {
            Ok(vec![
                (
                    "users/b".to_string(),
                    BTreeMap::from([("ord".to_string(), Value::Int(1))]),
                ),
                (
                    "users/c".to_string(),
                    BTreeMap::from([("ord".to_string(), Value::Int(2))]),
                ),
                (
                    "users/a".to_string(),
                    BTreeMap::from([("ord".to_string(), Value::Int(1))]),
                ),
            ])
        }

        fn begin_transaction(&mut self) -> Result<(), Box<dyn Error>> {
            Ok(())
        }

        fn transaction_get(&self, _path: &str) -> Result<Option<Value>, Box<dyn Error>> {
            Ok(None)
        }

        fn commit_transaction(&mut self, _writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>> {
            Ok(())
        }

        fn abort_transaction(&mut self) -> Result<(), Box<dyn Error>> {
            Ok(())
        }
    }

    #[test]
    fn default_page_and_count_methods_preserve_unshaped_query_semantics() {
        let firestore = FallbackFirestore;
        let query = QueryRef {
            source: QuerySource::Collection("users".to_string()),
            filters: vec![],
            order_by: vec![("ord".to_string(), Direction::Asc)],
            offset: Some(1),
            limit: Some(1),
        };
        let page = firestore.run_query_page(&query).unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].0, "users/b");
        assert_eq!(firestore.count_query(&query).unwrap(), 3);
    }
}
