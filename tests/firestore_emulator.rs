//! Integration tests against the Firestore emulator (`FIRESTORE_EMULATOR_HOST`).
//! Locally: build with `--features emulator`, start the emulator, or skip when
//! the host is unset. A configured but invalid or unreachable emulator is a
//! test failure, not a skip. CI: see `.github/workflows/ci.yml`
//! (`firebase emulators:exec` sets the host automatically).

#[macro_use]
mod common;

use std::collections::HashMap;

use firelang::{run_script, run_script_with_firestore};
use firelang::{FirestoreEmulator, Value};
use firestore::{FirestoreCreateSupport, FirestoreDb, FirestoreDbOptions};
use gcloud_sdk::google::firestore::v1::{value, ArrayValue, Document, MapValue, Value as SdkValue};

fn emulator() -> Option<FirestoreEmulator> {
    match FirestoreEmulator::try_from_env() {
        Ok(Some(emulator)) => Some(emulator),
        Ok(None) => None,
        Err(error) => {
            panic!("FIRESTORE_EMULATOR_HOST is configured, but emulator setup failed: {error}")
        }
    }
}
fn unique(prefix: &str) -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{prefix}_{n}")
}

fn reference_value(project: &str, database: &str, path: &str) -> SdkValue {
    SdkValue {
        value_type: Some(value::ValueType::ReferenceValue(format!(
            "projects/{project}/databases/{database}/documents/{path}"
        ))),
    }
}

