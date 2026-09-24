//! Firestore SDK adapter for the local emulator (`FIRESTORE_EMULATOR_HOST`).

mod client;
mod codec;
mod path;
mod writes;

use std::error::Error;

use gcloud_sdk::google::firestore::v1::{
    batch_get_documents_request, batch_get_documents_response, run_query_request, structured_query,
    BatchGetDocumentsRequest, BeginTransactionRequest, GetDocumentRequest, RollbackRequest,
    RunQueryRequest, StructuredQuery,
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
        Ok(Self {
            client: client::connect(&host, &project_id)?,
            transaction_id: None,
        })
    }
}

impl Firestore for FirestoreEmulator {
    fn get(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>> {
        let db = &self.client.db;
        let name = path::document_name(&self.client.project_id, path);
        let result = self.client.runtime.block_on(async move {
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
            Ok(resp) => codec::decode_document(&resp.into_inner()).map(Some),
            Err(status) if status.code() == Code::NotFound => Ok(None),
            Err(status) => Err(status.into()),
        }
    }

    fn apply(&mut self, writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>> {
        let sdk_writes = writes::write_ops_to_sdk_writes(&self.client.project_id, &writes)?;
        writes::commit_writes(&self.client.db, &self.client.runtime, sdk_writes, vec![])
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
        let transaction_id = self.client.runtime.block_on(async move {
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
        self.transaction_id = Some(transaction_id);
        Ok(())
    }

    fn transaction_get(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>> {
        let tid = self.transaction_id.clone().ok_or("not in transaction")?;
        let db = &self.client.db;
        let database = db.get_database_path().clone();
        let document = path::document_name(&self.client.project_id, path);
        let result = self.client.runtime.block_on(async move {
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
        result.as_ref().map(codec::decode_document).transpose()
    }

    fn commit_transaction(&mut self, writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>> {
        let tid = self.transaction_id.take().ok_or("not in transaction")?;
        let sdk_writes = writes::write_ops_to_sdk_writes(&self.client.project_id, &writes)?;
        writes::commit_writes(&self.client.db, &self.client.runtime, sdk_writes, tid)
    }

    fn abort_transaction(&mut self) -> Result<(), Box<dyn Error>> {
        let Some(tid) = self.transaction_id.take() else {
            return Ok(());
        };
        let db = &self.client.db;
        let database = db.get_database_path().clone();
        self.client.runtime.block_on(async move {
            db.client()
                .get()
                .rollback(Request::new(RollbackRequest {
                    database,
                    transaction: tid,
                }))
                .await?;
            Ok::<_, gcloud_sdk::tonic::Status>(())
        })?;
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
    let docs = fs.client.runtime.block_on(async move {
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
        let rel = path::relative_document_name(&doc.name)?;
        let Value::Map(fields) = codec::decode_document(&doc)? else {
            return Err("query result must be a map".into());
        };
        out.push((rel, fields));
    }
    Ok(out)
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
}
