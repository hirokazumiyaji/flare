//! `Value` ⇔ SDK `Value` のコーデック。`__name__` の付与はしない
//! (`__name__` は runtime が両バックエンド共通で付ける)。

use std::collections::{BTreeMap, HashMap};
use std::error::Error;

use gcloud_sdk::google::firestore::v1::{value, ArrayValue, Document, MapValue, Value as SdkValue};
use prost_types::{NullValue, Timestamp as ProtoTimestamp};

use super::path::relative_document_name;
use crate::value::{DocRef, Timestamp, Value};

/// Codec recursion is bounded so a deeply nested document cannot overflow the
/// native stack.  The root field is depth 0, matching `max_value_depth`.
fn too_deep(max_depth: usize) -> Box<dyn Error> {
    format!("value nesting limit exceeded (max {max_depth})").into()
}

pub(super) fn encode_array_items_bounded(
    items: &[Value],
    depth: usize,
    max_depth: usize,
) -> Result<Vec<SdkValue>, Box<dyn Error>> {
    items
        .iter()
        .map(|item| encode_field_value_bounded(item, depth, max_depth))
        .collect()
}

pub(super) fn encode_fields_bounded(
    map: &BTreeMap<String, Value>,
    depth: usize,
    max_depth: usize,
) -> Result<HashMap<String, SdkValue>, Box<dyn Error>> {
    let mut out = HashMap::new();
    for (k, v) in map {
        out.insert(k.clone(), encode_field_value_bounded(v, depth, max_depth)?);
    }
    Ok(out)
}

pub(super) fn encode_field_value_bounded(
    v: &Value,
    depth: usize,
    max_depth: usize,
) -> Result<SdkValue, Box<dyn Error>> {
    if depth > max_depth {
        return Err(too_deep(max_depth));
    }
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
                values: encode_array_items_bounded(items, depth + 1, max_depth)?,
            }),
            Value::Map(m) => value::ValueType::MapValue(MapValue {
                fields: encode_fields_bounded(m, depth + 1, max_depth)?,
            }),
            _ => return Err(format!("unsupported value in emulator write: {v:?}").into()),
        }),
    })
}

pub(super) fn decode_document_bounded(
    doc: &Document,
    max_depth: usize,
) -> Result<Value, Box<dyn Error>> {
    let mut out = BTreeMap::new();
    for (k, fv) in &doc.fields {
        out.insert(k.clone(), decode_field_value_bounded(fv, 0, max_depth)?);
    }
    Ok(Value::Map(out))
}

pub(super) fn decode_field_value_bounded(
    v: &SdkValue,
    depth: usize,
    max_depth: usize,
) -> Result<Value, Box<dyn Error>> {
    if depth > max_depth {
        return Err(too_deep(max_depth));
    }
    decode_field_value_inner(v, depth, max_depth)
}

fn decode_field_value_inner(
    v: &SdkValue,
    depth: usize,
    max_depth: usize,
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
                out.push(decode_field_value_bounded(x, depth + 1, max_depth)?);
            }
            Ok(Value::Array(out))
        }
        value::ValueType::MapValue(map) => {
            let mut out = BTreeMap::new();
            for (k, fv) in &map.fields {
                out.insert(
                    k.clone(),
                    decode_field_value_bounded(fv, depth + 1, max_depth)?,
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
        value::ValueType::ReferenceValue(name) => match relative_document_name(name) {
            Ok(rel) => Ok(Value::DocRef(DocRef { path: rel })),
            Err(_) => Ok(Value::Opaque(format!("Reference({name})"))),
        },
        other => Err(format!("unsupported Firestore field value: {other:?}").into()),
    }
}

fn hex_bytes(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::DEFAULT_MAX_VALUE_DEPTH;

    /// Convenience wrappers at the shipped default depth so the codec's
    /// behaviour tests stay focused on shape rather than on the budget.
    fn decode_document(doc: &Document) -> Result<Value, Box<dyn Error>> {
        decode_document_bounded(doc, DEFAULT_MAX_VALUE_DEPTH)
    }

    fn decode_field_value(v: &SdkValue) -> Result<Value, Box<dyn Error>> {
        decode_field_value_bounded(v, 0, DEFAULT_MAX_VALUE_DEPTH)
    }

    fn encode_field_value(v: &Value) -> Result<SdkValue, Box<dyn Error>> {
        encode_field_value_bounded(v, 0, DEFAULT_MAX_VALUE_DEPTH)
    }

    #[test]
    fn decode_document_has_no_name_field() {
        // `__name__` の付与は runtime が行う。decode はフィールドのみ返す。
        let doc = Document {
            name: "projects/p/databases/(default)/documents/users/x".to_string(),
            fields: HashMap::new(),
            create_time: None,
            update_time: None,
        };
        let Value::Map(m) = decode_document(&doc).unwrap() else {
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
        let Value::Map(m) = decode_document(&doc).unwrap() else {
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
            name: "projects/p/databases/(default)/documents/teams/t1/documents/x".to_string(),
            fields: HashMap::new(),
            create_time: None,
            update_time: None,
        };
        let Value::Map(m) = decode_document(&doc).unwrap() else {
            panic!();
        };
        assert!(m.is_empty());
        assert_eq!(
            relative_document_name(&doc.name).unwrap(),
            "teams/t1/documents/x"
        );
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
        let Value::Map(m) = decode_document(&doc).unwrap() else {
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

    /// Build a value nested `depth` levels: `[0]` at the root, then `depth - 1`
    /// further array wrappers.
    fn nested(depth: usize) -> Value {
        let mut v = Value::Int(0);
        for _ in 0..depth {
            v = Value::Array(vec![v]);
        }
        v
    }

    #[test]
    fn encode_rejects_values_deeper_than_the_budget() {
        // The root is depth 0, so a 4-deep value is legal at max 4.
        assert!(encode_field_value_bounded(&nested(4), 0, 4).is_ok());
        let err = encode_field_value_bounded(&nested(5), 0, 4).unwrap_err();
        assert!(
            format!("{err}").contains("value nesting limit exceeded"),
            "{err}"
        );
    }

    #[test]
    fn decode_rejects_documents_deeper_than_the_budget() {
        // A 2000-deep stored document must be a controlled error, not a stack
        // overflow. Only the bounded entry point is allowed to see it.
        let mut sdk = SdkValue {
            value_type: Some(value::ValueType::IntegerValue(0)),
        };
        for _ in 0..2_000 {
            sdk = SdkValue {
                value_type: Some(value::ValueType::ArrayValue(ArrayValue {
                    values: vec![sdk],
                })),
            };
        }
        let err = decode_field_value_bounded(&sdk, 0, 64).unwrap_err();
        assert!(
            format!("{err}").contains("value nesting limit exceeded"),
            "{err}"
        );
    }

    #[test]
    fn default_bounded_entry_points_use_the_shipped_default() {
        assert!(encode_field_value(&nested(8)).is_ok());
        assert!(encode_field_value(&nested(200)).is_err());
    }
}
