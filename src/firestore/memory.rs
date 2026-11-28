use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::sync::Arc;

use super::{prepare_query_filters, validate_write_op, Firestore, WriteOp};
use crate::value::{QueryRef, QuerySource, Timestamp, Value};

/// in-memory backend が `server_timestamp()` の request time を取得する時計。
///
/// 実運用では [`Timestamp::now`] を使い、テストでは commit ごとに異なる固定値を
/// 返すクロージャーを注入できる。
///
/// コミット処理はこのクロージャーを**高々 1 回**呼ぶ。request time は
/// `server_timestamp()` を含む sentinel を解決するときだけ必要になるため、
/// timestamp を含まない write や空の transaction commit では **0 回**である。
/// 呼び出し側が「1 commit = 1 回」のように回数で状態を返す closure を
/// 書かないこと: 同一 commit 内の複数 sentinel は 1 つの request time を
/// 共有し、commit ごとに値が同じでも次の commit でまた呼ばれる。
pub type RequestClock = Arc<dyn Fn() -> Result<Timestamp, Box<dyn Error>> + Send + Sync + 'static>;

struct RequestTime<'a> {
    clock: &'a RequestClock,
    value: Option<Timestamp>,
}

impl RequestTime<'_> {
    fn get(&mut self) -> Result<&Timestamp, Box<dyn Error>> {
        if self.value.is_none() {
            self.value = Some((self.clock)()?);
        }
        self.value
            .as_ref()
            .ok_or_else(|| "request time was not initialized".into())
    }
}

type DocMap = BTreeMap<String, Value>;
type DocStore = HashMap<String, Arc<DocMap>>;

pub struct InMemoryFirestore {
    docs: DocStore,
    tx_snapshot: Option<DocStore>,
    request_clock: RequestClock,
}

impl Default for InMemoryFirestore {
    fn default() -> Self {
        Self::with_clock(Timestamp::now)
    }
}

impl InMemoryFirestore {
    /// `server_timestamp()` を使う request time を注入して in-memory
    /// Firestore を作る。
    ///
    /// `clock` は 1 回の write / batch / transaction commit につき**高々 1 回**、
    /// そしてその commit が `server_timestamp()` を含むときだけ呼ばれる。
    /// timestamp を含まない write や空の commit では呼ばれないので、呼び出し
    /// 回数を前提にした状態を進めないこと(値は commit ごとに 1 つ共有される)。
    pub fn with_clock<F>(clock: F) -> Self
    where
        F: Fn() -> Result<Timestamp, Box<dyn Error>> + Send + Sync + 'static,
    {
        Self {
            docs: HashMap::new(),
            tx_snapshot: None,
            request_clock: Arc::new(clock),
        }
    }

    /// 常に同じ request time を返す、決定論的な clock を注入する。
    pub fn with_fixed_timestamp(seconds: i64) -> Self {
        Self::with_clock(move || Ok(Timestamp { seconds }))
    }
}

