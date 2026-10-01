//! Firestore SDK adapter for the local emulator (`FIRESTORE_EMULATOR_HOST`).

mod client;
mod codec;
mod path;
mod writes;

use std::env::{self, VarError};
use std::error::Error;
use std::time::Duration;

use gcloud_sdk::google::firestore::v1::{
    batch_get_documents_request, batch_get_documents_response, run_aggregation_query_request,
    run_query_request, structured_aggregation_query, structured_query, value as firestore_value,
    BatchGetDocumentsRequest, BeginTransactionRequest, GetDocumentRequest, RollbackRequest,
    RunAggregationQueryRequest, RunQueryRequest, StructuredAggregationQuery, StructuredQuery,
};
use gcloud_sdk::tonic::{Code, Request};

use super::{apply_query_shape, Firestore, QueryCandidates, WriteOp};
use crate::value::{QueryFilter, QueryOp, QueryRef, QuerySource, Value};

const COUNT_ALIAS: &str = "all";

pub use client::EmulatorAuth;

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
    // `Some` means the server-side transaction is still eligible for
    // rollback. It is cleared only after commit or rollback succeeds.
    //
    // State transitions:
    //   None --begin--> Some --commit/abort success--> None
    //   Some --preparation/commit/abort failure--> Some
    //   None --abort--> None
    transaction_id: Option<Vec<u8>>,
}

/// A validated emulator configuration. Holding this costs nothing and opens no
/// socket, so it can be obtained before the CLI blocks on script input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmulatorConfig {
    pub host: String,
    pub project_id: String,
    pub operation_timeout: Duration,
}

impl Default for EmulatorConfig {
    fn default() -> Self {
        Self {
            host: String::new(),
            project_id: String::new(),
            operation_timeout: DEFAULT_OPERATION_TIMEOUT,
        }
    }
}

/// Public so a library consumer can `downcast_ref` a connection failure out of
/// the `Box<dyn Error>` returned by [`FirestoreEmulator::new`] instead of
/// string-matching the message.
#[derive(Debug)]
pub struct EmulatorConnectionError {
    pub host: String,
    source: Box<dyn Error>,
}

impl std::fmt::Display for EmulatorConnectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "failed to connect to Firestore emulator at {}: {}",
            self.host, self.source
        )
    }
}

impl Error for EmulatorConnectionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

impl FirestoreEmulator {
    /// Reads the emulator configuration from the environment.
    ///
    /// This compatibility constructor preserves the historical `Option` API.
    /// Call [`Self::try_from_env`] when configuration and connection errors
    /// must be surfaced to the caller.
    pub fn from_env() -> Option<Self> {
        Self::try_from_env().ok().flatten()
    }

    /// `FIRESTORE_EMULATOR_HOST` から作る。`auth` で資格情報を選ぶ。
    /// rules 検証用に EnforceRules で接続したいテスト向け。
    pub fn from_env_with_auth(auth: client::EmulatorAuth) -> Option<Self> {
        let config = Self::config_from_env().ok()??;
        Self::new_with_auth_and_timeout(
            config.host,
            config.project_id,
            auth,
            config.operation_timeout,
        )
        .ok()
    }

    /// Fallible environment constructor. `Ok(None)` means that
    /// `FIRESTORE_EMULATOR_HOST` is not set; a configured but invalid or
    /// unreachable emulator is returned as an error.
    pub fn try_from_env() -> Result<Option<Self>, Box<dyn Error>> {
        match Self::config_from_env()? {
            None => Ok(None),
            Some(config) => Ok(Some(Self::new_with_timeout(
                config.host,
                config.project_id,
                config.operation_timeout,
            )?)),
        }
    }

