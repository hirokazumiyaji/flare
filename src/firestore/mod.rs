//! Firestore backend abstraction.

mod emulator;
mod memory;

pub use emulator::FirestoreEmulator;
pub use memory::InMemoryFirestore;

use std::collections::BTreeMap;
use std::error::Error;

use crate::value::{QueryFilter, QueryOp, QueryRef, Value};

/// Firestore のフィールド名の上限(UTF-8 バイト数)。
const MAX_FIELD_NAME_BYTES: usize = 1500;

/// Firestore に書き込む値の共有検証。
///
/// Firestore の `ArrayValue` は直接別の `ArrayValue` を含められない。ただし
/// Array の要素が Map で、その Map の値が Array になる間接的な入れ子は許可
/// される。Map を跨いだときは「配列の中にいる」状態を解除する。
pub(crate) fn validate_firestore_value(value: &Value) -> Result<(), Box<dyn Error>> {
    validate_firestore_value_at(value, false)
}

/// `value` を Array の要素として見たときの検証。配列の要素を 1 個ずつ
/// 検証する箇所から直接呼び出すことで、要素が Array であるものを検出できる。
pub(crate) fn validate_firestore_array_items(items: &[Value]) -> Result<(), Box<dyn Error>> {
    for item in items {
        validate_firestore_value_at(item, true)?;
    }
    Ok(())
}

fn validate_firestore_value_at(value: &Value, inside_array: bool) -> Result<(), Box<dyn Error>> {
    match value {
        Value::Array(items) => {
            if inside_array {
                return Err("Nested arrays are not allowed".into());
            }
            validate_firestore_array_items(items)
        }
        Value::Map(fields) => {
            for (name, value) in fields {
                validate_firestore_field_name(name)?;
                // Map は配列と配列の間に入る境界なので、配列内の Map に
                // 含まれる配列は直接入れ子ではなく許可する。
                validate_firestore_value_at(value, false)?;
            }
            Ok(())
        }
        // transform の引数も、適用後の Array の要素として検証する。
        Value::ArrayUnion(items) | Value::ArrayRemove(items) => {
            if inside_array {
                return Err("Nested arrays are not allowed".into());
            }
            validate_firestore_array_items(items)
        }
        _ => Ok(()),
    }
}

/// Firestore に保存できる値の型を検証する。query の filter operand と
/// write data は同じ Value 型を使うため、書き込み専用の型制約はここへ分ける。
pub(crate) fn validate_firestore_writable_value(value: &Value) -> Result<(), Box<dyn Error>> {
    match value {
        Value::Str(_)
        | Value::Int(_)
        | Value::Bool(_)
        | Value::Nil
        | Value::Timestamp(_)
        | Value::ServerTimestamp
        | Value::Increment(_)
        | Value::ArrayUnion(_)
        | Value::ArrayRemove(_)
        | Value::DeleteField => Ok(()),
        Value::Array(items) => {
            for item in items {
                validate_firestore_writable_value(item)?;
            }
            Ok(())
        }
        Value::Map(fields) => {
            for (name, value) in fields {
                validate_firestore_field_name(name)?;
                validate_firestore_writable_value(value)?;
            }
            Ok(())
        }
        _ => Err(format!("unsupported value in write: {}", value.inspect()).into()),
    }
}

/// ドキュメント / Map の全フィールドを、バックエンド共通で検証する。
/// フィールド名と値の両方を再帰的に検査し、フィールド transform の
/// ネストもここで両バックエンドに同じ規則を適用する。
#[cfg(test)]
pub(crate) fn validate_firestore_fields(
    fields: &BTreeMap<String, Value>,
) -> Result<(), Box<dyn Error>> {
    validate_firestore_fields_with_keys(fields, false)
}

fn validate_firestore_fields_with_keys(
    fields: &BTreeMap<String, Value>,
    keys_are_paths: bool,
) -> Result<(), Box<dyn Error>> {
    for (name, value) in fields {
        if keys_are_paths {
            validate_field_path(name)?;
        } else {
            validate_firestore_field_name(name)?;
        }
        reject_nested_sentinels(value, false)?;
        validate_firestore_value(value)?;
        validate_firestore_writable_value(value)?;
    }
    Ok(())
}

/// 1 つの `WriteOp` に対する検証 dispatch。in-memory と emulator は
/// 同じ helper を使い、variant を追加したときの検証漏れを防ぐ。
pub(crate) fn validate_write_op(op: &WriteOp) -> Result<(), Box<dyn Error>> {
    match op {
        WriteOp::Set { data, merge, .. } => {
            validate_firestore_fields_with_keys(data, *merge)?;
        }
        WriteOp::Update { data, .. } => validate_firestore_fields_with_keys(data, true)?,
        WriteOp::Delete { .. } => {}
    }
    Ok(())
}

