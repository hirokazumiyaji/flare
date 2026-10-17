use flare::firestore::WriteOp;
use flare::value::{Timestamp, Value};
use flare::{Firestore, InMemoryFirestore};
use proptest::prelude::*;
use std::collections::BTreeMap;

fn arb_leaf_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        any::<String>().prop_map(Value::Str),
        any::<i64>().prop_map(Value::Int),
        any::<bool>().prop_map(Value::Bool),
        Just(Value::Nil),
        any::<i64>().prop_map(|s| Value::Timestamp(Timestamp { seconds: s })),
        any::<Vec<u8>>().prop_map(Value::Bytes),
    ]
}

fn arb_field_name() -> impl Strategy<Value = String> {
    "[a-zA-Z][a-zA-Z0-9_]{0,15}"
}

fn arb_value() -> impl Strategy<Value = Value> {
    let leaf = arb_leaf_value();
    leaf.prop_recursive(
        4,  // max depth
        64, // max size
        8,  // items per collection
        |inner| {
            prop_oneof![
                prop::collection::btree_map(arb_field_name(), inner.clone(), 0..8)
                    .prop_map(Value::Map),
                prop::collection::vec(
                    prop_oneof![
                        arb_leaf_value(),
                        prop::collection::btree_map(arb_field_name(), inner, 0..8)
                            .prop_map(Value::Map)
                    ],
                    0..8
                )
                .prop_map(Value::Array),
            ]
        },
    )
}

fn arb_map_value() -> impl Strategy<Value = BTreeMap<String, Value>> {
    prop::collection::btree_map(arb_field_name(), arb_value(), 0..10)
}

proptest! {
    #[test]
    fn prop_in_memory_firestore_round_trip(data in arb_map_value()) {
        let mut fs = InMemoryFirestore::default();
        fs.apply(vec![WriteOp::Set {
            path: "test_col/test_doc".to_string(),
            data: data.clone(),
            merge: false,
        }])
        .expect("in-memory set must succeed");

        let retrieved = fs
            .get("test_col/test_doc")
            .expect("in-memory get must succeed")
            .expect("document must exist");

        if let Value::Map(retrieved_map) = retrieved {
            for (k, v) in &data {
                prop_assert_eq!(retrieved_map.get(k), Some(v));
            }
        } else {
            panic!("retrieved document must be a Map");
        }
    }
}

#[cfg(feature = "emulator")]
proptest! {
    #[test]
    fn prop_emulator_codec_round_trip(val in arb_value()) {
        use flare::firestore::{decode_field_value_bounded, encode_field_value_bounded};

        let encoded = encode_field_value_bounded(&val, 0, 100)
            .expect("encoding valid Value within depth should succeed");
        let decoded = decode_field_value_bounded(&encoded, "projects/test/databases/(default)", 0, 100)
            .expect("decoding should succeed");
        prop_assert_eq!(decoded, val);
    }
}