impl Firestore for InMemoryFirestore {
    fn get(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>> {
        Ok(self
            .docs
            .get(path)
            .map(|doc| Value::Map(doc.as_ref().clone())))
    }

    fn apply(&mut self, writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>> {
        // Sample REQUEST_TIME lazily: ordinary writes must not depend on a
        // fallible clock callback, and all sentinels in one commit share it.
        let mut request_time = RequestTime {
            clock: &self.request_clock,
            value: None,
        };
        // Clone the map (paths + Arc pointers). Unrelated payloads stay shared
        // until a write `make_mut`s that entry. A failed batch drops `next`.
        let mut next = self.docs.clone();
        for op in writes {
            apply_write_to_docs(&mut next, op, &mut request_time)?;
        }
        self.docs = next;
        Ok(())
    }

    fn run_query(&self, query: &QueryRef) -> Result<super::QueryCandidates, Box<dyn Error>> {
        super::validate_query_filters(&query.filters)?;
        let (field_paths, normalized_values) = prepare_query_filters(&query.filters)?;
        Ok(self
            .candidates(&query.source)
            .filter(|(path, doc)| {
                super::matches_filters(path, doc, &query.filters, &field_paths, &normalized_values)
            })
            .map(|(path, doc)| (path.clone(), doc.clone()))
            .collect())
    }

    fn run_query_page(&self, query: &QueryRef) -> Result<super::QueryCandidates, Box<dyn Error>> {
        super::validate_query_filters(&query.filters)?;
        let (field_paths, normalized_values) = prepare_query_filters(&query.filters)?;
        let order_by = super::parse_order_by(query)?;
        let rows = super::page_borrowed_candidates(
            self.candidates(&query.source).filter(|(path, doc)| {
                super::matches_filters(path, doc, &query.filters, &field_paths, &normalized_values)
            }),
            query,
            &order_by,
        );
        Ok(rows
            .into_iter()
            .map(|(path, doc)| (path.clone(), doc.clone()))
            .collect())
    }

    fn count_query(&self, query: &QueryRef) -> Result<i64, Box<dyn Error>> {
        super::validate_query_filters(&query.filters)?;
        let (field_paths, normalized_values) = prepare_query_filters(&query.filters)?;
        Ok(self
            .candidates(&query.source)
            .filter(|(path, doc)| {
                super::matches_filters(path, doc, &query.filters, &field_paths, &normalized_values)
            })
            .count() as i64)
    }

    /// The budget bounds the **matching candidates** the backend walks, not the
    /// page the script receives: `limit` / `offset` shape the page afterwards,
    /// so a `limit(3)` query over 20 matches still succeeds while the scan
    /// stays bounded.  Counting first and cloning afterwards also keeps the
    /// extra match that proves the budget was exceeded from ever being
    /// materialized.
    fn run_query_bounded(
        &self,
        query: &QueryRef,
        max_results: usize,
    ) -> Result<super::QueryCandidates, Box<dyn Error>> {
        super::validate_query_filters(&query.filters)?;
        let (field_paths, normalized_values) = prepare_query_filters(&query.filters)?;
        let order_by = super::parse_order_by(query)?;
        let mut matched = 0usize;
        for _ in self.candidates(&query.source).filter(|(path, doc)| {
            super::matches_filters(path, doc, &query.filters, &field_paths, &normalized_values)
        }) {
            matched += 1;
            if matched > max_results {
                return Err(super::query_result_limit_error(max_results));
            }
        }
        Ok(super::page_borrowed_candidates(
            self.candidates(&query.source).filter(|(path, doc)| {
                super::matches_filters(path, doc, &query.filters, &field_paths, &normalized_values)
            }),
            query,
            &order_by,
        )
        .into_iter()
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
            Some(snapshot) => Ok(snapshot
                .get(path)
                .map(|doc| Value::Map(doc.as_ref().clone()))),
            None => Err("not in transaction".into()),
        }
    }

    fn commit_transaction(&mut self, writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>> {
        // Build the committed state from a copy.  A failed write (for
        // example, an update of a missing document) must not consume the
        // transaction state: the runtime needs to be able to abort it and a
        // direct caller may retry after correcting the writes.
        if writes.is_empty() {
            // Nothing can fail, so the snapshot can be moved instead of
            // cloned; this keeps `transaction { ... }` with no staged write
            // from copying the whole store.
            self.docs = self.tx_snapshot.take().ok_or("not in transaction")?;
            return Ok(());
        }
        let mut next = self
            .tx_snapshot
            .as_ref()
            .ok_or("not in transaction")?
            .clone();
        // transaction も同じ commit request の clock を全 write で共有する。
        let mut request_time = RequestTime {
            clock: &self.request_clock,
            value: None,
        };
        for op in writes {
            apply_write_to_docs(&mut next, op, &mut request_time)?;
        }
        self.docs = next;
        self.tx_snapshot = None;
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
    ) -> impl Iterator<Item = (&'a String, &'a DocMap)> + 'a {
        self.docs
            .iter()
            .filter(move |(path, _)| match source {
                QuerySource::Collection(collection) => is_direct_child(collection, path),
                QuerySource::CollectionGroup(id) => path_in_group(id, path),
            })
            .map(|(path, doc)| (path, doc.as_ref()))
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
    docs: &mut DocStore,
    op: WriteOp,
    request_time: &mut RequestTime<'_>,
) -> Result<(), Box<dyn Error>> {
    validate_write_op(&op)?;
    match op {
        WriteOp::Set {
            path,
            mut data,
            merge,
        } => {
            if merge {
                let current = Arc::make_mut(docs.entry(path).or_default());
                apply_update_map(current, &mut data, request_time)?;
            } else {
                resolve_sentinel_map(&mut data, None, request_time)?;
                docs.insert(path, Arc::new(data));
            }
            Ok(())
        }
        WriteOp::Update { path, mut data } => {
            let current = docs
                .get_mut(&path)
                .ok_or_else(|| format!("document not found: {path}"))?;
            apply_update_map(Arc::make_mut(current), &mut data, request_time)?;
            Ok(())
        }
        WriteOp::Delete { path } => {
            docs.remove(&path);
            Ok(())
        }
    }
}

fn apply_update_map(
    current: &mut DocMap,
    updates: &mut BTreeMap<String, Value>,
    request_time: &mut RequestTime<'_>,
) -> Result<(), Box<dyn Error>> {
    let super::WriteValues {
        plain,
        transforms,
        deletes,
    } = super::plan_update_writes(updates)?;

    // Firestore の挙動に合わせ、1) 削除 2) 通常フィールド更新 3) フィールド変換(increment等)
    // の順序で適用する。
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
            Value::ServerTimestamp => Value::Timestamp(request_time.get()?.clone()),
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
    map: &mut DocMap,
    current: Option<&DocMap>,
    request_time: &mut RequestTime<'_>,
) -> Result<(), Box<dyn Error>> {
    for (k, v) in map.clone() {
        super::reject_nested_sentinels(&v, false)?;
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
            Value::ServerTimestamp => Value::Timestamp(request_time.get()?.clone()),
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

/// フィールドパスでネストした値を削除する。パスが存在しなければ何もしない。
/// 中間が存在して Map でなければエラー(insert と同じ厳しさ)。
fn remove_at_field_path(
    map: &mut DocMap,
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
fn lookup_field_path<'a>(map: Option<&'a DocMap>, segments: &[&str]) -> Option<&'a Value> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_commit_keeps_transaction_until_abort() {
        let mut fs = InMemoryFirestore::default();
        fs.apply(vec![WriteOp::Set {
            path: "users/existing".into(),
            data: BTreeMap::from([("n".into(), Value::Int(1))]),
            merge: false,
        }])
        .unwrap();
        fs.begin_transaction().unwrap();

        let err = fs
            .commit_transaction(vec![WriteOp::Update {
                path: "users/missing".into(),
                data: BTreeMap::from([("n".into(), Value::Int(2))]),
            }])
            .unwrap_err();
        assert!(err.to_string().contains("document not found"));
        // The failed commit did not consume the snapshot, so cleanup can
        // still be performed and a new transaction can be started afterward.
        assert!(fs.transaction_get("users/existing").unwrap().is_some());

        fs.abort_transaction().unwrap();
        fs.begin_transaction().unwrap();
        fs.abort_transaction().unwrap();
    }

    #[test]
    fn failed_batch_apply_leaves_prior_docs_unchanged() {
        let mut fs = InMemoryFirestore::default();
        fs.apply(vec![WriteOp::Set {
            path: "users/a".into(),
            data: BTreeMap::from([("n".into(), Value::Int(1))]),
            merge: false,
        }])
        .unwrap();

        let err = fs
            .apply(vec![
                WriteOp::Update {
                    path: "users/a".into(),
                    data: BTreeMap::from([("n".into(), Value::Int(99))]),
                },
                WriteOp::Update {
                    path: "users/missing".into(),
                    data: BTreeMap::from([("n".into(), Value::Int(2))]),
                },
            ])
            .unwrap_err();
        assert!(err.to_string().contains("document not found"));
        assert_eq!(
            fs.get("users/a").unwrap(),
            Some(Value::Map(BTreeMap::from([("n".into(), Value::Int(1))])))
        );
    }

    #[test]
    fn apply_keeps_shared_payload_for_untouched_docs() {
        let mut fs = InMemoryFirestore::default();
        let payload = "x".repeat(8 * 1024);
        fs.apply(vec![
            WriteOp::Set {
                path: "users/a".into(),
                data: BTreeMap::from([("payload".into(), Value::Str(payload.clone()))]),
                merge: false,
            },
            WriteOp::Set {
                path: "users/b".into(),
                data: BTreeMap::from([("payload".into(), Value::Str(payload))]),
                merge: false,
            },
        ])
        .unwrap();

        let untouched = Arc::as_ptr(fs.docs.get("users/a").unwrap());
        let updated_before = Arc::as_ptr(fs.docs.get("users/b").unwrap());
        fs.apply(vec![WriteOp::Update {
            path: "users/b".into(),
            data: BTreeMap::from([("n".into(), Value::Int(1))]),
        }])
        .unwrap();
        assert_eq!(
            Arc::as_ptr(fs.docs.get("users/a").unwrap()),
            untouched,
            "untouched document payload must stay shared across apply"
        );
        assert_ne!(
            Arc::as_ptr(fs.docs.get("users/b").unwrap()),
            updated_before,
            "updated document must take a private payload"
        );
    }

    #[test]
    fn apply_shares_one_request_time_across_sentinels() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let clock_calls = Arc::clone(&calls);
        let mut fs = InMemoryFirestore::with_clock(move || {
            clock_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Timestamp { seconds: 42 })
        });

        fs.apply(vec![
            WriteOp::Set {
                path: "users/a".into(),
                data: BTreeMap::from([("t".into(), Value::ServerTimestamp)]),
                merge: false,
            },
            WriteOp::Set {
                path: "users/b".into(),
                data: BTreeMap::from([("t".into(), Value::ServerTimestamp)]),
                merge: false,
            },
        ])
        .unwrap();

        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let expected = Value::Timestamp(Timestamp { seconds: 42 });
        assert_eq!(
            fs.get("users/a").unwrap(),
            Some(Value::Map(BTreeMap::from([("t".into(), expected.clone())])))
        );
        assert_eq!(
            fs.get("users/b").unwrap(),
            Some(Value::Map(BTreeMap::from([("t".into(), expected)])))
        );
    }
}
