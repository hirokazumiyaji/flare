use std::collections::{BTreeMap, HashMap};
use std::error::Error;

use super::{Firestore, WriteOp};
use crate::value::{Direction, QueryFilter, QueryOp, QueryRef, QuerySource, Timestamp, Value};

#[derive(Default)]
pub struct InMemoryFirestore {
    docs: HashMap<String, BTreeMap<String, Value>>,
    tx_snapshot: Option<HashMap<String, BTreeMap<String, Value>>>,
}

impl Firestore for InMemoryFirestore {
    fn get(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>> {
        Ok(self.docs.get(path).cloned().map(Value::Map))
    }

    fn apply(&mut self, writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>> {
        let mut next = self.docs.clone();
        for op in writes {
            apply_write_to_docs(&mut next, op)?;
        }
        self.docs = next;
        Ok(())
    }

    fn run_query(&self, query: &QueryRef) -> Result<Vec<Value>, Box<dyn Error>> {
        super::validate_query_filters(&query.filters)?;
        match &query.source {
            QuerySource::Collection(path) => self.query_collection(
                path,
                &query.filters,
                &query.order_by,
                query.offset,
                query.limit,
            ),
            QuerySource::CollectionGroup(id) => self.query_collection_group(
                id,
                &query.filters,
                &query.order_by,
                query.offset,
                query.limit,
            ),
        }
    }

    fn begin_transaction(&mut self) -> Result<(), Box<dyn Error>> {
        if self.tx_snapshot.is_some() {
            return Err("already in transaction".into());
        }
        self.tx_snapshot = Some(self.docs.clone());
        Ok(())
    }

    fn transaction_get(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>> {
        match &self.tx_snapshot {
            Some(snapshot) => Ok(snapshot.get(path).cloned().map(Value::Map)),
            None => Err("not in transaction".into()),
        }
    }

    fn commit_transaction(&mut self, writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>> {
        let mut next = self.tx_snapshot.take().ok_or("not in transaction")?;
        for op in writes {
            apply_write_to_docs(&mut next, op)?;
        }
        self.docs = next;
        Ok(())
    }

    fn abort_transaction(&mut self) -> Result<(), Box<dyn Error>> {
        self.tx_snapshot = None;
        Ok(())
    }
}

impl InMemoryFirestore {
    fn query_collection(
        &self,
        path: &str,
        filters: &[QueryFilter],
        order_by: &[(String, Direction)],
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Result<Vec<Value>, Box<dyn Error>> {
        let mut matches: Vec<(&String, &BTreeMap<String, Value>)> = vec![];
        let prefix = format!("{path}/");
        for (doc_path, doc) in &self.docs {
            if !doc_path.starts_with(&prefix) {
                continue;
            }
            if doc_path[prefix.len()..].contains('/') {
                continue;
            }
            if matches_filters(doc, filters) {
                matches.push((doc_path, doc));
            }
        }
        sort_matches(&mut matches, order_by);
        Ok(take_matches(matches, offset, limit))
    }

    fn query_collection_group(
        &self,
        id: &str,
        filters: &[QueryFilter],
        order_by: &[(String, Direction)],
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Result<Vec<Value>, Box<dyn Error>> {
        let mut matches: Vec<(&String, &BTreeMap<String, Value>)> = vec![];
        for (doc_path, doc) in &self.docs {
            let segments: Vec<&str> = doc_path.split('/').collect();
            if segments.len() < 2 {
                continue;
            }
            let mut matched_group = false;
            for idx in (0..segments.len() - 1).step_by(2) {
                if segments[idx] == id {
                    matched_group = true;
                    break;
                }
            }
            if !matched_group {
                continue;
            }
            if matches_filters(doc, filters) {
                matches.push((doc_path, doc));
            }
        }
        sort_matches(&mut matches, order_by);
        Ok(take_matches(matches, offset, limit))
    }
}

fn apply_write_to_docs(
    docs: &mut HashMap<String, BTreeMap<String, Value>>,
    op: WriteOp,
) -> Result<(), Box<dyn Error>> {
    match op {
        WriteOp::Set {
            path,
            mut data,
            merge,
        } => {
            if merge {
                let current = docs.entry(path).or_default();
                apply_update_map(current, &mut data)?;
            } else {
                resolve_sentinel_map(&mut data)?;
                docs.insert(path, data);
            }
            Ok(())
        }
        WriteOp::Update { path, mut data } => {
            let current = docs
                .get_mut(&path)
                .ok_or_else(|| format!("document not found: {path}"))?;
            apply_update_map(current, &mut data)?;
            Ok(())
        }
        WriteOp::Delete { path } => {
            docs.remove(&path);
            Ok(())
        }
    }
}

fn wrap_doc(path: &str, doc: &BTreeMap<String, Value>) -> Value {
    let mut out = doc.clone();
    out.insert("__name__".to_string(), Value::Str(path.to_string()));
    Value::Map(out)
}

fn take_matches(
    sorted: Vec<(&String, &BTreeMap<String, Value>)>,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Vec<Value> {
    let skip = offset.unwrap_or(0).min(sorted.len());
    let mut values: Vec<Value> = sorted
        .into_iter()
        .skip(skip)
        .map(|(p, d)| wrap_doc(p, d))
        .collect();
    if let Some(l) = limit {
        values.truncate(l.min(values.len()));
    }
    values
}

/// 結果の並べ替え。order_by が空なら従来どおりパス辞書順。指定時は
/// フィールド値の順に並べ(同値はパス辞書順でタイブレーク)、対象フィールドを
/// 持たないドキュメントは除外する。型が異なる値同士は型ランクで比べる
/// (同型のみを規定する where と異なり、順序は全順序にする必要があるため)。
fn sort_matches(
    matches: &mut Vec<(&String, &BTreeMap<String, Value>)>,
    order_by: &[(String, Direction)],
) {
    if order_by.is_empty() {
        matches.sort_by(|a, b| a.0.cmp(b.0));
        return;
    }
    matches.retain(|(_, doc)| order_by.iter().all(|(field, _)| doc.contains_key(field)));
    matches.sort_by(|a, b| {
        for (field, dir) in order_by {
            let ord = super::compare_order_values(&a.1[field], &b.1[field]);
            let ord = match dir {
                Direction::Asc => ord,
                Direction::Desc => ord.reverse(),
            };
            if ord != std::cmp::Ordering::Equal {
                return ord;
            }
        }
        a.0.cmp(b.0)
    });
}

pub(crate) fn matches_filters(doc: &BTreeMap<String, Value>, filters: &[QueryFilter]) -> bool {
    filters.iter().all(|f| {
        // `!=` / `in` / `not-in` はフィールドの存在を要求する(存在しなければ
        // マッチしない)。`order_by` 対象の存在チェックは sort 側で行う。
        let actual = doc.get(&f.field);
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
                    if list.contains(&Value::Nil) || v == &Value::Nil {
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

/// 大小比較は同型のみを規定する(Int / Str / Bool / Timestamp)。
/// 型が異なる・Nil は常にマッチしない(emulator の Firestore と実用上の
/// 挙動を合わせる。異なる型同士の Firestore の型順序は未規定 = spec §13)。
/// フィルタの文字列値は 1,500 バイト未満に制限されており(emulator の
/// Firestore と同じ)、有効なフィルタに対しては全文比較でよい(最初の差異
/// 位置は常にフィルタ自身の長さ以内 = 1,500 バイト未満に収まる)。
fn compare_values(a: Option<&Value>, b: &Value, pred: impl Fn(std::cmp::Ordering) -> bool) -> bool {
    match (a, b) {
        (Some(Value::Int(x)), Value::Int(y)) => pred(x.cmp(y)),
        (Some(Value::Str(x)), Value::Str(y)) => pred(x.cmp(y)),
        (Some(Value::Bool(x)), Value::Bool(y)) => pred(x.cmp(y)),
        (Some(Value::Timestamp(x)), Value::Timestamp(y)) => pred(x.seconds.cmp(&y.seconds)),
        _ => false,
    }
}

fn apply_update_map(
    current: &mut BTreeMap<String, Value>,
    updates: &mut BTreeMap<String, Value>,
) -> Result<(), Box<dyn Error>> {
    for (k, v) in updates.iter() {
        super::validate_field_path(k)?;
        reject_nested_sentinels(v, false)?;
    }
    // Firestore の挙動に合わせ、1) 削除 2) 通常フィールド更新 3) フィールド変換(increment等)
    // の順序で適用する。
    let mut deletes = vec![];
    let mut plain = vec![];
    let mut transforms = vec![];
    for (k, v) in updates.iter() {
        match v {
            Value::DeleteField => deletes.push(k.clone()),
            Value::ArrayUnion(_)
            | Value::ArrayRemove(_)
            | Value::ServerTimestamp
            | Value::Increment(_) => transforms.push((k.clone(), v.clone())),
            _ => plain.push((k.clone(), v.clone())),
        }
    }
    super::validate_no_overlapping_paths(
        deletes
            .iter()
            .map(String::as_str)
            .chain(plain.iter().map(|(k, _)| k.as_str())),
    )?;
    for k in &deletes {
        let segments: Vec<&str> = k.split('.').collect();
        remove_at_field_path(current, &segments)?;
    }
    for (k, v) in plain {
        let segments: Vec<&str> = k.split('.').collect();
        super::insert_at_field_path(current, &segments, v)?;
    }
    for (k, v) in transforms {
        let segments: Vec<&str> = k.split('.').collect();
        let base = lookup_field_path(Some(current), &segments);
        let resolved = match v {
            Value::ArrayUnion(values) => {
                let mut arr = match base {
                    Some(Value::Array(a)) => a.clone(),
                    _ => vec![],
                };
                for item in values {
                    if !arr.contains(&item) {
                        arr.push(item);
                    }
                }
                Value::Array(arr)
            }
            Value::ArrayRemove(values) => {
                let arr = match base {
                    Some(Value::Array(a)) => a.clone(),
                    _ => vec![],
                };
                Value::Array(arr.into_iter().filter(|v| !values.contains(v)).collect())
            }
            Value::ServerTimestamp => Value::Timestamp(Timestamp {
                seconds: Timestamp::now()?.seconds,
            }),
            Value::Increment(n) => match base {
                Some(Value::Int(cur)) => Value::Int(cur.checked_add(n).unwrap_or_else(|| {
                    if n.is_negative() { i64::MIN } else { i64::MAX }
                })),
                _ => Value::Int(n),
            },
            Value::DeleteField => unreachable!(),
            other => other,
        };
        super::insert_at_field_path(current, &segments, resolved)?;
    }
    Ok(())
}

fn resolve_sentinel_map(map: &mut BTreeMap<String, Value>) -> Result<(), Box<dyn Error>> {
    for (k, v) in map.clone() {
        reject_nested_sentinels(&v, false)?;
        let resolved = match v {
            Value::ArrayUnion(values) => {
                let mut arr = vec![];
                for item in values {
                    if !arr.contains(&item) {
                        arr.push(item);
                    }
                }
                Value::Array(arr)
            }
            Value::ArrayRemove(_) => Value::Array(vec![]),
            Value::ServerTimestamp => Value::Timestamp(Timestamp {
                seconds: Timestamp::now()?.seconds,
            }),
            Value::Increment(n) => Value::Int(n),
            // 置換 set(merge なし)でフィールド削除は意味を持たない。
            // Firestore と同様にエラーにし、emulator と挙動を揃える。
            Value::DeleteField => {
                return Err("delete_field() cannot be used with set() without merge".into());
            }
            other => other,
        };
        map.insert(k, resolved);
    }
    Ok(())
}

fn reject_nested_sentinels(value: &Value, nested: bool) -> Result<(), Box<dyn Error>> {
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

/// フィールドパスでネストした値を削除する。パスが存在しなければ何もしない。
/// 中間が存在して Map でなければエラー(insert と同じ厳しさ)。
fn remove_at_field_path(
    map: &mut BTreeMap<String, Value>,
    segments: &[&str],
) -> Result<(), Box<dyn Error>> {
    let Some((first, rest)) = segments.split_first() else {
        return Err("empty field path".into());
    };
    if rest.is_empty() {
        map.remove(*first);
        return Ok(());
    }
    match map.get_mut(*first) {
        None => Ok(()),
        Some(Value::Map(inner)) => remove_at_field_path(inner, rest),
        Some(_) => Err(format!("cannot update field path: field {first:?} is not a map").into()),
    }
}

/// フィールドパスでネストした値を参照する(中間が Map でなければ見つからない)。
fn lookup_field_path<'a>(
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
