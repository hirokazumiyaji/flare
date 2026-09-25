//! Firestore backend abstraction.

mod emulator;
mod memory;

pub use emulator::FirestoreEmulator;
pub use memory::InMemoryFirestore;

use std::collections::BTreeMap;
use std::error::Error;

use crate::value::{QueryFilter, QueryOp, QueryRef, Value};

/// A parsed query field path.
///
/// Query field paths are user-facing strings, while the emulator sends a
/// Firestore `FieldReference.field_path` string.  Keeping the decoded segments
/// and the canonical wire spelling together prevents the two backends from
/// interpreting quoted names, nested names, and the document-name pseudo-field
/// differently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QueryFieldPath {
    segments: Vec<String>,
    wire: String,
    document_id: bool,
}

impl QueryFieldPath {
    /// Parse a query field path and produce its canonical Firestore spelling.
    ///
    /// A dot always separates path segments.  A segment can be written as a
    /// simple name (`[A-Za-z_][A-Za-z0-9_]*`) or quoted with backticks.  A
    /// non-simple, non-empty unquoted segment is retained as a literal field
    /// name and is quoted in the wire representation.  This is intentional:
    /// `a-b` was already a valid in-memory field lookup, so silently turning it
    /// into a nested/invalid path would be a compatibility break.  Quoting is
    /// required when a segment contains a dot (for example `` `a.b` ``).
    pub(crate) fn parse(input: &str) -> Result<Self, Box<dyn Error>> {
        if input.is_empty() {
            return Err(invalid_query_field_path(input, "empty field path"));
        }

        let chars: Vec<char> = input.chars().collect();
        let mut segments = Vec::new();
        let mut quoted = Vec::new();
        let mut index = 0;

        while index < chars.len() {
            let (segment, was_quoted, next) = if chars[index] == '`' {
                parse_quoted_segment(input, &chars, index)?
            } else {
                let start = index;
                while index < chars.len() && chars[index] != '.' {
                    if chars[index] == '`' {
                        return Err(invalid_query_field_path(
                            input,
                            "backtick must delimit a complete segment",
                        ));
                    }
                    index += 1;
                }
                (chars[start..index].iter().collect::<String>(), false, index)
            };
            index = next;
            if segment.is_empty() {
                return Err(invalid_query_field_path(input, "empty field segment"));
            }
            segments.push(segment);
            quoted.push(was_quoted);

            if index == chars.len() {
                break;
            }
            if chars[index] != '.' {
                return Err(invalid_query_field_path(
                    input,
                    "quoted segment must be followed by '.' or end of path",
                ));
            }
            index += 1;
            if index == chars.len() {
                return Err(invalid_query_field_path(input, "empty field segment"));
            }
        }

        let document_id = segments.len() == 1 && segments[0] == "__name__" && !quoted[0];
        if segments.iter().any(|segment| segment == "__name__") && !document_id {
            return Err(invalid_query_field_path(
                input,
                "__name__ is a document-name pseudo-field and cannot be nested",
            ));
        }
        if !document_id && quoted[0] && segments[0] == "__name__" {
            return Err(invalid_query_field_path(
                input,
                "__name__ is a reserved document-name pseudo-field",
            ));
        }

        let wire = segments
            .iter()
            .map(|segment| quote_wire_segment(segment))
            .collect::<Vec<_>>()
            .join(".");
        Ok(Self {
            segments,
            wire,
            document_id,
        })
    }

    pub(crate) fn wire(&self) -> &str {
        &self.wire
    }

    pub(crate) fn is_document_id(&self) -> bool {
        self.document_id
    }

    /// Look up the path in a decoded document. `__name__` is handled by the
    /// caller because its value comes from the candidate path, not the map.
    pub(crate) fn lookup<'a>(&self, doc: &'a BTreeMap<String, Value>) -> Option<&'a Value> {
        let mut current = doc;
        for (index, segment) in self.segments.iter().enumerate() {
            let value = current.get(segment)?;
            if index + 1 == self.segments.len() {
                return Some(value);
            }
            let Value::Map(next) = value else {
                return None;
            };
            current = next;
        }
        None
    }
}

/// Return the canonical spelling used by the Firestore RPC layer.
pub(crate) fn normalize_query_field_path(path: &str) -> Result<String, Box<dyn Error>> {
    Ok(parse_query_field_path(path)?.wire)
}

/// Parse a query field path while retaining its decoded segments.
pub(crate) fn parse_query_field_path(path: &str) -> Result<QueryFieldPath, Box<dyn Error>> {
    QueryFieldPath::parse(path)
}

