//! Firestore SDK adapter for the local emulator (`FIRESTORE_EMULATOR_HOST`).

mod client;
mod codec;
mod path;
mod writes;

use std::error::Error;
use std::time::Duration;

use gcloud_sdk::google::firestore::v1::{
    batch_get_documents_request, batch_get_documents_response, run_query_request, structured_query,
    BatchGetDocumentsRequest, BeginTransactionRequest, Document, GetDocumentRequest,
    RollbackRequest, RunQueryRequest, StructuredQuery,
};
use gcloud_sdk::tonic::{Code, Request};

use super::{Firestore, QueryCandidates, WriteOp};
use crate::value::{QueryFilter, QueryOp, QueryRef, QuerySource, Value};

pub struct FirestoreEmulator {
    client: client::EmulatorClient,
    transaction_id: Option<Vec<u8>>,
}

impl FirestoreEmulator {
    pub fn from_env() -> Option<Self> {
        let host = std::env::var("FIRESTORE_EMULATOR_HOST").ok()?;
        let project_id =
            std::env::var("FIRESTORE_PROJECT_ID").unwrap_or_else(|_| "firelang-test".to_string());
        Self::new(host, project_id).ok()
    }

    /// 接続を確立して作る。emulator に届かない場合はエラーを返す。
    pub fn new(host: String, project_id: String) -> Result<Self, Box<dyn Error>> {
        Self::new_with_timeout(
            host,
            project_id,
            Some(Duration::from_millis(
                crate::limits::DEFAULT_BACKEND_TIMEOUT_MS,
            )),
        )
    }

    /// Construct an adapter with the deadline applied to connection setup as
    /// well as every subsequent RPC. `None` explicitly disables deadlines.
    pub fn new_with_timeout(
        host: String,
        project_id: String,
        operation_timeout: Option<Duration>,
    ) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            client: client::connect(&host, &project_id, operation_timeout)?,
            transaction_id: None,
        })
    }

    /// Return the currently configured per-operation deadline.
    ///
    /// `None` is meaningful: it is the explicit unlimited setting used by
    /// `RuntimeLimits::unlimited()` and disables the adapter deadline.
    pub fn operation_timeout(&self) -> Option<Duration> {
        self.client.operation_timeout
    }

    /// Set the per-operation deadline used by all subsequent emulator RPCs.
    /// This is also the hook used by `Runtime` when it propagates
    /// `RuntimeLimits::backend_timeout`.
    pub fn set_operation_timeout(&mut self, timeout: Option<Duration>) {
        self.client.operation_timeout = timeout;
    }
}

