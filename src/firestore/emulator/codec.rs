//! `Value` ⇔ SDK `Value` のコーデック。`__name__` の付与はしない
//! (`__name__` は runtime が両バックエンド共通で付ける)。

use std::collections::{BTreeMap, HashMap};
use std::error::Error;

use gcloud_sdk::google::firestore::v1::{value, ArrayValue, Document, MapValue, Value as SdkValue};
use prost_types::{NullValue, Timestamp as ProtoTimestamp};

use super::path::{document_name, relative_document_name_for};
use crate::value::{DocRef, Timestamp, Value};

pub(super) fn encode_array_items(items: &[Value]) -> Result<Vec<SdkValue>, Box<dyn Error>> {
    items.iter().map(encode_field_value).collect()
}

pub(super) fn encode_fields(
    map: &BTreeMap<String, Value>,
) -> Result<HashMap<String, SdkValue>, Box<dyn Error>> {
    let mut out = HashMap::new();
    for (k, v) in map {
        out.insert(k.clone(), encode_field_value(v)?);
    }
    Ok(out)
}

/// Encodes a value for a document write.
///
/// `DocRef` is deliberately not accepted here. In this API references are
/// query-filter values; the write codec remains restricted to the documented
/// writable field values and reports an unsupported value for references.
pub(super) fn encode_field_value(v: &Value) -> Result<SdkValue, Box<dyn Error>> {
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

/// Encodes a value used in a structured query filter.
///
/// Firestore's `ReferenceValue` is a fully qualified resource name, while a
/// `DocRef` in firelang contains only a document-relative path. Query
/// construction supplies the project and uses the backend's default database
/// to turn that path into a reference. Unlike [`encode_field_value`], this
/// encoder walks arrays and maps recursively so references nested in either
/// container are encoded as references too.
pub(super) fn encode_query_value(project_id: &str, v: &Value) -> Result<SdkValue, Box<dyn Error>> {
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
            Value::DocRef(doc_ref) => {
                value::ValueType::ReferenceValue(query_reference_name(project_id, doc_ref)?)
            }
            Value::Array(items) => value::ValueType::ArrayValue(ArrayValue {
                values: items
                    .iter()
                    .map(|item| encode_query_value(project_id, item))
                    .collect::<Result<Vec<_>, _>>()?,
            }),
            Value::Map(map) => value::ValueType::MapValue(MapValue {
                fields: map
                    .iter()
                    .map(|(key, value)| {
                        encode_query_value(project_id, value).map(|encoded| (key.clone(), encoded))
                    })
                    .collect::<Result<HashMap<_, _>, _>>()?,
            }),
            _ => return Err(format!("unsupported value in emulator query: {v:?}").into()),
        }),
    })
}

fn query_reference_name(project_id: &str, doc_ref: &DocRef) -> Result<String, Box<dyn Error>> {
    let segments: Vec<&str> = doc_ref.path.split('/').collect();
    if segments.iter().any(|segment| segment.is_empty()) || !segments.len().is_multiple_of(2) {
        return Err(format!(
            "invalid document path in emulator query reference: {}",
            doc_ref.path
        )
        .into());
    }
    Ok(document_name(project_id, &doc_ref.path))
}

pub(super) fn decode_document(
    doc: &Document,
    project_id: &str,
    database_id: &str,
) -> Result<Value, Box<dyn Error>> {
    let mut out = BTreeMap::new();
    for (k, fv) in &doc.fields {
        out.insert(
            k.clone(),
            decode_field_value_for_context(fv, project_id, database_id)?,
        );
    }
    Ok(Value::Map(out))
}

#[cfg(test)]
pub(super) fn decode_field_value(v: &SdkValue) -> Result<Value, Box<dyn Error>> {
    decode_field_value_for_context(v, "p", "(default)")
}