    /// Parse and validate the emulator configuration from the environment
    /// without opening a connection.
    ///
    /// This is deliberately separate from [`FirestoreEmulator::try_from_env`]:
    /// validation is pure and instant, so a CLI can reject a misconfiguration
    /// before it blocks on reading a script from stdin, while the actual
    /// network connect is deferred until the client is first needed.
    /// A malformed `FIRESTORE_EMULATOR_TIMEOUT_MS` is reported instead of
    /// silently treated as an unavailable emulator.
    pub fn config_from_env() -> Result<Option<EmulatorConfig>, Box<dyn Error>> {
        let host = match env::var("FIRESTORE_EMULATOR_HOST") {
            Ok(host) => host,
            Err(VarError::NotPresent) => return Ok(None),
            Err(VarError::NotUnicode(_)) => {
                return Err("FIRESTORE_EMULATOR_HOST must be valid UTF-8".into());
            }
        };
        // Validate both variables here, not in `new`: this is the pure,
        // instant half of the setup, and the CLI relies on it to reject every
        // misconfiguration before it blocks on script input. `new` re-validates
        // so it stays safe for direct callers.
        validate_config("FIRESTORE_EMULATOR_HOST", &host)?;

        let project_id = match env::var("FIRESTORE_PROJECT_ID") {
            Ok(project_id) => project_id,
            Err(VarError::NotPresent) => "firelang-test".to_string(),
            Err(VarError::NotUnicode(_)) => {
                return Err("FIRESTORE_PROJECT_ID must be valid UTF-8".into());
            }
        };
        validate_config("FIRESTORE_PROJECT_ID", &project_id)?;

        Ok(Some(EmulatorConfig {
            host,
            project_id,
            operation_timeout: operation_timeout_from_env()?,
        }))
    }

    /// Construct using the configured application default.
    pub fn new(host: String, project_id: String) -> Result<Self, Box<dyn Error>> {
        Self::new_with_timeout(host, project_id, DEFAULT_OPERATION_TIMEOUT)
    }

    /// 接続を確立して作る。`auth = EnforceRules` なら admin 資格を使わず
    /// security rules を評価させる(deploy rules の検証用)。
    pub fn new_with_auth(
        host: String,
        project_id: String,
        auth: client::EmulatorAuth,
    ) -> Result<Self, Box<dyn Error>> {
        Self::new_with_auth_and_timeout(host, project_id, auth, DEFAULT_OPERATION_TIMEOUT)
    }

