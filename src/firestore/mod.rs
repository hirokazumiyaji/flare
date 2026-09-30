//! Firestore backend abstraction.

mod emulator;
mod memory;

pub use emulator::FirestoreEmulator;
pub use memory::InMemoryFirestore;

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

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

/// Marker error used when a backend would materialize more query candidates
/// than the runtime's query budget permits.
///
/// A distinct type (rather than a formatted `String`) lets the runtime tell a
/// budget failure apart from a genuine backend failure, so it can record the
/// budget that was hit and still report the failure as a *runtime* error with a
/// source position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QueryResultLimitExceeded {
    pub(crate) max_results: usize,
}

impl fmt::Display for QueryResultLimitExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "query result limit exceeded (max {})", self.max_results)
    }
}

impl Error for QueryResultLimitExceeded {}

/// Error used when a backend would materialize more query candidates than
/// the runtime's query budget permits.
pub(crate) fn query_result_limit_error(max_results: usize) -> Box<dyn Error> {
    Box::new(QueryResultLimitExceeded { max_results })
}

pub trait Firestore {
    fn get(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>>;
    /// Apply writes atomically: either all writes persist or none do.
    fn apply(&mut self, writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>>;
    /// Returns filter-matching candidates as (relative path, document fields).
    /// Ordering, offset/limit, and `__name__` are NOT applied here; the runtime
    /// applies them uniformly for both backends (spec §7).
    fn run_query(&self, query: &QueryRef) -> Result<QueryCandidates, Box<dyn Error>>;

    /// Run a query while refusing to return more than `max_results` matching
    /// candidates.
    ///
    /// The default is a compatibility fallback: it delegates to `run_query`
    /// and then rejects an oversized result, so existing third-party
    /// implementations of [`Firestore`] keep compiling. That bounds the value
    /// handed to the runtime, but *not* peak memory during collection. A
    /// backend that must bound memory has to override this and stop before the
    /// first extra candidate is materialized.
    fn run_query_bounded(
        &self,
        query: &QueryRef,
        max_results: usize,
    ) -> Result<QueryCandidates, Box<dyn Error>> {
        let rows = self.run_query(query)?;
        if rows.len() > max_results {
            return Err(query_result_limit_error(max_results));
        }
        Ok(rows)
    }

    /// Configure a per-call timeout for backends that support one.
    ///
    /// The default is intentionally a no-op so existing third-party
    /// implementations remain source-compatible. Such an implementation
    /// cannot be forcibly interrupted by a synchronous trait call; it should
    /// override this hook when it has a cancellable I/O path. The built-in
    /// emulator overrides it and applies the deadline to every RPC.
    fn set_timeout(&mut self, _timeout: Option<std::time::Duration>) {}

    /// Configure the maximum `Value` nesting depth this backend may recurse
    /// through while encoding or decoding.
    ///
    /// Backends that convert values recursively (the built-in emulator codec)
    /// must bound that recursion, otherwise a deeply nested document overflows
    /// the native stack before the runtime can inspect the result. The default
    /// is a no-op for source compatibility.
    fn set_max_value_depth(&mut self, _max_depth: usize) {}

    /// Whether this backend actually enforces the configured timeout.
    ///
    /// This is a capability query, not another limit. It lets embedders
    /// distinguish the built-in adapter from a legacy synchronous backend
    /// that must retain the compatibility no-op above.
    fn supports_backend_timeout(&self) -> bool {
        false
    }

    fn begin_transaction(&mut self) -> Result<(), Box<dyn Error>>;
    fn transaction_get(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>>;
    fn commit_transaction(&mut self, writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>>;
    /// Rollback an open transaction. Idempotent: succeeds when no
    /// transaction is open. Failures are returned so callers can decide;
    /// `Runtime` ignores them to preserve the original block error.
    fn abort_transaction(&mut self) -> Result<(), Box<dyn Error>>;
}