/// Firestore の MapValue / Document.fields のキーとして使える名前か検証する。
/// 予約名は `__.*__`、空文字列は禁止。フィールド名は UTF-8 1,500 バイトまで。
pub(crate) fn validate_firestore_field_name(name: &str) -> Result<(), Box<dyn Error>> {
    if name.is_empty() {
        return Err("The property.name is the empty string.".into());
    }
    if name.len() > MAX_FIELD_NAME_BYTES {
        return Err(format!(
            "invalid field name: {name:?} is too large (maximum {MAX_FIELD_NAME_BYTES} UTF-8 bytes)"
        )
        .into());
    }
    // Firestore の予約正規表現 `__.*__`。`__name__` も含め、Map の
    // すべてのネストレベルで拒否する。
    if name.len() >= 4 && name.starts_with("__") && name.ends_with("__") {
        return Err(format!("field name {name} is reserved").into());
    }
    Ok(())
}

/// センチネルは `data_map` の直下にだけ置ける。値の中の Map / Array を
/// 再帰的にたどり、ネストされた transform を両バックエンドで拒否する。
pub(crate) fn reject_nested_sentinels(value: &Value, nested: bool) -> Result<(), Box<dyn Error>> {
    if nested
        && matches!(
            value,
            Value::ArrayUnion(_)
                | Value::ArrayRemove(_)
                | Value::ServerTimestamp
                | Value::Increment(_)
                | Value::DeleteField
        )
    {
        return Err("field transforms cannot be nested".into());
    }
    match value {
        Value::Array(items) | Value::ArrayUnion(items) | Value::ArrayRemove(items) => {
            for item in items {
                reject_nested_sentinels(item, true)?;
            }
        }
        Value::Map(map) => {
            for item in map.values() {
                reject_nested_sentinels(item, true)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// クエリフィルタ値の共有検証。in-memory / emulator のクエリ実行前に
/// 両バックエンドが呼び、同じエラーを同じメッセージで返す。
/// - 文字列フィルタ値は 1,500 UTF-8 バイト未満(Firestore 準拠。`in` 等の
///   配列要素・`array_contains` の要素も含む)
/// - `in` / `not-in` / `array_contains_any` の値は Array で要素は最大 10 個
pub(crate) fn validate_query_filters(filters: &[QueryFilter]) -> Result<(), Box<dyn Error>> {
    for f in filters {
        // `in` / `not-in` / `array_contains_any` の配列は候補値の一覧であり、
        // その要素を保存用 ArrayValue とみなすと、配列要素である候補を
        // 誤って拒否してしまう。各要素を独立したトップレベル値として検証する。
        if let (Value::Array(items), QueryOp::In | QueryOp::NotIn | QueryOp::ArrayContainsAny) =
            (&f.value, f.op)
        {
            for item in items {
                validate_firestore_value(item)?;
            }
        } else {
            validate_firestore_value(&f.value)?;
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
        // 単純フィールド名でも、Firestore の予約名(`__.*__`)やサイズ制限は
        // 適用される。update / merge set のパスではここでも検証する。
        validate_firestore_field_name(segment)?;
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
    fn value_validator_rejects_only_directly_nested_arrays() {
        let direct = Value::Array(vec![Value::Array(vec![Value::Int(1)])]);
        let err = validate_firestore_value(&direct).unwrap_err();
        assert!(format!("{err}").contains("Nested arrays"));

        // A map between two arrays makes the second array an indirect child.
        let indirect = Value::Array(vec![Value::Map(BTreeMap::from([(
            "values".to_string(),
            Value::Array(vec![Value::Int(1), Value::Int(2)]),
        )]))]);
        assert!(validate_firestore_value(&indirect).is_ok());
    }

    #[test]
    fn field_name_validator_checks_empty_and_reserved_names_recursively() {
        for (name, expected) in [
            ("", "empty string"),
            ("__name__", "reserved"),
            ("__reserved__", "reserved"),
        ] {
            let value = Value::Map(BTreeMap::from([(name.to_string(), Value::Int(1))]));
            let err = validate_firestore_value(&value).unwrap_err();
            assert!(format!("{err}").contains(expected), "{err}");
        }

        let nested = Value::Map(BTreeMap::from([(
            "outer".to_string(),
            Value::Array(vec![Value::Map(BTreeMap::from([(
                "__name__".to_string(),
                Value::Int(1),
            )]))]),
        )]));
        let err = validate_firestore_value(&nested).unwrap_err();
        assert!(format!("{err}").contains("reserved"), "{err}");

        let valid_nested = Value::Map(BTreeMap::from([(
            "outer".to_string(),
            Value::Array(vec![Value::Map(BTreeMap::from([(
                "values".to_string(),
                Value::Array(vec![Value::Int(1)]),
            )]))]),
        )]));
        assert!(validate_firestore_value(&valid_nested).is_ok());
    }

    #[test]
    fn transform_arguments_use_array_aware_validation() {
        let fields = BTreeMap::from([(
            "values".to_string(),
            Value::ArrayUnion(vec![Value::Array(vec![Value::Int(1)])]),
        )]);
        let err = validate_firestore_fields(&fields).unwrap_err();
        assert!(format!("{err}").contains("Nested arrays"), "{err}");

        let valid = BTreeMap::from([(
            "values".to_string(),
            Value::ArrayUnion(vec![Value::Map(BTreeMap::from([(
                "items".to_string(),
                Value::Array(vec![Value::Int(1)]),
            )]))]),
        )]);
        assert!(validate_firestore_fields(&valid).is_ok());
    }
}