pub(super) fn decode_field_value_for_context(
    v: &SdkValue,
    project_id: &str,
    database_id: &str,
) -> Result<Value, Box<dyn Error>> {
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
                out.push(decode_field_value_for_context(x, project_id, database_id)?);
            }
            Ok(Value::Array(out))
        }
        value::ValueType::MapValue(map) => {
            let mut out = BTreeMap::new();
            for (k, fv) in &map.fields {
                out.insert(
                    k.clone(),
                    decode_field_value_for_context(fv, project_id, database_id)?,
                );
            }
            Ok(Value::Map(out))
        }
        // firelang が持たない型は読めることだけを目標にする。double・
        // geo_point・bytes は表示専用の Opaque にし、reference は相対パス
        // の DocRef にする。書き戻しは unsupported のまま(§3)。
        value::ValueType::DoubleValue(f) => Ok(Value::Opaque(format!("Double({f})"))),
        value::ValueType::BytesValue(b) => Ok(Value::Opaque(format!("Bytes({})", hex_bytes(b)))),
        value::ValueType::GeoPointValue(p) => Ok(Value::Opaque(format!(
            "GeoPoint({}, {})",
            p.latitude, p.longitude
        ))),
        value::ValueType::ReferenceValue(name) => {
            match relative_document_name_for(name, project_id, database_id) {
                Ok(rel) => Ok(Value::DocRef(DocRef { path: rel })),
                Err(_) => Ok(Value::Opaque(format!("Reference({name})"))),
            }
        }
        other => Err(format!("unsupported Firestore field value: {other:?}").into()),
    }
}

