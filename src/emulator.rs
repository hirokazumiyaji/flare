//! Firestore SDK adapter for the local emulator (`FIRESTORE_EMULATOR_HOST`).
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::error::Error;

use firestore::{FirestoreDb, FirestoreDbOptions};
use gcloud_sdk::google::firestore::v1::{
    batch_get_documents_request, batch_get_documents_response, document_transform, precondition,
    run_query_request, structured_query, value, write, ArrayValue, BatchGetDocumentsRequest,
    BeginTransactionRequest, CommitRequest, Document, DocumentMask, GetDocumentRequest, MapValue,
    Precondition, RunQueryRequest, StructuredQuery, Value as SdkValue, Write,
};
use gcloud_sdk::tonic::{Code, Request, Status};
use prost_types::{NullValue, Timestamp as ProtoTimestamp};
use tokio::runtime::{Builder as RuntimeBuilder, Runtime};

use crate::runtime::{Firestore, QueryFilter, Timestamp, Value, WriteOp};

pub struct FirestoreEmulator {
    host: String,
    project_id: String,
    runtime: Runtime,
    db: RefCell<Option<FirestoreDb>>,
    mode: Option<EmulatorMode>,
}

enum EmulatorMode {
    Batch(Vec<WriteOp>),
    Transaction {
        transaction_id: Vec<u8>,
        writes: Vec<WriteOp>,
    },
}

impl FirestoreEmulator {
    pub fn from_env() -> Option<Self> {
        let host = std::env::var("FIRESTORE_EMULATOR_HOST").ok()?;
        let project_id =
            std::env::var("FIRESTORE_PROJECT_ID").unwrap_or_else(|_| "firelang-test".to_string());
        Some(Self::new(host, project_id))
    }

    pub fn new(host: String, project_id: String) -> Self {
        let runtime = RuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .expect("create tokio runtime for Firestore SDK");
        Self {
            host,
            project_id,
            runtime,
            db: RefCell::new(None),
            mode: None,
        }
    }

    fn sdk_db(&self) -> Result<FirestoreDb, Box<dyn Error>> {
        {
            let db = self.db.borrow();
            if let Some(db) = db.as_ref() {
                return Ok(db.clone());
            }
        }

        let options = sdk_options(&self.host, &self.project_id);
        let db = self
            .runtime
            .block_on(FirestoreDb::with_options_token_source(
                options,
                gcloud_sdk::GCP_DEFAULT_SCOPES.clone(),
                emulator_token_source(),
            ))?;
        *self.db.borrow_mut() = Some(db.clone());
        Ok(db)
    }

    fn document_name(&self, path: &str) -> String {
        format!(
            "projects/{}/databases/(default)/documents/{}",
            self.project_id, path
        )
    }

    fn parent_path(&self, relative_parent: Option<&str>) -> String {
        match relative_parent {
            Some(parent) if !parent.is_empty() => format!(
                "projects/{}/databases/(default)/documents/{}",
                self.project_id, parent
            ),
            _ => format!("projects/{}/databases/(default)/documents", self.project_id),
        }
    }

    fn commit_single_write(&self, op: WriteOp) -> Result<(), Box<dyn Error>> {
        let write = build_write(self, &op)?;
        self.commit_writes(vec![write], vec![])
    }

    fn commit_writes(
        &self,
        writes: Vec<Write>,
        transaction: Vec<u8>,
    ) -> Result<(), Box<dyn Error>> {
        let db = self.sdk_db()?;
        let database = db.get_database_path().clone();
        let not_found_context = write_targets(&writes);
        let has_exists_precondition = writes.iter().any(write_requires_existing_document);
        let result = self.runtime.block_on(async move {
            db.client()
                .get()
                .commit(Request::new(CommitRequest {
                    database,
                    writes,
                    transaction,
                }))
                .await
        });
        match result {
            Ok(_) => Ok(()),
            Err(status) if missing_document_status(&status, has_exists_precondition) => {
                Err(not_found_error(&status, not_found_context.as_deref()).into())
            }
            Err(status) => Err(status.into()),
        }
    }
}

