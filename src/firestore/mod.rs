//! Firestore backend abstraction.

#[cfg(feature = "emulator")]
mod emulator;
mod memory;

#[cfg(feature = "emulator")]
pub use emulator::{
    parse_timeout_ms, EmulatorAuth, EmulatorConfig, EmulatorConnectionError, FirestoreEmulator,
    FirestoreEmulatorOptions, DEFAULT_OPERATION_TIMEOUT, OPERATION_TIMEOUT_ENV,
};
pub use memory::InMemoryFirestore;

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::error::Error;

use crate::value::{Direction, QueryFilter, QueryOp, QueryRef, Value};

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
        | Value::Bytes(_)
        // in-memory は参照を直接保持する。emulator の write codec が
        // 書き込みを拒否する(`unsupported value in emulator write`)。
        | Value::DocRef(_)
        | Value::ServerTimestamp
        | Value::Increment(_)
        | Value::DeleteField => Ok(()),
        // transform の要素も保存対象になるため、Array と同じく再帰検証する。
        // 検証しないと in-memory が保存できた値を emulator が encode 時に
        // 拒否し、両バックエンドで挙動が割れる。
        Value::ArrayUnion(items) | Value::ArrayRemove(items) => {
            for item in items {
                validate_firestore_writable_value(item)?;
            }
            Ok(())
        }
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
    // すべてのネストレベルで拒否する。Datastore 由来の数値 ID
    // (`__id[0-9]+__`)は既存参照として例外的に許可する。
    if name.len() >= 4
        && name.starts_with("__")
        && name.ends_with("__")
        && !is_legacy_datastore_numeric_id(name)
    {
        return Err(format!("field name {name} is reserved").into());
    }
    Ok(())
}

/// Datastore 由来の数値エンティティ ID(`__id` + 10進数字 + `__`)かどうか。
/// 予約パターン `__.*__` の例外として、既存参照の読み書きに使う。
pub(crate) fn is_legacy_datastore_numeric_id(id: &str) -> bool {
    let Some(inner) = id.strip_prefix("__id").and_then(|s| s.strip_suffix("__")) else {
        return false;
    };
    !inner.is_empty() && inner.bytes().all(|b| b.is_ascii_digit())
}

const MAX_QUERY_STRING_BYTES: usize = 1500;

/// 1 つの `set` / `update` data_map を、staging 前に検証する。
///
/// ここで使うのは `validate_write_op` と同じ共有 validator なので、runtime が
/// staging を拒否する条件と各 backend が write 境界で拒绝する条件は 1 箇所に
/// 集約されている。キーをフィールドパスとして扱うかフィールド名として扱うかは
/// backend に委ねるため、ここでは検証しない。
///
/// DocRef は両バックエンドで同じ扱いではないため、ここでは許可する。in-memory は
/// 参照を直接保持し、emulator の write codec が `unsupported value in emulator
/// write` で拒否する(§7)。
pub(crate) fn validate_write_data(data: &BTreeMap<String, Value>) -> Result<(), Box<dyn Error>> {
    for value in data.values() {
        reject_nested_sentinels(value, false)?;
        validate_firestore_value(value)?;
        validate_firestore_writable_value(value)?;
    }
    Ok(())
}

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

/// 値の中の文字列・バイト列を葉としてたどり、各値がクエリの上限未満で
/// あることを確認する。Firestore の制限は文字数ではなくバイト数で判定
/// される。`Str` だけでなく `Bytes` にも同じ上限を課す。Firestore の順序
/// 比較は比較対象の先頭しか見ないため、上限なしの値を渡すと emulator と
/// in-memory の結果がずれる(in-memory は全文比較するため)。
fn check_query_value_size(field: &str, value: &Value) -> Result<(), Box<dyn Error>> {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Value::Str(s) if s.len() >= MAX_QUERY_STRING_BYTES => {
                return Err(format!("value for {field} is too large to be used in a query").into());
            }
            Value::Bytes(b) if b.len() >= MAX_QUERY_STRING_BYTES => {
                return Err(format!("value for {field} is too large to be used in a query").into());
            }
            Value::Array(items) => pending.extend(items.iter()),
            Value::Map(fields) => pending.extend(fields.values()),
            _ => {}
        }
    }
    Ok(())
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

pub(crate) fn query_field_value<'a>(
    doc: &'a BTreeMap<String, Value>,
    field: &str,
) -> Option<&'a Value> {
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

