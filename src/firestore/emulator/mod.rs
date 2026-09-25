//! Firestore SDK adapter for the local emulator (`FIRESTORE_EMULATOR_HOST`).

mod client;
mod codec;
mod path;
mod writes;

use std::error::Error;
use std::time::Duration;

use gcloud_sdk::google::firestore::v1::{
    batch_get_documents_request, batch_get_documents_response, run_query_request, structured_query,
    BatchGetDocumentsRequest, BeginTransactionRequest, GetDocumentRequest, RollbackRequest,
    RunQueryRequest, StructuredQuery,
};
use gcloud_sdk::tonic::{Code, Request};

use super::{Firestore, QueryCandidates, WriteOp};
use crate::value::{QueryFilter, QueryOp, QueryRef, QuerySource, Value};

/// Default application-level deadline for one emulator RPC.
///
/// The deadline is deliberately owned by the firelang adapter rather than the
/// SDK transport.  A timed-out future is dropped, while the current-thread
/// runtime and SDK client remain available for the next operation.
pub const DEFAULT_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);

/// Environment variable used by [`FirestoreEmulator::from_env`] and the CLI
/// for an application-level per-operation timeout, in milliseconds.
pub const OPERATION_TIMEOUT_ENV: &str = "FIRESTORE_EMULATOR_TIMEOUT_MS";

/// Optional constructor settings for [`FirestoreEmulator`].
#[derive(Debug, Clone, Copy)]
pub struct FirestoreEmulatorOptions {
    pub operation_timeout: Duration,
}

impl Default for FirestoreEmulatorOptions {
    fn default() -> Self {
        Self {
            operation_timeout: DEFAULT_OPERATION_TIMEOUT,
        }
    }
}

impl FirestoreEmulatorOptions {
    pub fn with_operation_timeout(mut self, operation_timeout: Duration) -> Self {
        self.operation_timeout = operation_timeout;
        self
    }
}

pub struct FirestoreEmulator {
    client: client::EmulatorClient,
    transaction_id: Option<Vec<u8>>,
}

impl FirestoreEmulator {
    /// Construct from emulator environment variables using the default
    /// timeout, or `FIRESTORE_EMULATOR_TIMEOUT_MS` when it is set.
    pub fn from_env() -> Option<Self> {
        Self::try_from_env().ok().flatten()
    }

    /// Fallible form of [`Self::from_env`].  It is useful to embedding callers
    /// that want a malformed timeout/host configuration to be reported instead
    /// of silently treated as an unavailable emulator.
    pub fn try_from_env() -> Result<Option<Self>, Box<dyn Error>> {
        let Ok(host) = std::env::var("FIRESTORE_EMULATOR_HOST") else {
            return Ok(None);
        };
        let project_id =
            std::env::var("FIRESTORE_PROJECT_ID").unwrap_or_else(|_| "firelang-test".to_string());
        let operation_timeout = operation_timeout_from_env()?;
        Ok(Some(Self::new_with_timeout(
            host,
            project_id,
            operation_timeout,
        )?))
    }

    /// Construct using the configured application default.
    pub fn new(host: String, project_id: String) -> Result<Self, Box<dyn Error>> {
        Self::new_with_timeout(host, project_id, DEFAULT_OPERATION_TIMEOUT)
    }

    /// Construct with an embedding-level per-operation timeout override.
    pub fn new_with_timeout(
        host: String,
        project_id: String,
        operation_timeout: Duration,
    ) -> Result<Self, Box<dyn Error>> {
        Self::new_with_options(
            host,
            project_id,
            FirestoreEmulatorOptions { operation_timeout },
        )
    }

