//! `Value` ⇔ SDK `Value` のコーデック。`__name__` の付与はしない
//! (`__name__` は runtime が両バックエンド共通で付ける)。

use std::collections::{BTreeMap, HashMap};
use std::error::Error;

use gcloud_sdk::google::firestore::v1::{value, ArrayValue, Document, MapValue, Value as SdkValue};
use prost_types::{NullValue, Timestamp as ProtoTimestamp};

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
            Value::Bytes(bytes) => value::ValueType::BytesValue(bytes.clone()),
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

/// Decode a document using the resource root of the Firestore client that
/// returned it. References outside that root stay `Opaque` instead of being
/// mistaken for paths in the current database.
pub(super) fn decode_document(
    doc: &Document,
    resource_root: &str,
) -> Result<Value, Box<dyn Error>> {
    let mut out = BTreeMap::new();
    for (k, fv) in &doc.fields {
        out.insert(k.clone(), decode_field_value(fv, resource_root)?);
    }
    Ok(Value::Map(out))
}

pub(super) fn decode_field_value(
    v: &SdkValue,
    resource_root: &str,
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
        value::ValueType::BytesValue(bytes) => Ok(Value::Bytes(bytes.clone())),
        value::ValueType::ArrayValue(arr) => {
            let mut out = vec![];
            for x in &arr.values {
                out.push(decode_field_value(x, resource_root)?);
            }
            Ok(Value::Array(out))
        }
        value::ValueType::MapValue(map) => {
            let mut out = BTreeMap::new();
            for (k, fv) in &map.fields {
                out.insert(k.clone(), decode_field_value(fv, resource_root)?);
            }
            Ok(Value::Map(out))
        }
        // firelang が持たない型は読めることだけを目標にする。double・
        // geo_point は表示専用の Opaque にし、bytes は第一級値として
        // 扱い、reference は現在の resource root と完全一致する場合だけ
        // 相対パスの DocRef にする。別 project/database の reference は
        // 元の resource name を保持する。書き戻しは unsupported のまま(§3)。
        value::ValueType::DoubleValue(f) => Ok(Value::Opaque(format!("Double({f})"))),
        value::ValueType::GeoPointValue(p) => Ok(Value::Opaque(format!(
            "GeoPoint({}, {})",
            p.latitude, p.longitude
        ))),
        value::ValueType::ReferenceValue(name) => match local_reference_path(name, resource_root) {
            Some(path) => Ok(Value::DocRef(DocRef {
                path: path.to_string(),
            })),
            None => Ok(Value::Opaque(format!("Reference({name})"))),
        },
        other => Err(format!("unsupported Firestore field value: {other:?}").into()),
    }
}

/// Return a reference's path only when the complete Firestore resource root
/// matches. Requiring the separator after the root prevents a name such as
/// `.../documents-archive/...` from being accepted as a local reference.
fn local_reference_path<'a>(name: &'a str, resource_root: &str) -> Option<&'a str> {
    let root = resource_root.strip_suffix('/').unwrap_or(resource_root);
    let suffix = name.strip_prefix(root)?;
    let path = suffix.strip_prefix('/')?;
    if path.is_empty() || path.starts_with('/') {
        None
    } else {
        Some(path)
    }
}


#[cfg(test)]
mod tests {
    use super::super::path::relative_document_name;
    use super::*;

    const RESOURCE_ROOT: &str = "projects/p/databases/(default)/documents";

    fn sdk(value_type: value::ValueType) -> SdkValue {
        SdkValue {
            value_type: Some(value_type),
        }
    }

    fn reference(path: &str) -> SdkValue {
        sdk(value::ValueType::ReferenceValue(format!(
            "{RESOURCE_ROOT}/{path}"
        )))
    }

    #[test]
    fn decode_document_has_no_name_field() {
        // `__name__` の付与は runtime が行う。decode はフィールドのみ返す。
        let doc = Document {
            name: format!("{RESOURCE_ROOT}/users/x"),
            fields: HashMap::new(),
            create_time: None,
            update_time: None,
        };
        let Value::Map(m) = decode_document(&doc, RESOURCE_ROOT).unwrap() else {
            panic!();
        };
        assert!(!m.contains_key("__name__"));
        assert!(m.is_empty());
    }

    #[test]
    fn decode_document_returns_fields_without_name() {
        let doc = Document {
            name: format!("{RESOURCE_ROOT}/users/x"),
            fields: HashMap::from([(
                "a".to_string(),
                sdk(value::ValueType::StringValue("v".to_string())),
            )]),
            create_time: None,
            update_time: None,
        };
        let Value::Map(m) = decode_document(&doc, RESOURCE_ROOT).unwrap() else {
            panic!();
        };
        assert_eq!(m.get("a"), Some(&Value::Str("v".to_string())));
        assert!(!m.contains_key("__name__"));
    }

    #[test]
    fn decode_document_ignores_nested_documents_in_path() {
        // パス中の "documents" は相対パス化(呼び出し側)で保持される。
        // decode 自体は名前に触らない。
        let doc = Document {
            name: format!("{RESOURCE_ROOT}/teams/t1/documents/x"),
            fields: HashMap::new(),
            create_time: None,
            update_time: None,
        };
        let Value::Map(m) = decode_document(&doc, RESOURCE_ROOT).unwrap() else {
            panic!();
        };
        assert!(m.is_empty());
        assert_eq!(
            relative_document_name(&doc.name).unwrap(),
            "teams/t1/documents/x"
        );
    }