fn map_value(fields: impl IntoIterator<Item = (&'static str, SdkValue)>) -> SdkValue {
    SdkValue {
        value_type: Some(value::ValueType::MapValue(MapValue {
            fields: fields
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect::<HashMap<_, _>>(),
        })),
    }
}

fn array_value(values: impl IntoIterator<Item = SdkValue>) -> SdkValue {
    SdkValue {
        value_type: Some(value::ValueType::ArrayValue(ArrayValue {
            values: values.into_iter().collect(),
        })),
    }
}

fn seed_reference_documents(
    project_id: &str,
    collection_id: &str,
    documents: impl IntoIterator<Item = (&'static str, HashMap<String, SdkValue>)>,
) {
    let host = std::env::var("FIRESTORE_EMULATOR_HOST").expect("emulator host");
    let url = if host.contains("://") {
        host
    } else {
        format!("http://{host}")
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("create test runtime");
    let options = FirestoreDbOptions::new(project_id.to_string()).with_firebase_api_url(url);
    let db = runtime
        .block_on(FirestoreDb::with_options_token_source(
            options,
            gcloud_sdk::GCP_DEFAULT_SCOPES.clone(),
            gcloud_sdk::TokenSourceType::ExternalSource(Box::new(
                gcloud_sdk::ExternalJwtFunctionSource::new(|| {
                    std::future::ready(Ok(gcloud_sdk::Token::new(
                        "Bearer".to_string(),
                        "owner".into(),
                        chrono::DateTime::<chrono::Utc>::from_timestamp(4_102_444_800, 0)
                            .expect("valid token expiry"),
                    )))
                }),
            )),
        ))
        .expect("connect test Firestore client");
    for (id, fields) in documents {
        runtime
            .block_on(db.create_doc(
                collection_id,
                Some(id.to_string()),
                Document {
                    name: String::new(),
                    fields,
                    create_time: None,
                    update_time: None,
                },
                None,
            ))
            .expect("seed reference document");
    }
}

#[test]
fn firestore_global_functions_with_emulator() {
    let Some(fs) = emulator() else {
        eprintln!("skip: FIRESTORE_EMULATOR_HOST is not set");
        return;
    };
    let doc_path = format!("users/{}", unique("smoke"));
    let script = format!("set(doc(\"{doc_path}\"), {{value: \"ok\"}})\nget(\"{doc_path}\")");
    let out = run_script_with_firestore(&script, fs).unwrap();
    assert_eq!(out, map! {"value" => Value::Str("ok".to_string())});
}

#[test]
fn emulator_path_segment_id_errors_match_in_memory() {
    if emulator().is_none() {
        eprintln!("skip: no emulator");
        return;
    }

    let long_id = "x".repeat(1501);
    let sources = [
        r#"set("users/.", {a: 1})"#.to_string(),
        r#"set("users/..", {a: 1})"#.to_string(),
        r#"set("users/__reserved__", {a: 1})"#.to_string(),
        format!(r#"set("users/{long_id}", {{a: 1}})"#),
        r#"col("teams/../members")"#.to_string(),
        r#"col_group("__reserved__")"#.to_string(),
        r#"col("users").doc(".")"#.to_string(),
    ];

    for source in sources {
        let in_memory = run_script(&source).unwrap_err().to_string();
        let Some(fs) = emulator() else {
            eprintln!("skip: emulator disconnected");
            return;
        };
        let emulator_error = run_script_with_firestore(&source, fs)
            .unwrap_err()
            .to_string();
        assert_eq!(emulator_error, in_memory, "{source}");
    }
}

#[test]
fn emulator_accepts_1500_byte_path_segment() {
    let Some(fs) = emulator() else {
        eprintln!("skip: no emulator");
        return;
    };
    let id = "x".repeat(1500);
    let collection = format!("users/{}/docs", unique("id_limit"));
    let path = format!("{collection}/{id}");
    let source = format!(
        r#"set("{path}", {{ok: true}})
get("{path}")"#
    );
    let out = run_script_with_firestore(&source, fs).unwrap();
    assert_eq!(out, map! {"ok" => Value::Bool(true)});
}

#[test]
fn emulator_merge_set() {
    let Some(fs) = emulator() else {
        eprintln!("skip: FIRESTORE_EMULATOR_HOST is not set");
        return;
    };
    let p = unique("merge");
    let path = format!("users/{p}");
    // Same pattern as in-memory keyword merge test (lib.rs).
    let script = format!(
        "set(\"{path}\", {{count: 1}})\nset(\"{path}\", {{count: 2}}, merge: true)\nget(\"{path}\")"
    );
    let out = run_script_with_firestore(&script, fs).unwrap();
    assert_eq!(out, map! {"count" => Value::Int(2)});
}

#[test]
fn emulator_query_where_and_limit() {
    let Some(fs) = emulator() else {
        eprintln!("skip: FIRESTORE_EMULATOR_HOST is not set");
        return;
    };
    let id = unique("q");
    let script = format!(
        r#"set("users/{id}_z", {{age: 10}})
set("users/{id}_a", {{age: 30}})
set("users/{id}_m", {{age: 25}})
col("users").where("age", ">=", 20).limit(2).get()"#
    );
    let out = run_script_with_firestore(&script, fs).unwrap();
    let Value::Array(rows) = out else {
        panic!("expected array");
    };
    assert_eq!(rows.len(), 2);
    for (row, suffix) in rows.iter().zip([format!("{id}_a"), format!("{id}_m")]) {
        let Value::Map(m) = row else {
            panic!("expected map row");
        };
        assert_eq!(
            m.get("__name__"),
            Some(&Value::Str(format!("users/{suffix}")))
        );
    }
}

#[test]
fn emulator_document_name_filters_match_relative_paths() {
    let Some(fs) = emulator() else {
        eprintln!("skip: no emulator");
        return;
    };
    let id = unique("name");
    let first = format!("users/{id}/docs/1");
    let second = format!("users/{id}/docs/2");
    let script = format!(
        r#"set("{first}", {{n: 1}})
set("{second}", {{n: 2}})
[
  col("users/{id}/docs").where("__name__", "==", "{first}").get().len(),
  col("users/{id}/docs").where("__name__", "!=", "{first}").get().len(),
  col("users/{id}/docs").where("__name__", "in", ["{first}", "{second}"]).get().len()
]"#
    );
    let out = run_script_with_firestore(&script, fs).unwrap();
    assert_eq!(
        out,
        Value::Array(vec![Value::Int(1), Value::Int(1), Value::Int(2)])
    );
}

#[test]
fn emulator_collection_group_query() {
    let Some(fs) = emulator() else {
        eprintln!("skip: FIRESTORE_EMULATOR_HOST is not set");
        return;
    };
    let id = unique("cg");
    let script = format!(
        r#"set("teams/t1/members/{id}_x", {{active: true}})
set("teams/t2/members/{id}_y", {{active: true}})
col_group("members").where("active", "==", true).get()"#
    );
    let out = run_script_with_firestore(&script, fs).unwrap();
    // 他テストの docs が混ざってもよいよう、対象行の存在で判定する。
    assert!(
        out.inspect().contains(&format!("teams/t1/members/{id}_x")),
        "{}",
        out.inspect()
    );
}

#[test]
fn emulator_nested_map_roundtrip() {
    let Some(fs) = emulator() else {
        eprintln!("skip: FIRESTORE_EMULATOR_HOST is not set");
        return;
    };
    let p = unique("nest");
    let path = format!("cfg/{p}");
    let script = format!(
        r#"set("{path}", {{outer: {{inner: 42}}}})
get("{path}")"#
    );
    let out = run_script_with_firestore(&script, fs).unwrap();
    assert_eq!(out, map! {"outer" => map! {"inner" => Value::Int(42)}});
}

#[test]
fn emulator_value_validation_matches_in_memory() {
    // The preflight validator must reject these before the emulator RPC, with
    // the same rules as the in-memory backend.
    let run = unique("values");
    for (source, expected) in [
        (
            format!(r#"set("users/{run}/docs/direct", {{values: [[1]]}})"#),
            "Nested arrays",
        ),
        (
            format!(r#"set("users/{run}/docs/empty", {{"": 1}})"#),
            "empty string",
        ),
        (
            format!(r#"set("users/{run}/docs/reserved", {{__name__: 1}})"#),
            "reserved",
        ),
        (
            format!(r#"set("users/{run}/docs/nested", {{outer: [{{"__secret__": 1}}]}})"#),
            "reserved",
        ),
    ] {
        let Some(fs) = emulator() else {
            eprintln!("skip: no emulator");
            return;
        };
        let err = run_script_with_firestore(&source, fs).unwrap_err();
        assert!(format!("{err}").contains(expected), "{source}: {err}");
    }

    let Some(fs) = emulator() else {
        eprintln!("skip: no emulator");
        return;
    };
    let path = format!("users/{run}/docs/valid");
    let source = format!(
        r#"set("{path}", {{rows: [{{values: [1, 2]}}, {{values: [3]}}]}})
get("{path}")"#
    );
    let out = run_script_with_firestore(&source, fs).unwrap();
    assert_eq!(
        out.inspect(),
        "{\"rows\" => [{\"values\" => [1, 2]}, {\"values\" => [3]}]}"
    );
}

#[test]
fn emulator_bytes_roundtrip_and_query_equality() {
    let Some(fs) = emulator() else {
        eprintln!("skip: no emulator");
        return;
    };
    let run = unique("bytes");
    let script = format!(
        r#"
set("files/{run}/docs/a", {{payload: bytes([0, 255]), parts: [bytes(""), bytes([0, 127, 128, 255])], meta: {{checksum: bytes([1, 2])}}}})
set("files/{run}/docs/b", {{payload: bytes([1, 0])}})
set("files/{run}/docs/c", {{payload: bytes([0, 255, 1])}})
rows = col("files/{run}/docs").where("payload", "==", bytes([0, 255])).get()
[
  get("files/{run}/docs/a")["payload"],
  get("files/{run}/docs/a")["parts"],
  rows[0]["meta"],
  rows.len(),
  col("files/{run}/docs").order_by("payload", "asc").get().map {{ |row| row["__name__"] }},
]
"#
    );
    let out = run_script_with_firestore(&script, fs).unwrap();
    assert_eq!(
        out,
        Value::Array(vec![
            Value::Bytes(vec![0, 255]),
            Value::Array(vec![
                Value::Bytes(vec![]),
                Value::Bytes(vec![0, 127, 128, 255]),
            ]),
            map! {"checksum" => Value::Bytes(vec![1, 2])},
            Value::Int(1),
            Value::Array(vec![
                Value::Str(format!("files/{run}/docs/a")),
                Value::Str(format!("files/{run}/docs/c")),
                Value::Str(format!("files/{run}/docs/b")),
            ]),
        ])
    );
}

#[test]
fn emulator_server_timestamp_resolves() {
    let Some(fs) = emulator() else {
        eprintln!("skip: FIRESTORE_EMULATOR_HOST is not set");
        return;
    };
    let p = unique("st");
    let path = format!("users/{p}");
    let script = format!("set(\"{path}\", {{t: server_timestamp()}})\nget(\"{path}\")");
    let out = run_script_with_firestore(&script, fs).unwrap();
    let Value::Map(m) = out else {
        panic!("expected map");
    };
    assert!(matches!(m.get("t"), Some(Value::Timestamp(_))));
}

#[test]
fn emulator_batch_atomic_commit() {
    let Some(fs) = emulator() else {
        eprintln!("skip: FIRESTORE_EMULATOR_HOST is not set");
        return;
    };
    let a = unique("ba");
    let b = unique("bb");
    let script = format!(
        r#"batch {{
  set("users/{a}", {{n: 1}})
  set("users/{b}", {{n: 2}})
}}
get("users/{b}")"#
    );
    let out = run_script_with_firestore(&script, fs).unwrap();
    assert_eq!(out, map! {"n" => Value::Int(2)});
}

#[test]
fn emulator_transaction_read_then_write() {
    let Some(fs) = emulator() else {
        eprintln!("skip: FIRESTORE_EMULATOR_HOST is not set");
        return;
    };
    let p = unique("tx");
    let path = format!("users/{p}");
    let script = format!(
        r#"set("{path}", {{before: 1}})
transaction {{
  v = get("{path}")
  set("{path}", {{after: 2}})
}}
get("{path}")"#
    );
    let out = run_script_with_firestore(&script, fs).unwrap();
    assert_eq!(out, map! {"after" => Value::Int(2)});
}

#[test]
fn emulator_transaction_multiple_gets_before_write() {
    let Some(fs) = emulator() else {
        eprintln!("skip: FIRESTORE_EMULATOR_HOST is not set");
        return;
    };
    let p = unique("mg");
    let path_a = format!("users/{p}_a");
    let path_b = format!("users/{p}_b");
    let script = format!(
        r#"set("{path_a}", {{x: 1}})
set("{path_b}", {{y: 2}})
transaction {{
  a = get("{path_a}")
  b = get("{path_b}")
  set("{path_a}", {{sum: 3}})
}}
get("{path_a}")"#
    );
    let out = run_script_with_firestore(&script, fs).unwrap();
    assert_eq!(out, map! {"sum" => Value::Int(3)});
}

#[test]
fn emulator_update_missing_fails() {
    let Some(fs) = emulator() else {
        eprintln!("skip: FIRESTORE_EMULATOR_HOST is not set");
        return;
    };
    let p = unique("nom");
    let path = format!("users/{p}_missing");
    let script = format!(r#"update("{path}", {{x: 1}})"#);
    let err = run_script_with_firestore(&script, fs).unwrap_err();
    assert!(
        format!("{err}").contains("not found") || format!("{err}").contains("NOT_FOUND"),
        "{err}"
    );
}

#[test]
fn emulator_where_compare_matches_in_memory_semantics() {
    // Same case as in-memory `where_comparison_supports_str_timestamp_and_bool`.
    // Fixtures live in a per-run subcollection so the collection-scoped
    // queries stay repeatable against a long-lived local emulator.
    let Some(fs) = emulator() else {
        eprintln!("skip: FIRESTORE_EMULATOR_HOST is not set");
        return;
    };
    let run = unique("cmp");
    let script = format!(
        r#"set("users/{run}/rows/{run}_1", {{name: "alice", created: timestamp(100), active: true}})
set("users/{run}/rows/{run}_2", {{name: "bob", created: timestamp(200), active: false}})
[
  col("users/{run}/rows").where("name", ">", "a").get().len(),
  col("users/{run}/rows").where("created", ">", timestamp(150)).get().len(),
  col("users/{run}/rows").where("active", ">", false).get().len(),
]"#
    );
    let out = run_script_with_firestore(&script, fs).unwrap();
    assert_eq!(
        out,
        Value::Array(vec![Value::Int(2), Value::Int(1), Value::Int(1)])
    );
}

#[test]
fn emulator_where_cross_type_comparison_contract_is_shared() {
    // Same contract as in-memory
    // `where_cross_type_comparison_contract_is_shared`. The local matcher is
    // authoritative even when Firestore's server-side type ordering is active.
    let Some(fs) = emulator() else {
        eprintln!("skip: no emulator");
        return;
    };
    let run = unique("wherecmp");
    let collection = format!("users/{run}/rows");
    let setup = format!(
        r#"set("{collection}/int", {{v: 1}})
set("{collection}/str", {{v: "b"}})
set("{collection}/bool", {{v: true}})
set("{collection}/timestamp", {{v: timestamp(200)}})
set("{collection}/nil", {{v: nil}})
set("{collection}/array", {{v: [1]}})
set("{collection}/map", {{v: {{a: 1}}}})
set("{collection}/missing", {{other: 1}})"#,
    );
    let cases: &[(&str, &str, &str)] = &[
        (">", "0", "int"),
        (">=", "0", "int"),
        ("<", "2", "int"),
        ("<=", "2", "int"),
        (">", "\"a\"", "str"),
        (">=", "\"a\"", "str"),
        ("<", "\"c\"", "str"),
        ("<=", "\"c\"", "str"),
        (">", "false", "bool"),
        (">=", "false", "bool"),
        ("<", "true", ""),
        ("<=", "true", "bool"),
        (">", "timestamp(100)", "timestamp"),
        (">=", "timestamp(100)", "timestamp"),
        ("<", "timestamp(300)", "timestamp"),
        ("<=", "timestamp(300)", "timestamp"),
        (">", "nil", ""),
        (">=", "nil", ""),
        ("<", "nil", ""),
        ("<=", "nil", ""),
        (">", "[0]", ""),
        (">=", "[0]", ""),
        ("<", "[2]", ""),
        ("<=", "[2]", ""),
        (">", "{a: 0}", ""),
        (">=", "{a: 0}", ""),
        ("<", "{a: 2}", ""),
        ("<=", "{a: 2}", ""),
    ];
    let mut queries = Vec::new();
    let mut expected = Vec::new();
    for &(op, filter, suffix) in cases {
        queries.push(format!(
            "col(\"{collection}\").where(\"v\", \"{op}\", {filter}).get().map {{ |r| r[\"__name__\"] }}"
        ));
        let rows = if suffix.is_empty() {
            Vec::new()
        } else {
            vec![Value::Str(format!("{collection}/{suffix}"))]
        };
        expected.push(Value::Array(rows));
    }
    queries.push(format!(
        "col(\"{collection}\").where(\"v\", \"==\", nil).get().len()"
    ));
    queries.push(format!(
        "col(\"{collection}\").where(\"v\", \"!=\", nil).get().len()"
    ));
    expected.push(Value::Int(1));
    expected.push(Value::Int(6));
    queries.push(format!(
        "col(\"{collection}\").where(\"v\", \">\", \"a\").count()"
    ));
    expected.push(Value::Int(1));

    let script = format!("{setup}\n[\n{}\n]", queries.join(",\n"));
    let out = run_script_with_firestore(&script, fs).unwrap();
    assert_eq!(out, Value::Array(expected));
}

#[test]
fn emulator_dotted_field_path_update_matches_in_memory() {
    // Same case as in-memory `update_supports_dotted_field_path_like_firestore`.
    let Some(fs) = emulator() else {
        eprintln!("skip: FIRESTORE_EMULATOR_HOST is not set");
        return;
    };
    let p = unique("fp");
    let path = format!("users/{p}");
    let script = format!(
        r#"set("{path}", {{profile: {{name: "a", age: 1}}, n: 5}})
update("{path}", {{"profile.name": "b"}})
set("{path}", {{"profile.age": 2}}, merge: true)
get("{path}")"#
    );
    let out = run_script_with_firestore(&script, fs).unwrap();
    assert_eq!(
        out,
        map! {
            "profile" => map! {
                "name" => Value::Str("b".to_string()),
                "age" => Value::Int(2)
            },
            "n" => Value::Int(5)
        }
    );
}

#[test]
fn emulator_overlapping_update_and_merge_set_paths_match_in_memory() {
    let run = unique("ov");
    for script in [
        format!(
            r#"set("users/{run}/docs/update", {{}})
update("users/{run}/docs/update", {{a: delete_field(), "a.b": 1}})"#
        ),
        format!(
            r#"set("users/{run}/docs/merge", {{}})
set("users/{run}/docs/merge", {{a: 1, "a.b": 2}}, merge: true)"#
        ),
    ] {
        let Some(fs) = emulator() else {
            eprintln!("skip: no emulator");
            return;
        };
        let err = run_script_with_firestore(&script, fs).unwrap_err();
        assert!(
            format!("{err}").contains("overlapping field paths: a and a.b"),
            "{script}: {err}"
        );
    }
}

#[test]
fn emulator_replacement_set_non_simple_transform_keys_match_in_memory() {
    // A replacement `set` treats every map key as one literal field name.
    // Every non-simple transform target therefore needs a quoted API path,
    // including dotted, hyphenated, digit-leading, and backtick-containing keys.
    let Some(fs) = emulator() else {
        eprintln!("skip: FIRESTORE_EMULATOR_HOST is not set");
        return;
    };
    let run = unique("gv");
    let script = format!(
        r#"set("users/{run}/docs/names", {{"a-b": 1, "1start_digit": 2, "a`b": 3, "a\\b": 4}})
set("users/{run}/docs/names", {{"a-b": increment(1), "1start_digit": increment(2), "a`b": increment(3), "a\\b": increment(4)}})
set("users/{run}/docs/dotted", {{"a.b": [1]}})
set("users/{run}/docs/dotted", {{"a.b": array_union([2])}})
set("users/{run}/docs/remove", {{"x.y": ["a"]}})
set("users/{run}/docs/remove", {{"x.y": array_remove(["a"])}})
[
  get("users/{run}/docs/names")["a-b"],
  get("users/{run}/docs/names")["1start_digit"],
  get("users/{run}/docs/names")["a`b"],
  get("users/{run}/docs/names")["a\\b"],
  get("users/{run}/docs/dotted")["a.b"],
  get("users/{run}/docs/remove")["x.y"],
]"#
    );
    let out = run_script_with_firestore(&script, fs).unwrap();
    assert_eq!(
        out,
        Value::Array(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
            Value::Array(vec![Value::Int(2)]),
            Value::Array(vec![]),
        ])
    );
}

#[test]
fn emulator_where_rejects_large_string_filter_values() {
    // Same case as in-memory `where_rejects_large_string_filter_values`.
    // Firestore rejects string query filter values of 1,500 bytes or more
    // (`value for s is too large to be used in a query`); a 1,499-byte filter
    // evaluates normally on both backends.
    let run = unique("gp");
    let p = "x".repeat(1500);
    let script = format!(
        r#"set("users/{run}/docs/gp1", {{s: "{p}a"}})
col("users/{run}/docs").where("s", "==", "{p}").get()"#
    );
    let Some(fs) = emulator() else {
        eprintln!("skip: FIRESTORE_EMULATOR_HOST is not set");
        return;
    };
    let err = run_script_with_firestore(&script, fs).unwrap_err();
    assert!(
        format!("{err}").contains("too large to be used in a query"),
        "{err}"
    );
    let script = format!(
        r#"set("users/{run}/docs/gp2", {{s: "{p}a"}})
col("users/{run}/docs").where("s", "==", "{}x").get().len()"#,
        "x".repeat(1498)
    );
    let Some(fs) = emulator() else {
        eprintln!("skip: FIRESTORE_EMULATOR_HOST is not set");
        return;
    };
    let out = run_script_with_firestore(&script, fs).unwrap();
    assert_eq!(out, Value::Int(0));
}

#[test]
fn emulator_where_validates_nested_filter_string_sizes() {
    // Same equality, membership, and array_contains cases as the in-memory
    // `where_validates_nested_filter_string_sizes` test.
    let valid = "x".repeat(1499);
    let invalid = "x".repeat(1500);
    let run = unique("nested_filter");
    let make_script = |op: &str, value: &str| {
        format!(r#"col("users/{run}/docs").where("payload", "{op}", {value}).get()"#)
    };

    let valid_cases = [
        make_script("==", &format!(r#"["{valid}"]"#)),
        make_script("in", &format!(r#"[["{valid}"]]"#)),
        make_script("array_contains", &format!(r#"["{valid}"]"#)),
    ];
    for script in valid_cases {
        let Some(fs) = emulator() else {
            eprintln!("skip: no emulator");
            return;
        };
        let out = run_script_with_firestore(&script, fs).unwrap();
        assert_eq!(out, Value::Array(vec![]));
    }

    let invalid_cases = [
        make_script("==", &format!(r#"{{s: "{invalid}"}}"#)),
        make_script("in", &format!(r#"[{{s: "{invalid}"}}]"#)),
        make_script("array_contains", &format!(r#"{{s: "{invalid}"}}"#)),
        make_script("==", &format!(r#"{{s: ["{invalid}"]}}"#)),
    ];
    for script in invalid_cases {
        let Some(fs) = emulator() else {
            eprintln!("skip: no emulator");
            return;
        };
        let err = run_script_with_firestore(&script, fs).unwrap_err();
        assert!(
            format!("{err}").contains("value for payload is too large to be used in a query"),
            "{err}"
        );
    }
}

#[test]
fn emulator_update_field_paths_match_in_memory_validation() {
    // Same case as in-memory `update_field_paths_require_simple_field_grammar`.
    // Sentinel (transform) keys are validated too: Firestore's field_transforms
    // accepts quoted paths, so the write is rejected before the RPC to keep the
    // two backends consistent.
    let run = unique("gvv");
    for script in [
        format!(r#"update("users/{run}/docs/d1", {{"`a.b`": 1}})"#),
        format!(r#"update("users/{run}/docs/d1", {{"a..b": 1}})"#),
        format!(r#"update("users/{run}/docs/d1", {{"`a.b`": array_union([1])}})"#),
        format!(r#"update("users/{run}/docs/d1", {{"a..b": array_union([1])}})"#),
        format!(r#"set("users/{run}/docs/d1", {{"`a.b`": array_union([1])}}, merge: true)"#),
        format!(r#"set("users/{run}/docs/d1", {{"a..b": array_union([1])}}, merge: true)"#),
    ] {
        let Some(fs) = emulator() else {
            eprintln!("skip: FIRESTORE_EMULATOR_HOST is not set");
            return;
        };
        let err = run_script_with_firestore(&script, fs).unwrap_err();
        assert!(
            format!("{err}").contains("invalid field path"),
            "{script}: {err}"
        );
    }
}

#[test]
fn emulator_delete_field_and_increment_match_in_memory() {
    // Same cases as in-memory `delete_field_removes_fields_via_update_and_merge_set`
    // and `increment_adds_to_numeric_fields`.
    let Some(fs) = emulator() else {
        eprintln!("skip: FIRESTORE_EMULATOR_HOST is not set");
        return;
    };
    let run = unique("sent");
    let script = format!(
        "set(\"users/{run}\", {{name: \"a\", age: 1, visits: 10}})\nupdate(\"users/{run}\", {{age: delete_field(), visits: increment(5)}})\nget(\"users/{run}\")"
    );
    let out = run_script_with_firestore(&script, fs).unwrap();
    // The expected value omitted `name`, which the script sets, so this could
    // never pass wherever the emulator actually ran - it only looked green
    // because the suite skips without FIRESTORE_EMULATOR_HOST. Both backends
    // remove `age` and keep `name`; this now matches the sibling in-memory test.
    assert_eq!(
        out,
        map! {
            "name" => Value::Str("a".to_string()),
            "visits" => Value::Int(15)
        }
    );
}

#[test]
fn emulator_extended_query_matches_in_memory() {
    // Same cases as the in-memory order_by/where-offset-count tests.
    // Fixtures live in a per-run subcollection so the collection-scoped
    // queries stay repeatable against a long-lived local emulator.
    let Some(fs) = emulator() else {
        eprintln!("skip: FIRESTORE_EMULATOR_HOST is not set");
        return;
    };
    let run = unique("xq");
    let script = format!(
        r#"set("users/{run}/rows/r1", {{ord: 1, age: 20, tags: ["a", "b"]}})
set("users/{run}/rows/r2", {{ord: 2, age: 30, tags: ["b", "c"]}})
set("users/{run}/rows/r3", {{ord: 3, age: 20, tags: ["c"]}})
set("users/{run}/rows/r4", {{ord: 4, age: 40}})
set("users/{run}/rows/r5", {{ord: 5, age: 50, tags: []}})
set("users/{run}/rows/r6", {{other: 1}})
[
  col("users/{run}/rows").order_by("ord", "desc").limit(2).get().map {{ |r| r["ord"] }},
  col("users/{run}/rows").where("age", "!=", 20).get().len(),
  col("users/{run}/rows").where("age", "in", [20, 30]).get().len(),
  col("users/{run}/rows").where("tags", "array_contains", "b").get().len(),
  col("users/{run}/rows").where("tags", "array_contains_any", ["a", "c"]).get().len(),
  col("users/{run}/rows").order_by("ord", "asc").offset(3).get().len(),
  col("users/{run}/rows").where("age", ">=", 20).count(),
]"#
    );
    let out = run_script_with_firestore(&script, fs).unwrap();
    assert_eq!(
        out,
        Value::Array(vec![
            Value::Array(vec![Value::Int(5), Value::Int(4)]),
            Value::Int(3),
            Value::Int(3),
            Value::Int(2),
            Value::Int(3),
            Value::Int(2),
            Value::Int(5),
        ])
    );
}

#[test]
fn emulator_query_encodes_doc_refs_and_ignores_external_references() {
    let Some(fs) = emulator() else {
        eprintln!("skip: no emulator");
        return;
    };
    let project = std::env::var("FIRESTORE_PROJECT_ID").unwrap_or_else(|_| "firelang-test".into());
    let collection = format!("ref_queries_{}", unique("run"));
    let local = reference_value(&project, "(default)", "users/owner");
    let other_project = reference_value("other-project", "(default)", "users/owner");
    let other_database = reference_value(&project, "other", "users/owner");
    let matching_fields = HashMap::from([
        ("owner".to_string(), local.clone()),
        ("members".to_string(), array_value([local.clone()])),
        ("meta".to_string(), map_value([("owner", local.clone())])),
    ]);
    let external_fields = HashMap::from([
        ("owner".to_string(), other_project.clone()),
        ("members".to_string(), array_value([other_database.clone()])),
        (
            "meta".to_string(),
            map_value([("owner", other_database.clone())]),
        ),
        ("other_project".to_string(), other_project),
        ("other_database".to_string(), other_database),
    ]);
    seed_reference_documents(
        &project,
        &collection,
        [("matching", matching_fields), ("external", external_fields)],
    );

    let script = format!(
        r#"owner = doc("users/owner")
[
  col("{collection}").where("owner", "==", owner).get().len(),
  col("{collection}").where("owner", "in", [owner]).get().len(),
  col("{collection}").where("members", "array_contains", owner).get().len(),
  col("{collection}").where("meta", "==", {{owner: owner}}).get().len(),
  col("{collection}").where("other_project", "==", owner).get().len(),
  col("{collection}").where("other_database", "==", owner).get().len(),
]"#
    );
    let out = run_script_with_firestore(&script, fs).unwrap();
    assert_eq!(
        out,
        Value::Array(vec![
            Value::Int(1),
            Value::Int(1),
            Value::Int(1),
            Value::Int(1),
            Value::Int(0),
            Value::Int(0),
        ])
    );
}

fn run_on_shared_runtime(
    rt: &mut firelang::runtime::Runtime<FirestoreEmulator>,
    source: &str,
) -> Result<firelang::Value, Box<dyn std::error::Error>> {
    rt.run(source)
}

#[test]
fn emulator_failed_transaction_aborts_and_restarts() {
    // 失敗した transaction は rollback RPC で中断され、子スコープの local も
    // 残らない。同じ接続で新しい transaction を開始できる
    // ("already in transaction" にならない)。
    let Some(fs) = emulator() else {
        eprintln!("skip: FIRESTORE_EMULATOR_HOST is not set");
        return;
    };
    let mut rt = firelang::runtime::Runtime::new(fs);
    let run = unique("txab");
    let failing = run_on_shared_runtime(
        &mut rt,
        &format!(
            "transaction {{ tx_local = 1\nset(\"users/{run}\", {{x: tx_local}})\nundefined_var }}"
        ),
    );
    assert!(failing.is_err());
    // The comment above claims the failed transaction's local does not
    // survive: verify it directly. A scope-cleanup regression would leave
    // `tx_local` defined here instead of erroring.
    let leaked = run_on_shared_runtime(&mut rt, "tx_local").unwrap_err();
    assert!(
        format!("{leaked}").contains("undefined variable: tx_local"),
        "{leaked}"
    );
    let out = run_on_shared_runtime(
        &mut rt,
        &format!(
            "transaction {{ tx_local = 2\nset(\"users/{run}\", {{x: tx_local}}) }}\nget(\"users/{run}\")"
        ),
    )
    .unwrap();
    assert_eq!(out, map! {"x" => Value::Int(2)});
}