    /// auth と timeout の両方を指定する完全版。
    pub fn new_with_auth_and_timeout(
        host: String,
        project_id: String,
        auth: client::EmulatorAuth,
        operation_timeout: Duration,
    ) -> Result<Self, Box<dyn Error>> {
        validate_config("FIRESTORE_EMULATOR_HOST", &host)?;
        validate_config("FIRESTORE_PROJECT_ID", &project_id)?;

        let client = match client::connect(&host, &project_id, auth, operation_timeout) {
            Ok(client) => client,
            Err(source) => {
                return Err(Box::new(EmulatorConnectionError { host, source }));
            }
        };
        Ok(Self {
            client,
            transaction_id: None,
        })
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
        Self::new_with_auth_and_timeout(
            host,
            project_id,
            client::EmulatorAuth::Owner,
            options.operation_timeout,
        )
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

fn validate_config(name: &str, value: &str) -> Result<(), Box<dyn Error>> {
    if value.trim().is_empty() {
        return Err(format!("{name} must not be empty").into());
    }
    Ok(())
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
            Ok(resp) => {
                codec::decode_document(&resp.into_inner(), self.client.db.get_documents_path())
                    .map(Some)
            }
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
        // Firestore の server filter は候補を減らすためだけに使ってよい。
        // 最終的な一致判定は必ず共通の local matcher で行う(特に異型の
        // 大小比較を Firestore の型順序に委ねない)。
        let server_filters = server_prefilters(&query.filters);
        match &query.source {
            QuerySource::Collection(path) => {
                let (parent, collection_id) = path::split_collection_path(path)?;
                let sq = structured_query(
                    &self.client.project_id,
                    collection_id,
                    false,
                    &server_filters,
                )?;
                run_query_candidates(self, parent.as_deref(), sq, &query.filters)
            }
            QuerySource::CollectionGroup(id) => {
                let sq = structured_query(&self.client.project_id, id, true, &server_filters)?;

                run_query_candidates(self, None, sq, &query.filters)
            }
        }
    }

    fn run_query_page(&self, query: &QueryRef) -> Result<QueryCandidates, Box<dyn Error>> {
        super::validate_query_filters(&query.filters)?;
        let server_filters = server_prefilters(&query.filters);
        let (parent, mut sq) = match &query.source {
            QuerySource::Collection(path) => {
                let (parent, collection_id) = path::split_collection_path(path)?;
                (
                    parent,
                    structured_query(
                        &self.client.project_id,
                        collection_id,
                        false,
                        &server_filters,
                    )?,
                )
            }
            QuerySource::CollectionGroup(id) => (
                None,
                structured_query(&self.client.project_id, id, true, &server_filters)?,
            ),
        };
        if query.limit == Some(0) {
            return Ok(vec![]);
        }
        let parent = parent.as_deref();

        if can_push_path_page(query) {
            push_path_page(query, &mut sq)?;
            run_query_candidates(self, parent, sq, &query.filters)
        } else {
            let rows = run_query_candidates(self, parent, sq, &query.filters)?;
            Ok(apply_query_shape(rows, query))
        }
    }

    fn count_query(&self, query: &QueryRef) -> Result<i64, Box<dyn Error>> {
        super::validate_query_filters(&query.filters)?;
        let server_filters = server_prefilters(&query.filters);
        let (parent, aggregation) = match &query.source {
            QuerySource::Collection(path) => {
                let (parent, collection_id) = path::split_collection_path(path)?;
                let sq = structured_query(
                    &self.client.project_id,
                    collection_id,
                    false,
                    &server_filters,
                )?;
                (parent, count_aggregation_query(sq)?)
            }
            QuerySource::CollectionGroup(id) => {
                let sq = structured_query(&self.client.project_id, id, true, &server_filters)?;
                (None, count_aggregation_query(sq)?)
            }
        };

        if let Some(count) = run_count_aggregation(self, parent.as_deref(), aggregation)? {
            return Ok(count);
        }
        // 古い emulator が aggregation RPC を実装しない場合は、__name__ だけを
        // 返す projection query へフォールバックし、全フィールドを materialize
        // せずに件数を取得する。
        run_count_fallback(self, query)
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
        result
            .as_ref()
            .map(|doc| codec::decode_document(doc, self.client.db.get_documents_path()))
            .transpose()
    }

    fn commit_transaction(&mut self, writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>> {
        // Keep the transaction id until the whole commit succeeds.  In
        // particular, write preparation can fail before the Commit RPC is
        // sent, and the runtime will call abort_transaction for that error.
        // Taking the id here would make that rollback a no-op and leave the
        // server-side transaction open. A timeout also keeps the id: the
        // commit may have applied server-side, so the outcome stays
        // indeterminate and the runtime retries the rollback (via its
        // pending-rollback path) instead of dropping the id and risking a
        // non-idempotent retry.
        let tid = self.transaction_id.clone().ok_or("not in transaction")?;
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
        // Do not discard the id before Rollback returns.  A transient rollback
        // failure must leave enough state for a later retry; a successful
        // rollback (or an already-idle backend) clears the local state.
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
            Ok(()) => {}
            // NOT_FOUND means the server no longer holds this transaction:
            // it was already committed (a commit whose response was lost), or
            // already rolled back, or never existed.  That is terminal, not
            // retryable, so the local state must be dropped.  Propagating it
            // would wedge `Runtime::pending_rollback` and make every later
            // `transaction` block fail with "already in transaction" forever.
            // This mirrors how `get` above normalises the same status.
            Err(error) => {
                let not_found = error
                    .as_ref()
                    .downcast_ref::<gcloud_sdk::tonic::Status>()
                    .is_some_and(|status| status.code() == Code::NotFound);
                if !not_found {
                    return Err(error);
                }
            }
        }
        self.transaction_id = None;
        Ok(())
    }
}

/// サーバ側へ `__name__` の順序と offset/limit を渡せる条件。
/// 範囲比較を含む filter は Firestore が同フィールドの先頭 order_by を
/// 要求するため、既存の結果順 path へ push しない。値の範囲も proto の
/// i32 に収まる場合だけ page 全体だけを push する。
fn can_push_path_page(query: &QueryRef) -> bool {
    let has_page = query.offset.is_some() || query.limit.is_some();
    let has_order_requiring_filter = query.filters.iter().any(|filter| {
        matches!(
            filter.op,
            QueryOp::Gt | QueryOp::Ge | QueryOp::Lt | QueryOp::Le | QueryOp::Ne | QueryOp::NotIn
        )
    });
    has_page
        && matches!(query.source, QuerySource::Collection(_))
        && query.order_by.is_empty()
        && !has_order_requiring_filter
        && query.offset.unwrap_or(0) <= i32::MAX as usize
        && query.limit.is_none_or(|limit| limit <= i32::MAX as usize)
}

fn push_path_page(
    query: &QueryRef,
    structured_query: &mut StructuredQuery,
) -> Result<(), Box<dyn Error>> {
    structured_query.order_by = vec![structured_query::Order {
        field: Some(structured_query::FieldReference {
            field_path: "__name__".to_string(),
        }),
        direction: structured_query::Direction::Ascending as i32,
    }];
    structured_query.offset = i32::try_from(query.offset.unwrap_or(0))?;
    structured_query.limit = query.limit.map(i32::try_from).transpose()?;
    Ok(())
}

fn count_aggregation_query(
    query: StructuredQuery,
) -> Result<StructuredAggregationQuery, Box<dyn Error>> {
    Ok(StructuredAggregationQuery {
        aggregations: vec![structured_aggregation_query::Aggregation {
            alias: COUNT_ALIAS.to_string(),
            operator: Some(structured_aggregation_query::aggregation::Operator::Count(
                structured_aggregation_query::aggregation::Count { up_to: None },
            )),
        }],
        query_type: Some(structured_aggregation_query::QueryType::StructuredQuery(
            query,
        )),
    })
}

/// `None` は aggregation RPC 非対応、または aggregation row を返さない場合。
/// 呼び出し側はこの印字を全件クエリへのフォールバックに使う。
fn run_count_aggregation(
    fs: &FirestoreEmulator,
    parent: Option<&str>,
    query: StructuredAggregationQuery,
) -> Result<Option<i64>, Box<dyn Error>> {
    let db = &fs.client.db;
    let parent = path::parent_path(&fs.client.project_id, parent);
    let values = fs.client.runtime.block_on(async move {
        let response = match db
            .client()
            .get()
            .run_aggregation_query(Request::new(RunAggregationQueryRequest {
                parent,
                explain_options: None,
                query_type: Some(
                    run_aggregation_query_request::QueryType::StructuredAggregationQuery(query),
                ),
                consistency_selector: None,
            }))
            .await
        {
            Ok(response) => response,
            Err(status) if status.code() == Code::Unimplemented => return Ok(vec![]),
            Err(status) => return Err(status),
        };
        let mut stream = response.into_inner();
        let mut values = vec![];
        loop {
            match stream.message().await {
                Ok(Some(row)) => {
                    if let Some(result) = row.result {
                        values.extend(result.aggregate_fields.into_values());
                    }
                }
                Ok(None) => break,
                Err(status) if status.code() == Code::Unimplemented => return Ok(vec![]),
                Err(status) => return Err(status),
            }
        }
        Ok::<_, gcloud_sdk::tonic::Status>(values)
    })?;

    match values.as_slice() {
        [] => Ok(None),
        [value] => match &value.value_type {
            Some(firestore_value::ValueType::IntegerValue(count)) => Ok(Some(*count)),
            _ => Err("count aggregation must return an integer".into()),
        },
        _ => Err("count aggregation returned multiple results".into()),
    }
}

fn run_query_candidates(
    fs: &FirestoreEmulator,
    parent: Option<&str>,
    query: StructuredQuery,
    filters: &[QueryFilter],
) -> Result<QueryCandidates, Box<dyn Error>> {
    let db = &fs.client.db;
    let parent = path::parent_path(&fs.client.project_id, parent);
    let filters = filters.to_vec();
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
    let resource_root = fs.client.db.get_documents_path();
    for doc in docs {
        let rel = path::relative_document_name_for(
            &doc.name,
            &fs.client.project_id,
            path::DEFAULT_DATABASE_ID,
        )?;
        let Value::Map(fields) = codec::decode_document(&doc, resource_root)? else {
            return Err("query result must be a map".into());
        };
        if super::matches_filters(&rel, &fields, &filters) {
            out.push((rel, fields));
        }
    }
    // The server query is only a candidate-generation optimization.  Re-run
    // every predicate with the same matcher used by the in-memory backend so
    // server-specific null and structural-value behavior cannot leak through.
    out.retain(|(rel, fields)| super::matches_filters(rel, fields, &filters));
    Ok(out)
}

fn is_not_found(error: &(dyn Error + 'static)) -> bool {
    error
        .downcast_ref::<gcloud_sdk::tonic::Status>()
        .is_some_and(|status| status.code() == Code::NotFound)
}

fn server_prefilters(filters: &[QueryFilter]) -> Vec<QueryFilter> {
    filters
        .iter()
        .find(|filter| is_server_prefilter_safe(filter))
        .cloned()
        .into_iter()
        .collect()
}

fn is_server_prefilter_safe(filter: &QueryFilter) -> bool {
    // `__name__` の equality には Firestore の完全リソース名が必要で、
    // relative path を scalar string として送ると server が候補を除外する。
    if filter.field == "__name__" {
        return false;
    }
    match filter.op {
        // equality は構造比較そのものなので、encode 可能な scalar だけ使う。
        QueryOp::Eq => matches!(
            &filter.value,
            Value::Int(_) | Value::Str(_) | Value::Bool(_) | Value::Timestamp(_)
        ),
        // primitive の range だけ同じ型の一致を server 側から除外しない。
        QueryOp::Gt | QueryOp::Ge | QueryOp::Lt | QueryOp::Le => matches!(
            &filter.value,
            Value::Int(_) | Value::Str(_) | Value::Bool(_) | Value::Timestamp(_)
        ),
        // Nil / Array / Map の順序比較、membership、!= 系は server 依存に
        // しないため、候補をすべて取得して local matcher に渡す。
        QueryOp::Ne
        | QueryOp::In
        | QueryOp::NotIn
        | QueryOp::ArrayContains
        | QueryOp::ArrayContainsAny => false,
    }
}

fn run_count_fallback(fs: &FirestoreEmulator, query: &QueryRef) -> Result<i64, Box<dyn Error>> {
    let server_filters = server_prefilters(&query.filters);
    let (parent, mut sq) = match &query.source {
        QuerySource::Collection(path) => {
            let (parent, collection_id) = path::split_collection_path(path)?;
            (
                parent,
                structured_query(&fs.client.project_id, collection_id, false, &server_filters)?,
            )
        }
        QuerySource::CollectionGroup(id) => (
            None,
            structured_query(&fs.client.project_id, id, true, &server_filters)?,
        ),
    };
    sq.select = Some(structured_query::Projection {
        fields: vec![structured_query::FieldReference {
            field_path: "__name__".to_string(),
        }],
    });
    let db = &fs.client.db;
    let parent = path::parent_path(&fs.client.project_id, parent.as_deref());
    let count = fs.client.runtime.block_on(async move {
        let response = db
            .client()
            .get()
            .run_query(Request::new(RunQueryRequest {
                parent,
                explain_options: None,
                query_type: Some(run_query_request::QueryType::StructuredQuery(sq)),
                consistency_selector: None,
            }))
            .await?;
        let mut stream = response.into_inner();
        let mut count = 0_i64;
        while let Some(row) = stream.message().await? {
            if row.document.is_some() {
                count = count.checked_add(1).ok_or_else(|| {
                    gcloud_sdk::tonic::Status::resource_exhausted("query count overflow")
                })?;
            }
        }
        Ok::<_, gcloud_sdk::tonic::Status>(count)
    })?;
    Ok(count)
}

fn structured_query(
    project_id: &str,
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
        r#where: build_where(project_id, filters)?,
        order_by: vec![],
        start_at: None,
        end_at: None,
        offset: 0,
        limit: None,
        find_nearest: None,
    })
}