    pub fn new_with_options(
        host: String,
        project_id: String,
        options: FirestoreEmulatorOptions,
    ) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            client: client::connect(&host, &project_id, options.operation_timeout)?,
            transaction_id: None,
        })
    }

    /// Override the timeout after construction without replacing the SDK client
    /// or its reusable runtime.
    pub fn with_operation_timeout(mut self, operation_timeout: Duration) -> Self {
        self.client.operation_timeout = operation_timeout;
        self
    }

    /// Short alias for [`Self::with_operation_timeout`].
    pub fn with_timeout(self, operation_timeout: Duration) -> Self {
        self.with_operation_timeout(operation_timeout)
    }

    pub fn set_operation_timeout(&mut self, operation_timeout: Duration) {
        self.client.operation_timeout = operation_timeout;
    }

    pub fn operation_timeout(&self) -> Duration {
        self.client.operation_timeout
    }
}

fn operation_timeout_from_env() -> Result<Duration, Box<dyn Error>> {
    match std::env::var(OPERATION_TIMEOUT_ENV) {
        Ok(value) => parse_timeout_ms(&value),
        Err(std::env::VarError::NotPresent) => Ok(DEFAULT_OPERATION_TIMEOUT),
        Err(error) => Err(error.into()),
    }
}

pub fn parse_timeout_ms(value: &str) -> Result<Duration, Box<dyn Error>> {
    let millis = value.trim().parse::<u64>().map_err(|_| {
        format!("{OPERATION_TIMEOUT_ENV} must be a positive integer number of milliseconds")
    })?;
    if millis == 0 {
        return Err(format!("{OPERATION_TIMEOUT_ENV} must be greater than zero").into());
    }
    Ok(Duration::from_millis(millis))
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
        // transaction id は結果が決まるまで `self` に残す。
        // 先に take() すると、SDK 変換の失敗や commit の timeout で
        // server 側の transaction が孤児になり、`abort_transaction` も
        // 空振りして以後の `begin_transaction` が "already in transaction"
        // で失敗し続ける。timeout の場合は commit が server 側で適用済み
        // かもしれないので、その tx id を失うと結果の曖昧さを解消できない。
        let tid = self.transaction_id.clone().ok_or("not in transaction")?;
        let sdk_writes = writes::write_ops_to_sdk_writes(&self.client.project_id, &writes)?;
        let result = writes::commit_writes(
            &self.client.db,
            &self.client.runtime,
            self.client.operation_timeout,
            sdk_writes,
            tid,
        );
        // 成功、または決定的な失敗では server 側の transaction は閉じている。
        // timeout のときだけは commit が server 側で適用済みかもしれないので、
        // Rollback で「未適用」と確定させてから local state を捨てる。
        // (結果不明のまま tx id を失うと、呼び出し側の再試行が二重適用に
        // つながる。Rollback の成否自体は元の timeout を壊さないので無視。)
        if matches!(&result, Err(e) if client::is_timeout_error(e.as_ref())) {
            let _ = self.abort_transaction();
        }
        self.transaction_id = None;
        result
    }

    fn abort_transaction(&mut self) -> Result<(), Box<dyn Error>> {
        let Some(tid) = self.transaction_id.take() else {
            return Ok(());
        };
        let db = &self.client.db;
        let database = db.get_database_path().clone();
        client::run_operation(
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
        )?;
        Ok(())
    }
}

fn run_query_candidates(
    fs: &FirestoreEmulator,
    parent: Option<&str>,
    query: StructuredQuery,
) -> Result<QueryCandidates, Box<dyn Error>> {
    let db = &fs.client.db;
    let parent = path::parent_path(&fs.client.project_id, parent);
    let docs = client::run_operation(
        &fs.client.runtime,
        fs.client.operation_timeout,
        "query",
        async move {
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
            let mut docs = vec![];
            while let Some(row) = stream.message().await? {
                if let Some(doc) = row.document {
                    docs.push(doc);
                }
            }
            Ok::<_, gcloud_sdk::tonic::Status>(docs)
        },
    )?;

    let mut out = vec![];
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
        let res = FirestoreEmulator::new_with_timeout(
            "127.0.0.1:9".to_string(),
            "p".to_string(),
            std::time::Duration::from_millis(50),
        );
        match res {
            Ok(_) => panic!("expected connection failure"),
            Err(e) => assert!(!format!("{e}").is_empty()),
        }
    }
}