fn missing_document_status(status: &Status, has_exists_precondition: bool) -> bool {
    status.code() == Code::NotFound
        || (status.code() == Code::FailedPrecondition && has_exists_precondition)
}

fn not_found_error(status: &Status, context: Option<&str>) -> String {
    let detail = status.message();
    match (context, detail.is_empty()) {
        (Some(context), false) => format!("document not found: {context}: {detail}"),
        (Some(context), true) => format!("document not found: {context}"),
        (None, false) => format!("document not found: {detail}"),
        (None, true) => "document not found".to_string(),
    }
}

fn write_targets(writes: &[Write]) -> Option<String> {
    let mut targets: Vec<String> = writes.iter().filter_map(write_target).collect();
    targets.sort();
    targets.dedup();
    match targets.as_slice() {
        [] => None,
        [single] => Some(single.clone()),
        _ => Some(targets.join(", ")),
    }
}

fn write_requires_existing_document(write: &Write) -> bool {
    matches!(
        write
            .current_document
            .as_ref()
            .and_then(|p| p.condition_type),
        Some(precondition::ConditionType::Exists(true))
    )
}

fn write_target(write: &Write) -> Option<String> {
    let name = match write.operation.as_ref()? {
        write::Operation::Update(doc) => &doc.name,
        write::Operation::Delete(name) => name,
        write::Operation::Transform(transform) => &transform.document,
    };
    Some(relative_document_name(name).unwrap_or_else(|_| name.clone()))
}

fn sdk_options(host: &str, project_id: &str) -> FirestoreDbOptions {
    FirestoreDbOptions::new(project_id.to_string()).with_firebase_api_url(ensure_url_scheme(host))
}

fn ensure_url_scheme(host: &str) -> String {
    if host.contains("://") {
        host.to_string()
    } else {
        format!("http://{host}")
    }
}

fn emulator_token_source() -> gcloud_sdk::TokenSourceType {
    gcloud_sdk::TokenSourceType::ExternalSource(Box::new(
        gcloud_sdk::ExternalJwtFunctionSource::new(|| {
            std::future::ready(Ok(gcloud_sdk::Token::new(
                "Bearer".to_string(),
                "owner".into(),
                chrono::DateTime::<chrono::Utc>::from_timestamp(4_102_444_800, 0)
                    .expect("valid token expiry"),
            )))
        }),
    ))
}

