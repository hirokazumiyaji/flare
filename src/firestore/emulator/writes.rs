//! `WriteOp` → SDK `Write` の組み立て・transform・NotFound 整形。

use std::collections::BTreeMap;
use std::error::Error;
use std::time::Duration;

use firestore::FirestoreDb;
use gcloud_sdk::google::firestore::v1::{
    document_transform, precondition, value, write, ArrayValue, CommitRequest, Document,
    DocumentMask, Precondition, Value as SdkValue, Write,
};
use gcloud_sdk::tonic::{Code, Request, Status};
use tokio::runtime::Runtime;

use super::client;
use super::codec::encode_fields;
use super::path::document_name;
use crate::firestore::{
    insert_at_field_path, plan_update_writes, quote_field_name, split_write_values,
    validate_write_op, WriteOp,
};
use crate::value::Value;

pub(super) fn commit_writes(
    db: &FirestoreDb,
    runtime: &Runtime,
    operation_timeout: Duration,
    writes: Vec<Write>,
    transaction: Vec<u8>,
) -> Result<(), Box<dyn Error>> {
    let database = db.get_database_path().clone();
    let not_found_context = write_targets(&writes);
    let has_exists_precondition = writes.iter().any(write_requires_existing_document);
    let result = client::run_operation(runtime, operation_timeout, "commit", async move {
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
        Err(error) if status_is_missing_document(error.as_ref(), has_exists_precondition) => {
            // Preserve the existing not-found contract while retaining the
            // typed timeout error for cancellation.
            let status = error
                .downcast_ref::<Status>()
                .expect("commit error should be a gRPC status");
            Err(not_found_error(status, not_found_context.as_deref()).into())
        }
        Err(error) => Err(error),
    }
}

fn status_is_missing_document(
    error: &(dyn Error + 'static),
    has_exists_precondition: bool,
) -> bool {
    error
        .downcast_ref::<Status>()
        .is_some_and(|status| missing_document_status(status, has_exists_precondition))
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
    Some(super::path::relative_document_name(name).unwrap_or_else(|_| name.clone()))
}

pub(super) fn write_ops_to_sdk_writes(
    project_id: &str,
    ops: &[WriteOp],
) -> Result<Vec<Write>, Box<dyn Error>> {
    ops.iter().map(|op| build_write(project_id, op)).collect()
}

fn build_write(project_id: &str, op: &WriteOp) -> Result<Write, Box<dyn Error>> {
    // in-memory と同じ共有 validator を RPC 前に通す。検証に失敗した
    // write は emulator に到達しないため、後続 write も送信されない。
    validate_write_op(op)?;
    match op {
        WriteOp::Set { path, data, merge } => {
            if *merge {
                build_masked_write(project_id, path, data, false)
            } else {
                let split = split_write_values(data);
                if !split.deletes.is_empty() {
                    return Err("delete_field() cannot be used with set() without merge".into());
                }
                // 置換 set: キーはリテラルのフィールド名として保存する。
                // センチネル変換は、非単純名でも Firestore がリテラル名と
                // 解釈できる quoted field path にして送る。
                let transforms: Vec<_> = split
                    .transforms
                    .into_iter()
                    .map(|(key, value)| (quote_field_name(&key), value))
                    .collect();
                let fields = encode_fields(&split.plain)?;
                Ok(Write {
                    update_mask: None,
                    update_transforms: encode_field_transforms(&transforms)?,
                    current_document: None,
                    operation: Some(write::Operation::Update(Document {
                        name: document_name(project_id, path),
                        fields,
                        create_time: None,
                        update_time: None,
                    })),
                })
            }
        }
        WriteOp::Update { path, data } => build_masked_write(project_id, path, data, true),
        WriteOp::Delete { path } => Ok(Write {
            update_mask: None,
            update_transforms: vec![],
            current_document: None,
            operation: Some(write::Operation::Delete(document_name(project_id, path))),
        }),
    }
}

/// merge `set` と `update` の共通組み立て。違いは存在事前条件の有無だけ。
/// マスクには plain と delete のキーが入り、payload は plain をネスト化した
/// ものになる。フィールド path と mask 内の overlap は in-memory と同じ共通
/// helper で、RPC を送る前に検証する。
fn build_masked_write(
    project_id: &str,
    path: &str,
    data: &BTreeMap<String, Value>,
    require_existing: bool,
) -> Result<Write, Box<dyn Error>> {
    let split = plan_update_writes(data)?;
    // DeleteField は update_mask に含めつつ fields から除外する。

    let mut field_paths: Vec<String> = split.plain.keys().cloned().collect();
    field_paths.extend(split.deletes);
    let fields = encode_fields(&nest_field_paths(split.plain)?)?;
    Ok(Write {
        update_mask: Some(DocumentMask { field_paths }),
        update_transforms: encode_field_transforms(&split.transforms)?,
        current_document: require_existing.then_some(Precondition {
            condition_type: Some(precondition::ConditionType::Exists(true)),
        }),
        operation: Some(write::Operation::Update(Document {
            name: document_name(project_id, path),
            fields,
            create_time: None,
            update_time: None,
        })),
    })
}

/// フラットなフィールドパスマップをネストしたマップに組み替える。
/// update_mask のパス("profile.name" など)が payload から解決されるように
/// するための変換。
fn nest_field_paths(
    plain: BTreeMap<String, Value>,
) -> Result<BTreeMap<String, Value>, Box<dyn Error>> {
    let mut out = BTreeMap::new();
    for (key, value) in plain {
        let segments: Vec<&str> = key.split('.').collect();
        insert_at_field_path(&mut out, &segments, value)?;
    }
    Ok(out)
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
                values: super::codec::encode_array_items(items)?,
            })
        }
        Value::ArrayRemove(items) => {
            document_transform::field_transform::TransformType::RemoveAllFromArray(ArrayValue {
                values: super::codec::encode_array_items(items)?,
            })
        }
        Value::Increment(n) => {
            document_transform::field_transform::TransformType::Increment(SdkValue {
                value_type: Some(value::ValueType::IntegerValue(*n)),
            })
        }
        _ => return Err("not a transform value".into()),
    };
    Ok(document_transform::FieldTransform {
        field_path: field_path.to_string(),
        transform_type: Some(transform_type),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_write_update_treats_delete_field_as_mask_only() {
        let writes = write_ops_to_sdk_writes(
            "p",
            &[WriteOp::Update {
                path: "users/d1".to_string(),
                data: BTreeMap::from([
                    ("gone".to_string(), Value::DeleteField),
                    ("kept".to_string(), Value::Int(1)),
                ]),
            }],
        )
        .unwrap();
        let mut mask = writes[0]
            .update_mask
            .clone()
            .expect("update mask")
            .field_paths;
        mask.sort();
        assert_eq!(mask, vec!["gone".to_string(), "kept".to_string()]);
        let write::Operation::Update(doc) = writes[0].operation.as_ref().expect("update op") else {
            panic!("expected update operation");
        };
        assert!(!doc.fields.contains_key("gone"));
        assert!(doc.fields.contains_key("kept"));
    }

    #[test]
    fn build_write_merge_set_masks_deleted_fields() {
        let writes = write_ops_to_sdk_writes(
            "p",
            &[WriteOp::Set {
                path: "users/d2".to_string(),
                data: BTreeMap::from([("gone".to_string(), Value::DeleteField)]),
                merge: true,
            }],
        )
        .unwrap();
        assert_eq!(
            writes[0].update_mask.clone().expect("mask").field_paths,
            vec!["gone".to_string()]
        );
        let write::Operation::Update(doc) = writes[0].operation.as_ref().expect("update op") else {
            panic!("expected update operation");
        };
        assert!(doc.fields.is_empty());
    }

    #[test]
    fn masked_writes_reject_overlapping_paths_before_rpc() {
        for op in [
            WriteOp::Update {
                path: "users/overlap".to_string(),
                data: BTreeMap::from([
                    ("a".to_string(), Value::DeleteField),
                    ("a.b".to_string(), Value::Int(1)),
                ]),
            },
            WriteOp::Set {
                path: "users/overlap".to_string(),
                merge: true,
                data: BTreeMap::from([
                    ("a".to_string(), Value::Int(1)),
                    ("a.b".to_string(), Value::Int(2)),
                ]),
            },
        ] {
            let err = write_ops_to_sdk_writes("p", &[op]).unwrap_err();
            assert_eq!(err.to_string(), "overlapping field paths: a and a.b");
        }
    }

    #[test]
    fn build_write_replacement_set_rejects_delete_field() {
        let err = write_ops_to_sdk_writes(
            "p",
            &[WriteOp::Set {
                path: "users/d3".to_string(),
                data: BTreeMap::from([("gone".to_string(), Value::DeleteField)]),
                merge: false,
            }],
        )
        .unwrap_err();
        assert!(
            format!("{err}").contains("delete_field() cannot be used with set() without merge"),
            "{err}"
        );
    }

    #[test]
    fn replacement_set_quotes_non_simple_transform_field_names() {
        let writes = write_ops_to_sdk_writes(
            "p",
            &[WriteOp::Set {
                path: "users/quoted".to_string(),
                merge: false,
                data: BTreeMap::from([
                    ("1start_digit".to_string(), Value::Increment(1)),
                    ("a-b".to_string(), Value::Increment(1)),
                    ("a.b".to_string(), Value::Increment(1)),
                    ("a`b".to_string(), Value::Increment(1)),
                    (r"a\b".to_string(), Value::Increment(1)),
                    ("simple_2".to_string(), Value::Increment(1)),
                ]),
            }],
        )
        .unwrap();
        let paths: Vec<&str> = writes[0]
            .update_transforms
            .iter()
            .map(|transform| transform.field_path.as_str())
            .collect();
        assert_eq!(
            paths,
            vec![
                r#"`1start_digit`"#,
                r#"`a-b`"#,
                r#"`a.b`"#,
                r#"`a\\b`"#,
                r#"`a\`b`"#,
                "simple_2",
            ]
        );
    }

    #[test]
    fn build_write_encodes_increment_as_field_transform() {
        let writes = write_ops_to_sdk_writes(
            "p",
            &[WriteOp::Update {
                path: "users/i1".to_string(),
                data: BTreeMap::from([("count".to_string(), Value::Increment(5))]),
            }],
        )
        .unwrap();
        assert_eq!(writes[0].update_transforms.len(), 1);
        assert_eq!(writes[0].update_transforms[0].field_path, "count");
        match writes[0].update_transforms[0]
            .transform_type
            .as_ref()
            .expect("transform type")
        {
            document_transform::field_transform::TransformType::Increment(v) => {
                assert_eq!(v.value_type, Some(value::ValueType::IntegerValue(5)));
            }
            other => panic!("expected increment transform, got {other:?}"),
        }
    }

    #[test]
    fn build_write_update_includes_exists_precondition() {
        let writes = write_ops_to_sdk_writes(
            "p",
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
        let writes = write_ops_to_sdk_writes(
            "p",
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
        let writes = write_ops_to_sdk_writes(
            "p",
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
        let writes = write_ops_to_sdk_writes(
            "p",
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
        let writes = write_ops_to_sdk_writes(
            "p",
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
        let writes = write_ops_to_sdk_writes(
            "p",
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
    fn invalid_write_is_rejected_before_sdk_writes_are_built() {
        let cases = [
            BTreeMap::from([("__name__".to_string(), Value::Int(1))]),
            BTreeMap::from([(
                "items".to_string(),
                Value::Array(vec![Value::Array(vec![Value::Int(1)])]),
            )]),
        ];
        for data in cases {
            let err = write_ops_to_sdk_writes(
                "p",
                &[WriteOp::Set {
                    path: "users/invalid".to_string(),
                    data,
                    merge: false,
                }],
            )
            .unwrap_err();
            assert!(!err.to_string().is_empty());
        }
    }

    #[test]
    fn delete_write_uses_sdk_delete_operation() {
        let write = build_write(
            "p",
            &WriteOp::Delete {
                path: "users/a?b".to_string(),
            },
        )
        .unwrap();
        assert!(matches!(write.operation, Some(write::Operation::Delete(_))));
    }
}