/// Firestore の 1 セグメントの単純フィールド名
/// (`[A-Za-z_][A-Za-z0-9_]*`)かどうかを返す。
fn is_simple_field_name(field: &str) -> bool {
    let mut chars = field.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// フィールド名を Firestore API の field path 表現へ変換する。単純名だけを
/// quote せず、それ以外はバッククォートで囲み、内部のバッククォートと
/// バックスラッシュをエスケープする。
/// emulator でのみ使うため、通常ビルドの dead_code 警告を避ける。
/// 単体テストでも検証できるよう test でも有効化する。
#[cfg(any(test, feature = "emulator"))]
pub(crate) fn quote_field_name(field: &str) -> String {
    if is_simple_field_name(field) {
        return field.to_string();
    }

    let mut quoted = String::with_capacity(field.len() + 2);
    quoted.push('`');
    for ch in field.chars() {
        if matches!(ch, '`' | '\\') {
            quoted.push('\\');
        }
        quoted.push(ch);
    }
    quoted.push('`');
    quoted
}

/// write data を通常の値・transform・削除に分ける。update / merge では
/// Firestore と同じく「削除 → 通常値 → transform」の順に適用する。
#[derive(Debug)]
pub(crate) struct WriteValues {
    pub(crate) plain: BTreeMap<String, Value>,
    pub(crate) transforms: Vec<(String, Value)>,
    pub(crate) deletes: Vec<String>,
}

pub(crate) fn split_write_values(data: &BTreeMap<String, Value>) -> WriteValues {
    let mut plain = BTreeMap::new();
    let mut transforms = vec![];
    let mut deletes = vec![];
    for (key, value) in data {
        match value {
            Value::ArrayUnion(_)
            | Value::ArrayRemove(_)
            | Value::ServerTimestamp
            | Value::Increment(_) => transforms.push((key.clone(), value.clone())),
            Value::DeleteField => deletes.push(key.clone()),
            _ => {
                plain.insert(key.clone(), value.clone());
            }
        }
    }
    WriteValues {
        plain,
        transforms,
        deletes,
    }
}

/// センチネルは data_map の直下にだけ置ける。値の中の Map / Array を
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

/// update / merge set 共通の plan。plain と delete は mask に入るため互いに
/// overlap してはいけない。transform 同士も targets が曖昧になるため
/// overlap を拒否する。transform と mask の重なりは仕様 §6 の順序
/// (delete → plain → transform)を両 backend で適用するため許可し、
/// emulator 互換の parity テストで結果を確認する。
pub(crate) fn plan_update_writes(
    data: &BTreeMap<String, Value>,
) -> Result<WriteValues, Box<dyn Error>> {
    for key in data.keys() {
        validate_field_path(key)?;
    }

    let values = split_write_values(data);
    validate_no_overlapping_paths(
        values
            .deletes
            .iter()
            .map(String::as_str)
            .chain(values.plain.keys().map(String::as_str)),
    )?;
    validate_no_overlapping_paths(values.transforms.iter().map(|(path, _)| path.as_str()))?;
    // Keep the nested-sentinel rule in the shared plan so the emulator and
    // in-memory backend reject the same payload with the same message.
    for value in data.values() {
        reject_nested_sentinels(value, false)?;
    }
    Ok(values)
}

/// update / merge set のキーは `.` 区切りのフィールドパス(Firestore 準拠)。
/// 各セグメントは Firestore の単純フィールドパス文法
/// (`[A-Za-z_][A-Za-z0-9_]*`)に従うこと。バッククォートによるエスケープは
/// 非対応なので、バッククォートや空セグメントを含むキーはエラーにする。
pub(crate) fn validate_field_path(key: &str) -> Result<(), Box<dyn Error>> {
    for segment in key.split('.') {
        if !is_simple_field_name(segment) {
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
/// 順序・offset/limit と結果行への `__name__` 付与は未適用(runtime が行う)。
/// `run_query` は順序付けしない互換パスでもある。ただし `__name__` を対象と
/// する filter 自体は backend の共有 matcher が相対パスに対して評価する。
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

pub trait Firestore {
    fn get(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>>;
    /// Apply writes atomically: either all writes persist or none do.
    fn apply(&mut self, writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>>;
    /// Returns filter-matching candidates as (relative path, document fields).
    /// Ordering, offset, and limit are NOT applied here. This method remains the
    /// unshaped compatibility path for Firestore implementations. Ordering,
    /// offset/limit, and insertion of the synthetic `__name__` field are
    /// applied by the runtime; a filter on `__name__` itself is evaluated
    /// against the relative path by the shared backend matcher (spec §7).
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
    fn writable_check_rejects_non_writable_transform_elements() {
        // array_union / array_remove の要素も保存対象になるため、書き込み
        // 不可な値はここで拒否する。検証しないと in-memory が保存できた値を
        // emulator が encode 時に拒否し、挙動が割れる。DocRef は in-memory が
        // 直接保持するため許可する(emulator の write codec が拒否する)。
        let fields = BTreeMap::from([(
            "values".to_string(),
            Value::ArrayRemove(vec![Value::Opaque("Double(1.5)".to_string())]),
        )]);
        let err = validate_firestore_fields(&fields).unwrap_err();
        assert!(
            format!("{err}").contains("unsupported value in write"),
            "{err}"
        );
        let fields = BTreeMap::from([(
            "values".to_string(),
            Value::ArrayUnion(vec![Value::DocRef(crate::value::DocRef {
                path: "users/1".to_string(),
            })]),
        )]);
        assert!(validate_firestore_fields(&fields).is_ok());
        // Bytes は第一級の保存可能値。transform 要素でも許可する。
        let fields = BTreeMap::from([(
            "values".to_string(),
            Value::ArrayUnion(vec![Value::Bytes(vec![1, 2])]),
        )]);
        assert!(validate_firestore_fields(&fields).is_ok());
    }

    #[test]
    fn legacy_datastore_numeric_field_names_are_accepted() {
        let fields = BTreeMap::from([("__id123__".to_string(), Value::Int(1))]);
        assert!(validate_firestore_fields(&fields).is_ok());
        let fields = BTreeMap::from([("__reserved__".to_string(), Value::Int(1))]);
        let err = validate_firestore_fields(&fields).unwrap_err();
        assert!(format!("{err}").contains("reserved"), "{err}");
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

    fn filter(op: QueryOp, value: Value) -> QueryFilter {
        QueryFilter {
            field: "b".to_string(),
            op,
            value,
        }
    }

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

    #[test]
    fn filter_values_are_bounded_for_str_and_bytes() {
        let big_bytes = Value::Bytes(vec![0u8; MAX_QUERY_STRING_BYTES]);
        // ちょうど 1,500 バイトの Str / Bytes はどちらも却下する。
        let err = validate_query_filters(&[filter(QueryOp::Eq, Value::Str("x".repeat(1500)))])
            .unwrap_err();
        assert!(err.to_string().contains("too large"), "{err}");
        let err = validate_query_filters(&[filter(QueryOp::Eq, big_bytes)]).unwrap_err();
        assert!(err.to_string().contains("too large"), "{err}");
    }

    #[test]
    fn filter_values_below_the_bound_are_accepted() {
        let just_under = Value::Bytes(vec![0u8; MAX_QUERY_STRING_BYTES - 1]);
        validate_query_filters(&[filter(QueryOp::Eq, just_under)]).unwrap();
    }

    #[test]
    fn element_size_check_applies_inside_in_operands() {
        // `in` の配列要素も同じ上限の対象になる。
        let operand = Value::Array(vec![
            Value::Int(1),
            Value::Bytes(vec![0u8; MAX_QUERY_STRING_BYTES]),
        ]);
        let err = validate_query_filters(&[filter(QueryOp::In, operand)]).unwrap_err();
        assert!(err.to_string().contains("too large"), "{err}");
    }

    #[test]
    fn quote_field_name_quotes_every_non_simple_name() {
        assert_eq!(quote_field_name("simple_2"), "simple_2");
        assert_eq!(quote_field_name("a.b"), r#"`a.b`"#);
        assert_eq!(quote_field_name("a-b"), r#"`a-b`"#);
        assert_eq!(quote_field_name("1start_digit"), r#"`1start_digit`"#);
        assert_eq!(quote_field_name("a`b"), r#"`a\`b`"#);
        assert_eq!(quote_field_name(r"a\b"), r#"`a\\b`"#);
    }

    #[test]
    fn update_plan_rejects_mask_path_overlap() {
        let err = plan_update_writes(&BTreeMap::from([
            ("a".to_string(), Value::DeleteField),
            ("a.b".to_string(), Value::Int(1)),
        ]))
        .unwrap_err();
        assert_eq!(err.to_string(), "overlapping field paths: a and a.b");
    }

    #[test]
    fn update_plan_rejects_overlapping_transform_targets() {
        let err = plan_update_writes(&BTreeMap::from([
            ("a".to_string(), Value::Increment(1)),
            ("a.b".to_string(), Value::Increment(1)),
        ]))
        .unwrap_err();
        assert_eq!(err.to_string(), "overlapping field paths: a and a.b");
    }

    #[test]
    fn update_plan_allows_transform_path_overlap_for_ordered_application() {
        let plan = plan_update_writes(&BTreeMap::from([
            ("a".to_string(), Value::Int(100)),
            ("a.b".to_string(), Value::Increment(1)),
        ]))
        .unwrap();
        assert_eq!(plan.plain.get("a"), Some(&Value::Int(100)));
        assert_eq!(
            plan.transforms,
            vec![("a.b".to_string(), Value::Increment(1))]
        );
        assert!(plan.deletes.is_empty());
    }
}