/// Normalize a value used for a `__name__` filter to the relative path shape
/// used by the in-memory candidate matcher.  A DocRef is accepted as a
/// convenience and is equivalent to its relative path string.
pub(crate) fn normalize_document_id_filter_value(value: &Value) -> Value {
    match value {
        Value::DocRef(reference) => Value::Str(reference.path.clone()),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(normalize_document_id_filter_value)
                .collect(),
        ),
        Value::Map(map) => Value::Map(
            map.iter()
                .map(|(key, value)| (key.clone(), normalize_document_id_filter_value(value)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn parse_quoted_segment(
    input: &str,
    chars: &[char],
    mut index: usize,
) -> Result<(String, bool, usize), Box<dyn Error>> {
    // `index` points at the opening backtick.
    index += 1;
    let mut segment = String::new();
    let mut closed = false;
    while index < chars.len() {
        match chars[index] {
            '`' => {
                index += 1;
                closed = true;
                break;
            }
            '\\' if index + 1 < chars.len() => {
                // Firestore quotes use a backslash to escape a backtick.  We
                // also accept an escaped backslash so the decoder is lossless.
                segment.push(chars[index + 1]);
                index += 2;
            }
            other => {
                segment.push(other);
                index += 1;
            }
        }
    }
    if !closed {
        return Err(invalid_query_field_path(
            input,
            "unterminated quoted field segment",
        ));
    }
    Ok((segment, true, index))
}

fn quote_wire_segment(segment: &str) -> String {
    if is_simple_query_segment(segment) {
        return segment.to_string();
    }
    let mut quoted = String::with_capacity(segment.len() + 2);
    quoted.push('`');
    for ch in segment.chars() {
        if ch == '`' || ch == '\\' {
            quoted.push('\\');
        }
        quoted.push(ch);
    }
    quoted.push('`');
    quoted
}

fn is_simple_query_segment(segment: &str) -> bool {
    let mut chars = segment.chars();
    matches!(chars.next(), Some(ch) if ch.is_ascii_alphabetic() || ch == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn invalid_query_field_path(path: &str, reason: &str) -> Box<dyn Error> {
    format!("invalid field path: {path} ({reason})").into()
}

/// クエリフィルタ値の共有検証。in-memory / emulator のクエリ実行前に
/// 両バックエンドが呼び、同じエラーを同じメッセージで返す。
/// - 文字列フィルタ値は 1,500 UTF-8 バイト未満(Firestore 準拠。`in` 等の
///   配列要素・`array_contains` の要素も含む)
/// - `in` / `not-in` / `array_contains_any` の値は Array で要素は最大 10 個
pub(crate) fn validate_query_filters(filters: &[QueryFilter]) -> Result<(), Box<dyn Error>> {
    for f in filters {
        if f.field == "__name__" {
            validate_document_id_filter_value(&f.value)?;
        }
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

fn validate_document_id_filter_value(value: &Value) -> Result<(), Box<dyn Error>> {
    match value {
        Value::Str(path) => validate_document_filter_path(path),
        Value::DocRef(reference) => validate_document_filter_path(&reference.path),
        Value::Array(items) => items.iter().try_for_each(validate_document_id_filter_value),
        Value::Map(map) => map.values().try_for_each(validate_document_id_filter_value),
        _ => Ok(()),
    }
}

fn validate_document_filter_path(path: &str) -> Result<(), Box<dyn Error>> {
    let segments: Vec<&str> = path.split('/').collect();
    if segments.is_empty() || !segments.len().is_multiple_of(2) {
        return Err(format!("invalid document path: {path}").into());
    }
    for (index, segment) in segments.iter().enumerate() {
        let reserved_document_id = index % 2 == 1
            && segment.len() >= 4
            && segment.starts_with("__")
            && segment.ends_with("__");
        if segment.is_empty() || *segment == "." || *segment == ".." || reserved_document_id {
            return Err(format!("invalid document path: {path}").into());
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

    #[test]
    fn query_field_path_normalizes_simple_quoted_and_nested_segments() {
        assert_eq!(
            normalize_query_field_path("profile.age").unwrap(),
            "profile.age"
        );
        assert_eq!(
            normalize_query_field_path("`profile.name`").unwrap(),
            "`profile.name`"
        );
        assert_eq!(normalize_query_field_path("a-b").unwrap(), "`a-b`");
        assert_eq!(normalize_query_field_path("`a\\`b`").unwrap(), "`a\\`b`");
    }

    #[test]
    fn query_field_path_rejects_empty_and_malformed_segments() {
        for path in ["", ".a", "a.", "a..b", "`a", "a`b", "`a`b"] {
            let err = normalize_query_field_path(path).unwrap_err();
            assert!(
                format!("{err}").contains("invalid field path"),
                "{path}: {err}"
            );
        }
    }

    #[test]
    fn document_name_is_a_distinct_query_path() {
        let path = parse_query_field_path("__name__").unwrap();
        assert!(path.is_document_id());
        assert_eq!(path.wire(), "__name__");
        assert!(parse_query_field_path("profile.__name__").is_err());
        assert!(parse_query_field_path("`__name__`").is_err());
    }
}
