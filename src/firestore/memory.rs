use std::collections::{BTreeMap, HashMap};
use std::error::Error;

use super::{Firestore, WriteOp};
use crate::value::{QueryFilter, QueryOp, QueryRef, QuerySource, Timestamp, Value};

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
        validate_query_filter_values(&query.filters)?;
        match &query.source {
            QuerySource::Collection(path) => {
                self.query_collection(path, &query.filters, query.limit)
            }
            QuerySource::CollectionGroup(id) => {
                self.query_collection_group(id, &query.filters, query.limit)
            }
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
        matches.sort_by(|a, b| a.0.cmp(b.0));
        Ok(take_matches(matches, limit))
    }

    fn query_collection_group(
        &self,
        id: &str,
        filters: &[QueryFilter],
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
        matches.sort_by(|a, b| a.0.cmp(b.0));
        Ok(take_matches(matches, limit))
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

fn wrap_doc(path: &str, doc: &BTreeMap<String, Value>) -> Value {
    let mut out = doc.clone();
    out.insert("__name__".to_string(), Value::Str(path.to_string()));
    Value::Map(out)
}

fn take_matches(
    sorted: Vec<(&String, &BTreeMap<String, Value>)>,
    limit: Option<usize>,
) -> Vec<Value> {
    let n = limit.map_or(sorted.len(), |l| l.min(sorted.len()));
    sorted
        .into_iter()
        .take(n)
        .map(|(p, d)| wrap_doc(p, d))
        .collect()
}

fn matches_filters(doc: &BTreeMap<String, Value>, filters: &[QueryFilter]) -> bool {
    filters.iter().all(|f| {
        let Some(actual) = doc.get(&f.field) else {
            return false;
        };
        match f.op {
            QueryOp::Eq => actual == &f.value,
            QueryOp::Gt => compare_values(actual, &f.value, |o| o.is_gt()),
            QueryOp::Ge => compare_values(actual, &f.value, |o| o.is_ge()),
            QueryOp::Lt => compare_values(actual, &f.value, |o| o.is_lt()),
            QueryOp::Le => compare_values(actual, &f.value, |o| o.is_le()),
        }
    })
}

/// Firestore はクエリのフィルタ文字列値を 1,500 バイト未満に制限する
/// (1,500 バイト以上のフィルタ値はクエリ時にエラーになる。実機 emulator:
/// `value for {field} is too large to be used in a query`)。保存値に長さ
/// 制限はない。有効なフィルタ(< 1,500 バイト)に対しては、最初の差異位置
/// が常にフィルタ自身の長さ以内に収まるため、全文比較とインデックス接頭辞
/// 比較は同じ結果になる。
fn validate_query_filter_values(filters: &[QueryFilter]) -> Result<(), Box<dyn Error>> {
    for f in filters {
        if let Value::Str(s) = &f.value {
            if s.len() >= 1500 {
                return Err(
                    format!("value for {} is too large to be used in a query", f.field).into(),
                );
            }
        }
    }
    Ok(())
}

/// 大小比較は同型のみを規定する(Int / Str / Bool / Timestamp)。
/// 型が異なる・Nil は常にマッチしない(emulator の Firestore と実用上の
/// 挙動を合わせる。異なる型同士の Firestore の型順序は未規定 = spec §13)。
/// フィルタの文字列値は 1,500 バイト未満に制限されており(emulator の
/// Firestore と同じ)、有効なフィルタに対しては全文比較でよい(最初の差異
/// 位置は常にフィルタ自身の長さ以内 = 1,500 バイト未満に収まる)。
fn compare_values(a: &Value, b: &Value, pred: impl Fn(std::cmp::Ordering) -> bool) -> bool {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => pred(x.cmp(y)),
        (Value::Str(x), Value::Str(y)) => pred(x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => pred(x.cmp(y)),
        (Value::Timestamp(x), Value::Timestamp(y)) => pred(x.seconds.cmp(&y.seconds)),
        _ => false,
    }
}

fn apply_update_map(
    current: &mut BTreeMap<String, Value>,
    updates: &mut BTreeMap<String, Value>,
) -> Result<(), Box<dyn Error>> {
    for k in updates.keys() {
        super::validate_field_path(k)?;
    }
    // DeleteField / Increment は current への直接操作が必要なため、
    // 他のセンチネル解決より先に処理する。
    let mut deletes = vec![];
    let mut increments = vec![];
    for (k, v) in updates.iter() {
        match v {
            Value::DeleteField => deletes.push(k.clone()),
            Value::Increment(n) => {
                let segments: Vec<&str> = k.split('.').collect();
                let next = match lookup_field_path(Some(current), &segments) {
                    Some(Value::Int(cur)) => (*cur)
                        .checked_add(*n)
                        .ok_or_else(|| Box::<dyn Error>::from("integer overflow"))?,
                    Some(_) => {
                        return Err(format!("increment target must be an int: {k}").into());
                    }
                    None => *n,
                };
                increments.push((k.clone(), next));
            }
            _ => {}
        }
    }
    for k in &deletes {
        updates.remove(k);
        let segments: Vec<&str> = k.split('.').collect();
        remove_at_field_path(current, &segments)?;
    }
    for (k, next) in increments {
        updates.insert(k, Value::Int(next));
    }
    resolve_sentinel_map(updates, Some(current))?;
    for (k, v) in updates.iter() {
        let segments: Vec<&str> = k.split('.').collect();
        super::insert_at_field_path(current, &segments, v.clone())?;
    }
    Ok(())
}

fn resolve_sentinel_map(
    map: &mut BTreeMap<String, Value>,
    current: Option<&BTreeMap<String, Value>>,
) -> Result<(), Box<dyn Error>> {
    for (k, v) in map.clone() {
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
            Value::ServerTimestamp => Value::Timestamp(Timestamp {
                seconds: Timestamp::now()?.seconds,
            }),
            Value::Increment(n) => match base() {
                Some(Value::Int(cur)) => Value::Int(
                    (*cur)
                        .checked_add(n)
                        .ok_or_else(|| Box::<dyn Error>::from("integer overflow"))?,
                ),
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