impl Firestore for FirestoreEmulator {
    fn get(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>> {
        if self.mode.is_some() {
            return Err("internal error: get while in batch/transaction".into());
        }
        let db = self.sdk_db()?;
        let name = self.document_name(path);
        let result = self.runtime.block_on(async move {
            db.client()
                .get()
                .get_document(Request::new(GetDocumentRequest {
                    name,
                    mask: None,
                    consistency_selector: None,
                }))
                .await
        });
        match result {
            Ok(resp) => decode_document_for_get(&resp.into_inner()).map(Some),
            Err(status) if status.code() == Code::NotFound => Ok(None),
            Err(status) => Err(status.into()),
        }
    }

    fn set(
        &mut self,
        path: &str,
        value: BTreeMap<String, Value>,
        merge: bool,
    ) -> Result<(), Box<dyn Error>> {
        if self.mode.is_some() {
            return Err("internal error: set while in batch/transaction".into());
        }
        self.commit_single_write(WriteOp::Set {
            path: path.to_string(),
            data: value,
            merge,
        })
    }

    fn update(&mut self, path: &str, value: BTreeMap<String, Value>) -> Result<(), Box<dyn Error>> {
        if self.mode.is_some() {
            return Err("internal error: update while in batch/transaction".into());
        }
        self.commit_single_write(WriteOp::Update {
            path: path.to_string(),
            data: value,
        })
    }

    fn delete(&mut self, path: &str) -> Result<(), Box<dyn Error>> {
        if self.mode.is_some() {
            return Err("internal error: delete while in batch/transaction".into());
        }
        self.commit_single_write(WriteOp::Delete {
            path: path.to_string(),
        })
    }

    fn query_collection(
        &self,
        path: &str,
        filters: &[QueryFilter],
        limit: Option<usize>,
    ) -> Result<Vec<Value>, Box<dyn Error>> {
        let (parent, collection_id) = split_collection_path(path)?;
        let query = structured_query(collection_id, false, filters)?;
        run_query_sorted(self, parent.as_deref(), query, limit)
    }

    fn query_collection_group(
        &self,
        id: &str,
        filters: &[QueryFilter],
        limit: Option<usize>,
    ) -> Result<Vec<Value>, Box<dyn Error>> {
        let query = structured_query(id, true, filters)?;
        run_query_sorted(self, None, query, limit)
    }

    fn begin_batch(&mut self) -> Result<(), Box<dyn Error>> {
        if self.mode.is_some() {
            return Err("already in batch or transaction".into());
        }
        self.mode = Some(EmulatorMode::Batch(vec![]));
        Ok(())
    }

    fn batch_stage(&mut self, op: WriteOp) -> Result<(), Box<dyn Error>> {
        match &mut self.mode {
            Some(EmulatorMode::Batch(ops)) => {
                ops.push(op);
                Ok(())
            }
            _ => Err("not in batch".into()),
        }
    }

    fn commit_batch(&mut self) -> Result<(), Box<dyn Error>> {
        if let Some(EmulatorMode::Batch(ops)) = self.mode.take() {
            let writes = write_ops_to_sdk_writes(self, &ops)?;
            self.commit_writes(writes, vec![])
        } else {
            Err("not in batch".into())
        }
    }

    fn abort_batch(&mut self) {
        if matches!(self.mode, Some(EmulatorMode::Batch(_))) {
            self.mode = None;
        }
    }

    fn begin_transaction(&mut self) -> Result<(), Box<dyn Error>> {
        if self.mode.is_some() {
            return Err("already in batch or transaction".into());
        }
        let db = self.sdk_db()?;
        let database = db.get_database_path().clone();
        let transaction_id = self.runtime.block_on(async move {
            let resp = db
                .client()
                .get()
                .begin_transaction(Request::new(BeginTransactionRequest {
                    database,
                    options: None,
                }))
                .await?;
            Ok::<_, gcloud_sdk::tonic::Status>(resp.into_inner().transaction)
        })?;
        self.mode = Some(EmulatorMode::Transaction {
            transaction_id,
            writes: vec![],
        });
        Ok(())
    }

    fn get_in_transaction(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>> {
        let tid = match &self.mode {
            Some(EmulatorMode::Transaction { transaction_id, .. }) => transaction_id.clone(),
            _ => return Err("not in transaction".into()),
        };
        let db = self.sdk_db()?;
        let database = db.get_database_path().clone();
        let document = self.document_name(path);
        let result = self.runtime.block_on(async move {
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
        })?;
        result.as_ref().map(decode_document_for_get).transpose()
    }

    fn transaction_stage(&mut self, op: WriteOp) -> Result<(), Box<dyn Error>> {
        match &mut self.mode {
            Some(EmulatorMode::Transaction { writes, .. }) => {
                writes.push(op);
                Ok(())
            }
            _ => Err("not in transaction".into()),
        }
    }

    fn commit_transaction(&mut self) -> Result<(), Box<dyn Error>> {
        if let Some(EmulatorMode::Transaction {
            transaction_id,
            writes,
        }) = self.mode.take()
        {
            let sdk_writes = write_ops_to_sdk_writes(self, &writes)?;
            self.commit_writes(sdk_writes, transaction_id)
        } else {
            Err("not in transaction".into())
        }
    }

    fn abort_transaction(&mut self) {
        if matches!(self.mode, Some(EmulatorMode::Transaction { .. })) {
            self.mode = None;
        }
    }
}

fn run_query_sorted(
    fs: &FirestoreEmulator,
    parent: Option<&str>,
    query: StructuredQuery,
    limit: Option<usize>,
) -> Result<Vec<Value>, Box<dyn Error>> {
    let db = fs.sdk_db()?;
    let parent = fs.parent_path(parent);
    let docs = fs.runtime.block_on(async move {
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
    })?;

    let mut out = vec![];
    for doc in docs {
        let rel = relative_document_name(&doc.name)?;
        let value = decode_document_for_query(&doc)?;
        out.push((rel, value));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    let mut values: Vec<Value> = out.into_iter().map(|(_, v)| v).collect();
    if let Some(l) = limit {
        values.truncate(l);
    }
    Ok(values)
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
        let op = match f.op.as_str() {
            "==" => structured_query::field_filter::Operator::Equal,
            ">" => structured_query::field_filter::Operator::GreaterThan,
            ">=" => structured_query::field_filter::Operator::GreaterThanOrEqual,
            "<" => structured_query::field_filter::Operator::LessThan,
            "<=" => structured_query::field_filter::Operator::LessThanOrEqual,
            o => return Err(format!("unsupported query op: {o}").into()),
        };
        parts.push(structured_query::Filter {
            filter_type: Some(structured_query::filter::FilterType::FieldFilter(
                structured_query::FieldFilter {
                    field: Some(structured_query::FieldReference {
                        field_path: f.field.clone(),
                    }),
                    op: op.into(),
                    value: Some(encode_field_value(&f.value)?),
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

fn write_ops_to_sdk_writes(
    fs: &FirestoreEmulator,
    ops: &[WriteOp],
) -> Result<Vec<Write>, Box<dyn Error>> {
    let mut out = vec![];
    for op in ops {
        out.push(build_write(fs, op)?);
    }
    Ok(out)
}

fn build_write(fs: &FirestoreEmulator, op: &WriteOp) -> Result<Write, Box<dyn Error>> {
    match op {
        WriteOp::Set { path, data, merge } => {
            let (plain, transforms) = split_fields_and_transforms(data);
            let fields = encode_fields(&plain)?;
            Ok(Write {
                update_mask: if *merge {
                    Some(DocumentMask {
                        field_paths: plain.keys().cloned().collect(),
                    })
                } else {
                    None
                },
                update_transforms: encode_field_transforms(&transforms)?,
                current_document: None,
                operation: Some(gcloud_sdk::google::firestore::v1::write::Operation::Update(
                    Document {
                        name: fs.document_name(path),
                        fields,
                        create_time: None,
                        update_time: None,
                    },
                )),
            })
        }
        WriteOp::Update { path, data } => {
            let (plain, transforms) = split_fields_and_transforms(data);
            let fields = encode_fields(&plain)?;
            Ok(Write {
                update_mask: Some(DocumentMask {
                    field_paths: plain.keys().cloned().collect(),
                }),
                update_transforms: encode_field_transforms(&transforms)?,
                current_document: Some(Precondition {
                    condition_type: Some(precondition::ConditionType::Exists(true)),
                }),
                operation: Some(gcloud_sdk::google::firestore::v1::write::Operation::Update(
                    Document {
                        name: fs.document_name(path),
                        fields,
                        create_time: None,
                        update_time: None,
                    },
                )),
            })
        }
        WriteOp::Delete { path } => Ok(Write {
            update_mask: None,
            update_transforms: vec![],
            current_document: None,
            operation: Some(gcloud_sdk::google::firestore::v1::write::Operation::Delete(
                fs.document_name(path),
            )),
        }),
    }
}

fn split_fields_and_transforms(
    data: &BTreeMap<String, Value>,
) -> (BTreeMap<String, Value>, Vec<(String, Value)>) {
    let mut plain = BTreeMap::new();
    let mut transforms = vec![];
    for (k, v) in data {
        match v {
            Value::ArrayUnion(_) | Value::ArrayRemove(_) | Value::ServerTimestamp => {
                transforms.push((k.clone(), v.clone()));
            }
            _ => {
                plain.insert(k.clone(), v.clone());
            }
        }
    }
    (plain, transforms)
}

fn encode_field_transforms(
    transforms: &[(String, Value)],
) -> Result<Vec<document_transform::FieldTransform>, Box<dyn Error>> {
    let mut out = vec![];
    for (path, v) in transforms {
        out.push(encode_field_transform(path, v)?);
    }
    Ok(out)
}

fn encode_field_transform(
    field_path: &str,
    v: &Value,
) -> Result<document_transform::FieldTransform, Box<dyn Error>> {
    let transform_type = match v {
        Value::ServerTimestamp => {
            document_transform::field_transform::TransformType::SetToServerValue(
                document_transform::field_transform::ServerValue::RequestTime.into(),
            )
        }
        Value::ArrayUnion(items) => {
            document_transform::field_transform::TransformType::AppendMissingElements(ArrayValue {
                values: encode_array_items(items)?,
            })
        }
        Value::ArrayRemove(items) => {
            document_transform::field_transform::TransformType::RemoveAllFromArray(ArrayValue {
                values: encode_array_items(items)?,
            })
        }
        _ => return Err("not a transform value".into()),
    };
    Ok(document_transform::FieldTransform {
        field_path: field_path.to_string(),
        transform_type: Some(transform_type),
    })
}

fn encode_array_items(items: &[Value]) -> Result<Vec<SdkValue>, Box<dyn Error>> {
    items.iter().map(encode_field_value).collect()
}

fn encode_fields(
    map: &BTreeMap<String, Value>,
) -> Result<HashMap<String, SdkValue>, Box<dyn Error>> {
    let mut out = HashMap::new();
    for (k, v) in map {
        out.insert(k.clone(), encode_field_value(v)?);
    }
    Ok(out)
}

fn encode_field_value(v: &Value) -> Result<SdkValue, Box<dyn Error>> {
    Ok(SdkValue {
        value_type: Some(match v {
            Value::Str(s) => value::ValueType::StringValue(s.clone()),
            Value::Int(i) => value::ValueType::IntegerValue(*i),
            Value::Bool(b) => value::ValueType::BooleanValue(*b),
            Value::Nil => value::ValueType::NullValue(NullValue::NullValue.into()),
            Value::Timestamp(ts) => value::ValueType::TimestampValue(ProtoTimestamp {
                seconds: ts.seconds,
                nanos: 0,
            }),
            Value::Array(items) => value::ValueType::ArrayValue(ArrayValue {
                values: encode_array_items(items)?,
            }),
            Value::Map(m) => value::ValueType::MapValue(MapValue {
                fields: encode_fields(m)?,
            }),
            _ => return Err(format!("unsupported value in emulator write: {v:?}").into()),
        }),
    })
}

fn decode_document_for_get(doc: &Document) -> Result<Value, Box<dyn Error>> {
    decode_document_inner(doc, false)
}

fn decode_document_for_query(doc: &Document) -> Result<Value, Box<dyn Error>> {
    decode_document_inner(doc, true)
}

fn decode_document_inner(doc: &Document, include_doc_path: bool) -> Result<Value, Box<dyn Error>> {
    let mut out = BTreeMap::new();
    for (k, fv) in &doc.fields {
        out.insert(k.clone(), decode_field_value(fv)?);
    }
    if include_doc_path {
        if let Ok(rel) = relative_document_name(&doc.name) {
            out.insert("__name__".to_string(), Value::Str(rel));
        }
    }
    Ok(Value::Map(out))
}

fn decode_field_value(v: &SdkValue) -> Result<Value, Box<dyn Error>> {
    match v
        .value_type
        .as_ref()
        .ok_or("missing Firestore value type")?
    {
        value::ValueType::StringValue(s) => Ok(Value::Str(s.clone())),
        value::ValueType::IntegerValue(n) => Ok(Value::Int(*n)),
        value::ValueType::BooleanValue(b) => Ok(Value::Bool(*b)),
        value::ValueType::NullValue(_) => Ok(Value::Nil),
        value::ValueType::TimestampValue(ts) => Ok(Value::Timestamp(Timestamp {
            seconds: ts.seconds,
        })),
        value::ValueType::ArrayValue(arr) => {
            let mut out = vec![];
            for x in &arr.values {
                out.push(decode_field_value(x)?);
            }
            Ok(Value::Array(out))
        }
        value::ValueType::MapValue(map) => {
            let mut out = BTreeMap::new();
            for (k, fv) in &map.fields {
                out.insert(k.clone(), decode_field_value(fv)?);
            }
            Ok(Value::Map(out))
        }
        other => Err(format!("unsupported Firestore field value: {other:?}").into()),
    }
}

fn relative_document_name(name: &str) -> Result<String, Box<dyn Error>> {
    let (_, rel) = name
        .split_once("/documents/")
        .ok_or("invalid document name")?;
    Ok(rel.to_string())
}

fn split_collection_path(path: &str) -> Result<(Option<String>, &str), Box<dyn Error>> {
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() || parts.len().is_multiple_of(2) {
        return Err(format!("invalid collection path: {path}").into());
    }
    let collection_id = parts[parts.len() - 1];
    let parent = if parts.len() == 1 {
        None
    } else {
        Some(parts[..parts.len() - 1].join("/"))
    };
    Ok((parent, collection_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gcloud_sdk::google::firestore::v1::write;

    #[test]
    fn sdk_options_targets_emulator_host() {
        let options = sdk_options("127.0.0.1:9", "p");
        assert_eq!(options.google_project_id, "p");
        assert_eq!(
            options.firebase_api_url.as_deref(),
            Some("http://127.0.0.1:9")
        );
    }

    #[test]
    fn sdk_options_keeps_explicit_scheme() {
        let options = sdk_options("http://localhost:8080", "p");
        assert_eq!(
            options.firebase_api_url.as_deref(),
            Some("http://localhost:8080")
        );
    }

    #[test]
    fn split_collection_path_supports_subcollections() {
        let (parent, collection_id) = split_collection_path("teams/t1/members").unwrap();
        assert_eq!(parent.as_deref(), Some("teams/t1"));
        assert_eq!(collection_id, "members");
    }

    #[test]
    fn decode_empty_document_for_get_has_no_name_field() {
        let doc = Document {
            name: "projects/p/databases/(default)/documents/users/x".to_string(),
            fields: HashMap::new(),
            create_time: None,
            update_time: None,
        };
        let Value::Map(m) = decode_document_for_get(&doc).unwrap() else {
            panic!();
        };
        assert!(!m.contains_key("__name__"));
        assert!(m.is_empty());
    }

    #[test]
    fn decode_document_for_query_adds_name() {
        let doc = Document {
            name: "projects/p/databases/(default)/documents/users/x".to_string(),
            fields: HashMap::from([(
                "a".to_string(),
                SdkValue {
                    value_type: Some(value::ValueType::StringValue("v".to_string())),
                },
            )]),
            create_time: None,
            update_time: None,
        };
        let Value::Map(m) = decode_document_for_query(&doc).unwrap() else {
            panic!();
        };
        assert_eq!(m.get("__name__"), Some(&Value::Str("users/x".to_string())));
    }

    #[test]
    fn decode_document_for_query_preserves_documents_in_path() {
        let doc = Document {
            name: "projects/p/databases/(default)/documents/teams/t1/documents/x".to_string(),
            fields: HashMap::new(),
            create_time: None,
            update_time: None,
        };
        let Value::Map(m) = decode_document_for_query(&doc).unwrap() else {
            panic!();
        };
        assert_eq!(
            m.get("__name__"),
            Some(&Value::Str("teams/t1/documents/x".to_string()))
        );
    }

    #[test]
    fn build_write_update_includes_exists_precondition() {
        let fs = FirestoreEmulator::new("127.0.0.1:9".to_string(), "p".to_string());
        let writes = write_ops_to_sdk_writes(
            &fs,
            &[WriteOp::Update {
                path: "c/d".to_string(),
                data: BTreeMap::from([("x".to_string(), Value::Int(1))]),
            }],
        )
        .unwrap();
        assert_eq!(
            writes[0]
                .current_document
                .as_ref()
                .and_then(|p| p.condition_type),
            Some(precondition::ConditionType::Exists(true))
        );
    }

    #[test]
    fn not_found_error_includes_write_target_and_status_message() {
        let fs = FirestoreEmulator::new("127.0.0.1:9".to_string(), "p".to_string());
        let writes = write_ops_to_sdk_writes(
            &fs,
            &[WriteOp::Update {
                path: "users/missing".to_string(),
                data: BTreeMap::from([("x".to_string(), Value::Int(1))]),
            }],
        )
        .unwrap();
        let status = Status::not_found("No document to update");
        let error = not_found_error(&status, write_targets(&writes).as_deref());
        assert_eq!(
            error,
            "document not found: users/missing: No document to update"
        );
    }

    #[test]
    fn failed_precondition_is_missing_document_only_for_exists_precondition() {
        let fs = FirestoreEmulator::new("127.0.0.1:9".to_string(), "p".to_string());
        let writes = write_ops_to_sdk_writes(
            &fs,
            &[WriteOp::Update {
                path: "users/missing".to_string(),
                data: BTreeMap::from([("x".to_string(), Value::Int(1))]),
            }],
        )
        .unwrap();
        assert!(write_requires_existing_document(&writes[0]));
        assert!(missing_document_status(
            &Status::failed_precondition("No document to update"),
            true
        ));
        assert!(!missing_document_status(
            &Status::failed_precondition("other precondition failed"),
            false
        ));
    }

    #[test]
    fn update_mask_excludes_transform_keys() {
        let fs = FirestoreEmulator::new("127.0.0.1:9".to_string(), "p".to_string());
        let writes = write_ops_to_sdk_writes(
            &fs,
            &[WriteOp::Update {
                path: "c/d".to_string(),
                data: BTreeMap::from([
                    ("plain".to_string(), Value::Int(1)),
                    (
                        "tags".to_string(),
                        Value::ArrayUnion(vec![Value::Str("x".into())]),
                    ),
                ]),
            }],
        )
        .unwrap();
        let mask = &writes[0].update_mask.as_ref().unwrap().field_paths;
        assert!(mask.iter().any(|v| v == "plain"));
        assert!(!mask.iter().any(|v| v == "tags"));
        assert_eq!(writes[0].update_transforms.len(), 1);
    }

    #[test]
    fn set_merge_mask_excludes_transform_keys() {
        let fs = FirestoreEmulator::new("127.0.0.1:9".to_string(), "p".to_string());
        let writes = write_ops_to_sdk_writes(
            &fs,
            &[WriteOp::Set {
                path: "c/d".to_string(),
                merge: true,
                data: BTreeMap::from([
                    ("plain".to_string(), Value::Int(1)),
                    (
                        "tags".to_string(),
                        Value::ArrayRemove(vec![Value::Str("x".into())]),
                    ),
                ]),
            }],
        )
        .unwrap();
        let mask = &writes[0].update_mask.as_ref().unwrap().field_paths;
        assert_eq!(mask, &vec!["plain".to_string()]);
    }

    #[test]
    fn server_timestamp_uses_sdk_transform() {
        let fs = FirestoreEmulator::new("127.0.0.1:9".to_string(), "p".to_string());
        let writes = write_ops_to_sdk_writes(
            &fs,
            &[WriteOp::Set {
                path: "c/d".to_string(),
                merge: true,
                data: BTreeMap::from([("updated_at".to_string(), Value::ServerTimestamp)]),
            }],
        )
        .unwrap();
        let transform = &writes[0].update_transforms[0];
        assert_eq!(transform.field_path, "updated_at");
        assert!(matches!(
            transform.transform_type,
            Some(document_transform::field_transform::TransformType::SetToServerValue(_))
        ));
    }

    #[test]
    fn delete_write_uses_sdk_delete_operation() {
        let fs = FirestoreEmulator::new("127.0.0.1:9".to_string(), "p".to_string());
        let write = build_write(
            &fs,
            &WriteOp::Delete {
                path: "users/a?b".to_string(),
            },
        )
        .unwrap();
        assert!(matches!(write.operation, Some(write::Operation::Delete(_))));
    }
}