    #[test]
    fn decode_exotic_field_values_as_first_class_or_opaque_values() {
        let sdk = |t| SdkValue {
            value_type: Some(t),
        };

        assert_eq!(
            decode_field_value(&sdk(value::ValueType::DoubleValue(1.5)), RESOURCE_ROOT,).unwrap(),
            Value::Opaque("Double(1.5)".to_string())
        );
        assert_eq!(
            decode_field_value(
                &sdk(value::ValueType::BytesValue(vec![0x00, 0xff])),
                RESOURCE_ROOT,
            )
            .unwrap(),
            Value::Bytes(vec![0x00, 0xff])
        );
        assert_eq!(
            decode_field_value(
                &sdk(value::ValueType::GeoPointValue(
                    gcloud_sdk::google::r#type::LatLng {
                        latitude: 35.7,
                        longitude: 139.7,
                    },
                )),
                RESOURCE_ROOT,
            )
            .unwrap(),
            Value::Opaque("GeoPoint(35.7, 139.7)".to_string())
        );
        assert_eq!(
            decode_field_value(&reference("users/1"), RESOURCE_ROOT).unwrap(),
            Value::DocRef(DocRef {
                path: "users/1".to_string()
            })
        );
    }

    #[test]
    fn references_outside_the_current_resource_root_stay_opaque() {
        let cases = [
            "projects/other-project/databases/(default)/documents/users/1",
            "projects/p/databases/other-database/documents/users/1",
            "projects/p/databases/(default)/documents-archive/users/1",
        ];
        for name in cases {
            assert_eq!(
                decode_field_value(
                    &sdk(value::ValueType::ReferenceValue(name.to_string())),
                    RESOURCE_ROOT,
                )
                .unwrap(),
                Value::Opaque(format!("Reference({name})"))
            );
        }
    }

    #[test]
    fn nested_arrays_and_maps_propagate_the_resource_root() {
        let nested = sdk(value::ValueType::ArrayValue(ArrayValue {
            values: vec![sdk(value::ValueType::MapValue(MapValue {
                fields: HashMap::from([
                    ("local".to_string(), reference("users/1")),
                    (
                        "external_project".to_string(),
                        sdk(value::ValueType::ReferenceValue(
                            "projects/other-project/databases/(default)/documents/users/2"
                                .to_string(),
                        )),
                    ),
                    (
                        "nested".to_string(),
                        sdk(value::ValueType::ArrayValue(ArrayValue {
                            values: vec![sdk(value::ValueType::ReferenceValue(
                                "projects/p/databases/other-database/documents/users/3".to_string(),
                            ))],
                        })),
                    ),
                ]),
            }))],
        }));

        assert_eq!(
            decode_field_value(&nested, RESOURCE_ROOT).unwrap(),
            Value::Array(vec![Value::Map(BTreeMap::from([
                (
                    "local".to_string(),
                    Value::DocRef(DocRef {
                        path: "users/1".to_string()
                    })
                ),
                (
                    "external_project".to_string(),
                    Value::Opaque(
                        "Reference(projects/other-project/databases/(default)/documents/users/2)"
                            .to_string()
                    )
                ),
                (
                    "nested".to_string(),
                    Value::Array(vec![Value::Opaque(
                        "Reference(projects/p/databases/other-database/documents/users/3)"
                            .to_string()
                    )])
                ),
            ]))])
        );
    }

    #[test]
    fn decode_document_with_exotic_fields_does_not_fail() {
        let doc = Document {
            name: format!("{RESOURCE_ROOT}/users/x"),
            fields: HashMap::from([
                (
                    "name".to_string(),
                    sdk(value::ValueType::StringValue("a".to_string())),
                ),
                ("score".to_string(), sdk(value::ValueType::DoubleValue(4.5))),
                ("ref".to_string(), reference("users/1")),
                (
                    "external".to_string(),
                    sdk(value::ValueType::ReferenceValue(
                        "projects/other-project/databases/(default)/documents/users/2".to_string(),
                    )),
                ),
            ]),
            create_time: None,
            update_time: None,
        };
        let Value::Map(m) = decode_document(&doc, RESOURCE_ROOT).unwrap() else {
            panic!();
        };
        assert_eq!(m.get("name"), Some(&Value::Str("a".to_string())));
        assert_eq!(
            m.get("score"),
            Some(&Value::Opaque("Double(4.5)".to_string()))
        );
        assert_eq!(
            m.get("ref"),
            Some(&Value::DocRef(DocRef {
                path: "users/1".to_string()
            }))
        );
        assert_eq!(
            m.get("external"),
            Some(&Value::Opaque(
                "Reference(projects/other-project/databases/(default)/documents/users/2)"
                    .to_string()
            ))
        );
    }

    #[test]
    fn bytes_encode_and_decode_losslessly_when_nested() {
        let value = Value::Array(vec![Value::Map(BTreeMap::from([(
            "payload".to_string(),
            Value::Bytes(vec![0x00, 0x7f, 0x80, 0xff]),
        )]))]);
        let encoded = encode_field_value(&value).unwrap();
        assert_eq!(decode_field_value(&encoded, RESOURCE_ROOT).unwrap(), value);
    }

    #[test]
    fn encode_opaque_value_is_rejected_on_write() {
        let err = encode_field_value(&Value::Opaque("Double(1.5)".to_string())).unwrap_err();
        assert!(
            format!("{err}").contains("unsupported value in emulator write"),
            "{err}"
        );
    }
}