// `filters` is the deliberately small, server-safe subset selected by
// `server_prefilters`; all original filters are checked again locally.
fn build_where(
    project_id: &str,
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
                    value: Some(codec::encode_query_value(project_id, &f.value)?),
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
        // OS に一時的に確保させ、接続していない loopback port を取得する。
        // discard port のように「通常誰も使わない」固定 port には依存しない。
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let host = listener.local_addr().unwrap().to_string();
        drop(listener);

        // 接続は構築時に確立する。届かない相手には connect deadline までに
        // エラーになる。
        let res = FirestoreEmulator::new_with_timeout(
            host,
            "p".to_string(),
            std::time::Duration::from_millis(50),
        );
        match res {
            Ok(_) => panic!("expected connection failure"),
            Err(e) => assert!(!format!("{e}").is_empty()),
        }
    }

    fn query() -> QueryRef {
        QueryRef {
            source: QuerySource::Collection("users".to_string()),
            filters: vec![],
            order_by: vec![],
            offset: Some(2),
            limit: Some(3),
        }
    }

    #[test]
    fn path_page_pushdown_is_limited_to_semantically_safe_queries() {
        let mut query = query();
        assert!(can_push_path_page(&query));

        let mut sq = structured_query("p", "users", false, &[]).unwrap();
        push_path_page(&query, &mut sq).unwrap();
        assert_eq!(sq.offset, 2);
        assert_eq!(sq.limit, Some(3));
        assert_eq!(sq.order_by.len(), 1);
        assert_eq!(
            sq.order_by[0]
                .field
                .as_ref()
                .map(|field| field.field_path.as_str()),
            Some("__name__")
        );

        query.filters.push(QueryFilter {
            field: "age".to_string(),
            op: QueryOp::Ge,
            value: Value::Int(20),
        });
        assert!(!can_push_path_page(&query));

        query.filters.clear();
        query
            .order_by
            .push(("ord".to_string(), crate::value::Direction::Asc));
        assert!(!can_push_path_page(&query));
    }

    #[test]
    fn count_aggregation_uses_unshaped_count_star_query() {
        let aggregation =
            count_aggregation_query(structured_query("p", "users", false, &[]).unwrap()).unwrap();
        assert_eq!(aggregation.aggregations.len(), 1);
        assert_eq!(aggregation.aggregations[0].alias, COUNT_ALIAS);
        assert!(matches!(
            aggregation.aggregations[0].operator.as_ref(),
            Some(structured_aggregation_query::aggregation::Operator::Count(
                structured_aggregation_query::aggregation::Count { up_to: None }
            ))
        ));
        let Some(structured_aggregation_query::QueryType::StructuredQuery(query)) =
            aggregation.query_type
        else {
            panic!("expected structured count query");
        };
        assert_eq!(query.offset, 0);
        assert_eq!(query.limit, None);
        assert!(query.order_by.is_empty());
    }

    #[test]
    fn build_where_encodes_doc_refs_for_reference_operators() {
        let ref_value = || {
            Value::DocRef(crate::value::DocRef {
                path: "users/owner".to_string(),
            })
        };
        let cases = [
            (QueryOp::Eq, ref_value()),
            (QueryOp::In, Value::Array(vec![ref_value()])),
            (QueryOp::ArrayContains, ref_value()),
        ];
        for (op, value) in cases {
            let filter = build_where(
                "query-project",
                &[QueryFilter {
                    field: "owner".to_string(),
                    op,
                    value,
                }],
            )
            .unwrap()
            .unwrap();
            let Some(structured_query::filter::FilterType::FieldFilter(field_filter)) =
                filter.filter_type
            else {
                panic!("expected field filter for {op:?}");
            };
            let encoded = field_filter.value.expect("filter value");
            match op {
                QueryOp::In => {
                    let Some(gcloud_sdk::google::firestore::v1::value::ValueType::ArrayValue(
                        values,
                    )) = encoded.value_type
                    else {
                        panic!("expected array filter value");
                    };
                    assert_eq!(values.values.len(), 1);
                    assert_eq!(
                        values.values[0].value_type,
                        Some(
                            gcloud_sdk::google::firestore::v1::value::ValueType::ReferenceValue(
                                "projects/query-project/databases/(default)/documents/users/owner"
                                    .to_string()
                            )
                        )
                    );
                }
                QueryOp::Eq | QueryOp::ArrayContains => assert_eq!(
                    encoded.value_type,
                    Some(
                        gcloud_sdk::google::firestore::v1::value::ValueType::ReferenceValue(
                            "projects/query-project/databases/(default)/documents/users/owner"
                                .to_string()
                        )
                    )
                ),
                other => panic!("unexpected test case: {other:?}"),
            }
        }
    }
}