impl Firestore for FirestoreEmulator {
    fn get(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>> {
        let db = &self.client.db;
        let name = path::document_name(&self.client.project_id, path);
        let result = client::run_operation(
            &self.client.runtime,
            self.client.operation_timeout,
            "get",
            async move {
                db.client()
                    .get()
                    .get_document(Request::new(GetDocumentRequest {
                        name,
                        mask: None,
                        consistency_selector: None,
                    }))
                    .await
            },
        );
        match result {
            Ok(resp) => codec::decode_document(&resp.into_inner()).map(Some),
            Err(error) if is_not_found(error.as_ref()) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn apply(&mut self, writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>> {
        let sdk_writes = writes::write_ops_to_sdk_writes(&self.client.project_id, &writes)?;
        writes::commit_writes(
            &self.client.db,
            &self.client.runtime,
            self.client.operation_timeout,
            sdk_writes,
            vec![],
        )
    }

    fn run_query(&self, query: &QueryRef) -> Result<QueryCandidates, Box<dyn Error>> {
        super::validate_query_filters(&query.filters)?;
        match &query.source {
            QuerySource::Collection(path) => {
                let (parent, collection_id) = path::split_collection_path(path)?;
                let sq = structured_query(collection_id, false, &query.filters)?;
                run_query_candidates(self, parent.as_deref(), sq)
            }
            QuerySource::CollectionGroup(id) => {
                let sq = structured_query(id, true, &query.filters)?;
                run_query_candidates(self, None, sq)
            }
        }
    }

    fn run_query_bounded(
        &self,
        query: &QueryRef,
        max_results: usize,
    ) -> Result<QueryCandidates, Box<dyn Error>> {
        super::validate_query_filters(&query.filters)?;
        let (parent, sq) = match &query.source {
            QuerySource::Collection(path) => {
                let (parent, collection_id) = path::split_collection_path(path)?;
                let sq = structured_query(collection_id, false, &query.filters)?;
                (parent, sq)
            }
            QuerySource::CollectionGroup(id) => {
                let sq = structured_query(id, true, &query.filters)?;
                (None, sq)
            }
        };
        run_query_candidates_bounded(self, parent.as_deref(), sq, max_results)
    }

    fn set_timeout(&mut self, timeout: Option<Duration>) {
        self.set_operation_timeout(timeout);
    }

    fn supports_backend_timeout(&self) -> bool {
        true
    }

    fn begin_transaction(&mut self) -> Result<(), Box<dyn Error>> {
        if self.transaction_id.is_some() {
            return Err("already in transaction".into());
        }
        let db = &self.client.db;
        let database = db.get_database_path().clone();
        let transaction_id = client::run_operation(
            &self.client.runtime,
            self.client.operation_timeout,
            "transaction",
            async move {
                let resp = db
                    .client()
                    .get()
                    .begin_transaction(Request::new(BeginTransactionRequest {
                        database,
                        options: None,
                    }))
                    .await?;
                Ok::<_, gcloud_sdk::tonic::Status>(resp.into_inner().transaction)
            },
        )?;
        self.transaction_id = Some(transaction_id);
        Ok(())
    }

    fn transaction_get(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>> {
        let tid = self.transaction_id.clone().ok_or("not in transaction")?;
        let db = &self.client.db;
        let database = db.get_database_path().clone();
        let document = path::document_name(&self.client.project_id, path);
        let result = client::run_operation(
            &self.client.runtime,
            self.client.operation_timeout,
            "transaction_get",
            async move {
                let response = db
                    .client()
                    .get()
                    .batch_get_documents(Request::new(BatchGetDocumentsRequest {
                        database,
                        documents: vec![document],
                        mask: None,
                        consistency_selector: Some(
                            batch_get_documents_request::ConsistencySelector::Transaction(tid),
                        ),
                    }))
                    .await?;
                let mut stream = response.into_inner();
                while let Some(row) = stream.message().await? {
                    match row.result {
                        Some(batch_get_documents_response::Result::Found(doc)) => {
                            return Ok(Some(doc));
                        }
                        Some(batch_get_documents_response::Result::Missing(_)) => return Ok(None),
                        None => {}
                    }
                }
                Ok::<_, gcloud_sdk::tonic::Status>(None)
            },
        )?;
        result.as_ref().map(codec::decode_document).transpose()
    }

    fn commit_transaction(&mut self, writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>> {
        let tid = self
            .transaction_id
            .clone()
            .ok_or_else(|| "not in transaction".to_string())?;
        let sdk_writes = writes::write_ops_to_sdk_writes(&self.client.project_id, &writes)?;
        let result = writes::commit_writes(
            &self.client.db,
            &self.client.runtime,
            self.client.operation_timeout,
            sdk_writes,
            tid,
        );
        if result.is_ok() {
            self.transaction_id = None;
        }
        result
    }

    fn abort_transaction(&mut self) -> Result<(), Box<dyn Error>> {
        let Some(tid) = self.transaction_id.clone() else {
            return Ok(());
        };
        let db = &self.client.db;
        let database = db.get_database_path().clone();
        let result = client::run_operation(
            &self.client.runtime,
            self.client.operation_timeout,
            "transaction_rollback",
            async move {
                db.client()
                    .get()
                    .rollback(Request::new(RollbackRequest {
                        database,
                        transaction: tid,
                    }))
                    .await?;
                Ok::<_, gcloud_sdk::tonic::Status>(())
            },
        );
        match result {
            Ok(()) => {
                self.transaction_id = None;
                Ok(())
            }
            // A commit can have been accepted even when its response timed
            // out. If rollback reports that the transaction is already gone,
            // treat the cleanup as complete rather than retaining a dead ID.
            Err(error) if is_not_found(error.as_ref()) => {
                self.transaction_id = None;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}

fn run_query_candidates(
    fs: &FirestoreEmulator,
    parent: Option<&str>,
    query: StructuredQuery,
) -> Result<QueryCandidates, Box<dyn Error>> {
    let (docs, _) = run_query_candidate_documents(fs, parent, query, None)?;
    decode_query_documents(docs)
}

/// Consume a server-streaming query while keeping at most `max_results`
/// document values in memory.  One additional row is enough to prove that the
/// budget was exceeded; it is dropped without being decoded or retained, and
/// dropping the stream cancels the remaining RPC.
fn run_query_candidates_bounded(
    fs: &FirestoreEmulator,
    parent: Option<&str>,
    query: StructuredQuery,
    max_results: usize,
) -> Result<QueryCandidates, Box<dyn Error>> {
    let (docs, exceeded) = run_query_candidate_documents(fs, parent, query, Some(max_results))?;
    if exceeded {
        return Err(super::query_result_limit_error(max_results));
    }
    decode_query_documents(docs)
}

/// Return raw documents and whether a document beyond the bounded result set
/// was observed. The client-side collector is deliberately independent of
/// the query's user-visible `limit`/ordering shape, so it cannot accidentally
/// turn a bounded check into a partial or differently ordered result.
struct QueryDocumentCollector {
    max_results: Option<usize>,
    docs: Vec<Document>,
    exceeded: bool,
}

impl QueryDocumentCollector {
    fn new(max_results: Option<usize>) -> Self {
        Self {
            max_results,
            docs: Vec::new(),
            exceeded: false,
        }
    }

    /// Add one document, returning `false` when the caller should stop
    /// consuming the stream.  The overflowing document is deliberately not
    /// retained.
    fn push(&mut self, doc: Document) -> bool {
        if self
            .max_results
            .is_some_and(|max_results| self.docs.len() >= max_results)
        {
            self.exceeded = true;
            return false;
        }
        self.docs.push(doc);
        true
    }

    fn into_parts(self) -> (Vec<Document>, bool) {
        (self.docs, self.exceeded)
    }
}

fn run_query_candidate_documents(
    fs: &FirestoreEmulator,
    parent: Option<&str>,
    query: StructuredQuery,
    max_results: Option<usize>,
) -> Result<(Vec<Document>, bool), Box<dyn Error>> {
    let db = &fs.client.db;
    let parent = path::parent_path(&fs.client.project_id, parent);
    let timeout = fs.client.operation_timeout;
    client::run_operation(&fs.client.runtime, timeout, "query", async move {
        let response = db
            .client()
            .get()
            .run_query(Request::new(RunQueryRequest {
                parent,
                explain_options: None,
                query_type: Some(run_query_request::QueryType::StructuredQuery(query)),
                consistency_selector: None,
            }))
            .await?;
        let mut stream = response.into_inner();
        let mut collector = QueryDocumentCollector::new(max_results);
        while let Some(row) = stream.message().await? {
            let Some(doc) = row.document else {
                continue;
            };
            if !collector.push(doc) {
                // The collector has already retained max_results documents.
                // It deliberately drops this one and the stream is dropped
                // when this future returns.
                break;
            }
        }
        Ok::<_, gcloud_sdk::tonic::Status>(collector.into_parts())
    })
}

fn decode_query_documents(docs: Vec<Document>) -> Result<QueryCandidates, Box<dyn Error>> {
    let mut out = Vec::with_capacity(docs.len());
    for doc in docs {
        let rel = path::relative_document_name(&doc.name)?;
        let Value::Map(fields) = codec::decode_document(&doc)? else {
            return Err("query result must be a map".into());
        };
        out.push((rel, fields));
    }
    Ok(out)
}

fn is_not_found(error: &(dyn Error + 'static)) -> bool {
    error
        .downcast_ref::<gcloud_sdk::tonic::Status>()
        .is_some_and(|status| status.code() == Code::NotFound)
}

fn structured_query(
    collection_id: &str,
    all_descendants: bool,
    filters: &[QueryFilter],
) -> Result<StructuredQuery, Box<dyn Error>> {
    Ok(StructuredQuery {
        select: None,
        from: vec![structured_query::CollectionSelector {
            collection_id: collection_id.to_string(),
            all_descendants,
        }],
        r#where: build_where(filters)?,
        order_by: vec![],
        start_at: None,
        end_at: None,
        offset: 0,
        limit: None,
        find_nearest: None,
    })
}

fn build_where(
    filters: &[QueryFilter],
) -> Result<Option<structured_query::Filter>, Box<dyn Error>> {
    let mut parts = vec![];
    for f in filters {
        let op = match f.op {
            QueryOp::Eq => structured_query::field_filter::Operator::Equal,
            QueryOp::Ne => structured_query::field_filter::Operator::NotEqual,
            QueryOp::Gt => structured_query::field_filter::Operator::GreaterThan,
            QueryOp::Ge => structured_query::field_filter::Operator::GreaterThanOrEqual,
            QueryOp::Lt => structured_query::field_filter::Operator::LessThan,
            QueryOp::Le => structured_query::field_filter::Operator::LessThanOrEqual,
            QueryOp::In => structured_query::field_filter::Operator::In,
            QueryOp::NotIn => structured_query::field_filter::Operator::NotIn,
            QueryOp::ArrayContains => structured_query::field_filter::Operator::ArrayContains,
            QueryOp::ArrayContainsAny => structured_query::field_filter::Operator::ArrayContainsAny,
        };
        parts.push(structured_query::Filter {
            filter_type: Some(structured_query::filter::FilterType::FieldFilter(
                structured_query::FieldFilter {
                    field: Some(structured_query::FieldReference {
                        field_path: f.field.clone(),
                    }),
                    op: op.into(),
                    value: Some(codec::encode_field_value(&f.value)?),
                },
            )),
        });
    }
    Ok(match parts.len() {
        0 => None,
        1 => parts.pop(),
        _ => Some(structured_query::Filter {
            filter_type: Some(structured_query::filter::FilterType::CompositeFilter(
                structured_query::CompositeFilter {
                    op: structured_query::composite_filter::Operator::And.into(),
                    filters: parts,
                },
            )),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_fails_fast_without_emulator() {
        // 接続は構築時に確立する。届かない相手には即エラーになる。
        // (9番ポートは discard のため emulator がいることはない)
        let res = FirestoreEmulator::new("127.0.0.1:9".to_string(), "p".to_string());
        match res {
            Ok(_) => panic!("expected connection failure"),
            Err(e) => assert!(!format!("{e}").is_empty()),
        }
    }

    #[test]
    fn bounded_document_collector_drops_the_first_over_budget_row() {
        let mut collector = QueryDocumentCollector::new(Some(2));
        assert!(collector.push(Document::default()));
        assert!(collector.push(Document::default()));
        assert!(!collector.push(Document::default()));
        assert!(!collector.push(Document::default()));

        let (docs, exceeded) = collector.into_parts();
        assert_eq!(docs.len(), 2);
        assert!(exceeded);
    }
}