fn hex_bytes(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_document_has_no_name_field() {
        // `__name__` の付与は runtime が行う。decode はフィールドのみ返す。
        let doc = Document {
            name: "projects/p/databases/(default)/documents/users/x".to_string(),
            fields: HashMap::new(),
            create_time: None,
            update_time: None,
        };
        let Value::Map(m) = decode_document(&doc, "p", "(default)").unwrap() else {
            panic!();
        };
        assert!(!m.contains_key("__name__"));
        assert!(m.is_empty());
    }

    #[test]
    fn decode_document_returns_fields_without_name() {
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
        let Value::Map(m) = decode_document(&doc, "p", "(default)").unwrap() else {
            panic!();
        };
        assert_eq!(m.get("a"), Some(&Value::Str("v".to_string())));
        assert!(!m.contains_key("__name__"));
    }

    #[test]
    fn decode_document_rejects_malformed_document_path() {
        // パス中の "documents" は相対パス化(呼び出し側)で保持される。
        // decode 自体は名前に触らない。
        let doc = Document {
            name: "projects/p/databases/(default)/documents/teams/t1/x".to_string(),
            fields: HashMap::new(),
            create_time: None,
            update_time: None,
        };
        let err = relative_document_name_for(&doc.name, "p", "(default)").unwrap_err();
        assert!(err.to_string().contains("invalid document name"), "{err}");
    }

    #[test]
    fn decode_exotic_field_values_as_opaque_or_docref() {
        let sdk = |t| SdkValue {
            value_type: Some(t),
        };
        assert_eq!(
            decode_field_value(&sdk(value::ValueType::DoubleValue(1.5))).unwrap(),
            Value::Opaque("Double(1.5)".to_string())
        );
        assert_eq!(
            decode_field_value(&sdk(value::ValueType::BytesValue(vec![0x00, 0xff]))).unwrap(),
            Value::Opaque("Bytes(00ff)".to_string())
        );
        assert_eq!(
            decode_field_value(&sdk(value::ValueType::GeoPointValue(
                gcloud_sdk::google::r#type::LatLng {
                    latitude: 35.7,
                    longitude: 139.7,
                },
            )))
            .unwrap(),
            Value::Opaque("GeoPoint(35.7, 139.7)".to_string())
        );
        assert_eq!(
            decode_field_value(&sdk(value::ValueType::ReferenceValue(
                "projects/p/databases/(default)/documents/users/1".to_string(),
            )))
            .unwrap(),
            Value::DocRef(DocRef {
                path: "users/1".to_string()
            })
        );
        assert_eq!(
            decode_field_value_for_context(
                &sdk(value::ValueType::ReferenceValue(
                    "projects/other/databases/(default)/documents/users/1".to_string(),
                )),
                "p",
                "(default)",
            )
            .unwrap(),
            Value::Opaque(
                "Reference(projects/other/databases/(default)/documents/users/1)".to_string()
            )
        );
    }

    #[test]
    fn decode_document_with_exotic_fields_does_not_fail() {
        let doc = Document {
            name: "projects/p/databases/(default)/documents/users/x".to_string(),
            fields: HashMap::from([
                (
                    "name".to_string(),
                    SdkValue {
                        value_type: Some(value::ValueType::StringValue("a".to_string())),
                    },
                ),
                (
                    "score".to_string(),
                    SdkValue {
                        value_type: Some(value::ValueType::DoubleValue(4.5)),
                    },
                ),
            ]),
            create_time: None,
            update_time: None,
        };
        let Value::Map(m) = decode_document(&doc, "p", "(default)").unwrap() else {
            panic!();
        };
        assert_eq!(m.get("name"), Some(&Value::Str("a".to_string())));
        assert_eq!(
            m.get("score"),
            Some(&Value::Opaque("Double(4.5)".to_string()))
        );
    }

    #[test]
    fn encode_opaque_value_is_rejected_on_write() {
        let err = encode_field_value(&Value::Opaque("Double(1.5)".to_string())).unwrap_err();
        assert!(
            format!("{err}").contains("unsupported value in emulator write"),
            "{err}"
        );
    }

    #[test]
    fn query_encoder_uses_fully_qualified_reference_name() {
        let encoded = encode_query_value(
            "my-project",
            &Value::DocRef(DocRef {
                path: "users/1".to_string(),
            }),
        )
        .unwrap();
        assert_eq!(
            encoded.value_type,
            Some(value::ValueType::ReferenceValue(
                "projects/my-project/databases/(default)/documents/users/1".to_string()
            ))
        );
    }

    #[test]
    fn query_encoder_recurses_through_arrays_and_maps() {
        let value = Value::Map(BTreeMap::from([
            (
                "owner".to_string(),
                Value::DocRef(DocRef {
                    path: "users/owner".to_string(),
                }),
            ),
            (
                "nested".to_string(),
                Value::Array(vec![
                    Value::Int(7),
                    Value::Map(BTreeMap::from([(
                        "watchers".to_string(),
                        Value::Array(vec![Value::DocRef(DocRef {
                            path: "users/watcher".to_string(),
                        })]),
                    )])),
                ]),
            ),
        ]));
        let encoded = encode_query_value("my-project", &value).unwrap();
        let Some(value::ValueType::MapValue(map)) = encoded.value_type else {
            panic!("expected map value");
        };
        assert_eq!(
            map.fields["owner"].value_type.as_ref(),
            Some(&value::ValueType::ReferenceValue(
                "projects/my-project/databases/(default)/documents/users/owner".to_string()
            ))
        );
        let Some(value::ValueType::ArrayValue(items)) = map.fields["nested"].value_type.as_ref()
        else {
            panic!("expected nested array");
        };
        let Some(value::ValueType::MapValue(nested_map)) = items.values[1].value_type.as_ref()
        else {
            panic!("expected nested map");
        };
        let Some(value::ValueType::ArrayValue(watchers)) =
            nested_map.fields["watchers"].value_type.as_ref()
        else {
            panic!("expected nested watcher array");
        };
        assert_eq!(
            watchers.values[0].value_type.as_ref(),
            Some(&value::ValueType::ReferenceValue(
                "projects/my-project/databases/(default)/documents/users/watcher".to_string()
            ))
        );
    }

    #[test]
    fn query_encoder_rejects_opaque_external_reference() {
        let err = encode_query_value(
            "my-project",
            &Value::Opaque(
                "Reference(projects/other-project/databases/(default)/documents/users/1)"
                    .to_string(),
            ),
        )
        .unwrap_err();
        assert!(
            format!("{err}").contains("unsupported value in emulator query"),
            "{err}"
        );
    }

    #[test]
    fn query_encoder_rejects_external_resource_name_in_doc_ref() {
        let err = encode_query_value(
            "my-project",
            &Value::DocRef(DocRef {
                path: "projects/other-project/databases/(default)/documents/users/1".to_string(),
            }),
        )
        .unwrap_err();
        assert!(
            format!("{err}").contains("invalid document path in emulator query reference"),
            "{err}"
        );
    }

    #[test]
    fn write_encoder_still_rejects_doc_ref() {
        let err = encode_field_value(&Value::DocRef(DocRef {
            path: "users/1".to_string(),
        }))
        .unwrap_err();
        assert!(
            format!("{err}").contains("unsupported value in emulator write"),
            "{err}"
        );
    }
}
