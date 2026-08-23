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

    fn abort_transaction(&mut self) {
        self.tx_snapshot = None;
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
            QueryOp::Gt => compare_int(actual, &f.value, |a, b| a > b),
            QueryOp::Ge => compare_int(actual, &f.value, |a, b| a >= b),
            QueryOp::Lt => compare_int(actual, &f.value, |a, b| a < b),
            QueryOp::Le => compare_int(actual, &f.value, |a, b| a <= b),
        }
    })
}

fn compare_int(a: &Value, b: &Value, pred: impl Fn(i64, i64) -> bool) -> bool {
    matches!((a, b), (Value::Int(x), Value::Int(y)) if pred(*x, *y))
}

fn apply_update_map(
    current: &mut BTreeMap<String, Value>,
    updates: &mut BTreeMap<String, Value>,
) -> Result<(), Box<dyn Error>> {
    resolve_sentinel_map(updates, Some(current))?;
    for (k, v) in updates.iter() {
        current.insert(k.clone(), v.clone());
    }
    Ok(())
}

fn resolve_sentinel_map(
    map: &mut BTreeMap<String, Value>,
    current: Option<&BTreeMap<String, Value>>,
) -> Result<(), Box<dyn Error>> {
    for (k, v) in map.clone() {
        let resolved = match v {
            Value::ArrayUnion(values) => {
                let mut base = match current.and_then(|m| m.get(&k)) {
                    Some(Value::Array(arr)) => arr.clone(),
                    _ => vec![],
                };
                for item in values {
                    if !base.contains(&item) {
                        base.push(item);
                    }
                }
                Value::Array(base)
            }
            Value::ArrayRemove(values) => {
                let base = match current.and_then(|m| m.get(&k)) {
                    Some(Value::Array(arr)) => arr.clone(),
                    _ => vec![],
                };
                Value::Array(base.into_iter().filter(|v| !values.contains(v)).collect())
            }
            Value::ServerTimestamp => Value::Timestamp(Timestamp {
                seconds: Timestamp::now()?.seconds,
            }),
            other => other,
        };
        map.insert(k, resolved);
    }
    Ok(())
}
