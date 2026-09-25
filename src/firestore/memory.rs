use std::collections::{BTreeMap, HashMap};
use std::error::Error;

use super::{Firestore, WriteOp};
use crate::value::{QueryRef, QuerySource, Timestamp, Value};

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

    fn run_query(&self, query: &QueryRef) -> Result<super::QueryCandidates, Box<dyn Error>> {
        super::validate_query_filters(&query.filters)?;
        Ok(self
            .candidates(&query.source)
            .filter(|(path, doc)| super::matches_filters(path, doc, &query.filters))
            .map(|(path, doc)| (path.clone(), doc.clone()))
            .collect())
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
    /// ソースに属するドキュメント候補を列挙する。フィルタはバックエンド
    /// 共通の matcher、順序・offset/limit・`__name__` は runtime が適用する。
    fn candidates<'a>(
        &'a self,
        source: &'a QuerySource,
    ) -> impl Iterator<Item = (&'a String, &'a BTreeMap<String, Value>)> + 'a {
        self.docs.iter().filter(move |(path, _)| match source {
            QuerySource::Collection(collection) => is_direct_child(collection, path),
            QuerySource::CollectionGroup(id) => path_in_group(id, path),
        })
    }
}

fn is_direct_child(collection: &str, doc_path: &str) -> bool {
    let prefix = format!("{collection}/");
    doc_path.starts_with(&prefix) && !doc_path[prefix.len()..].contains('/')
}

fn path_in_group(id: &str, doc_path: &str) -> bool {
    let segments: Vec<&str> = doc_path.split('/').collect();
    if segments.len() < 2 {
        return false;
    }
    (0..segments.len() - 1)
        .step_by(2)
        .any(|idx| segments[idx] == id)
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
                resolve_sentinel_map(&mut data, None)?;
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
            Value::ServerTimestamp => Value::Timestamp(Timestamp::now()?),
            Value::Increment(n) => match base {
                Some(Value::Int(cur)) => Value::Int(cur.checked_add(n).unwrap_or_else(|| {
                    if n.is_negative() {
                        i64::MIN
                    } else {
                        i64::MAX
                    }
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

fn resolve_sentinel_map(
    map: &mut BTreeMap<String, Value>,
    current: Option<&BTreeMap<String, Value>>,
) -> Result<(), Box<dyn Error>> {
    for (k, v) in map.clone() {
        reject_nested_sentinels(&v, false)?;
        let segments: Vec<&str> = k.split('.').collect();
        let base = || lookup_field_path(current, &segments);
        let resolved = match v {
            Value::ArrayUnion(values) => {
                let mut arr = match base() {
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
                let arr = match base() {
                    Some(Value::Array(a)) => a.clone(),
                    _ => vec![],
                };
                Value::Array(arr.into_iter().filter(|v| !values.contains(v)).collect())
            }
            Value::ServerTimestamp => Value::Timestamp(Timestamp::now()?),
            Value::Increment(n) => match base() {
                Some(Value::Int(cur)) => Value::Int(cur.checked_add(n).unwrap_or_else(|| {
                    if n.is_negative() {
                        i64::MIN
                    } else {
                        i64::MAX
                    }
                })),
                _ => Value::Int(n),
            },
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
