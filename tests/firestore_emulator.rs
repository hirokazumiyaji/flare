//! Integration tests against the Firestore emulator (`FIRESTORE_EMULATOR_HOST`).
//! Locally: build with `--features emulator`, start the emulator, or skip when
//! the host is unset. A configured but invalid or unreachable emulator is a
//! test failure, not a skip. CI: see `.github/workflows/ci.yml`
//! (`firebase emulators:exec` sets the host automatically).

#[macro_use]
mod common;

use common::{test_collection, test_namespace, unique};
use firelang::run_script_with_firestore;
use firelang::run_script_with_firestore_and_limits;
use firelang::runtime::Runtime;
use firelang::{EmulatorAuth, FirestoreEmulator, RuntimeLimits, Value};
use firestore::{FirestoreCreateSupport, FirestoreDb, FirestoreDbOptions};
use gcloud_sdk::google::firestore::v1::{value, ArrayValue, Document, MapValue, Value as SdkValue};
use std::collections::HashMap;

fn emulator() -> Option<FirestoreEmulator> {
    match FirestoreEmulator::try_from_env() {
        Ok(Some(emulator)) => Some(emulator),
        Ok(None) => None,
        Err(error) => {
            panic!("FIRESTORE_EMULATOR_HOST is configured, but emulator setup failed: {error}")
        }
    }
}

fn assert_runtime_write_error(err: Box<dyn std::error::Error>) {
    use firelang::{ErrorKind, FirelangError};

    let ferr = err
        .downcast_ref::<FirelangError>()
        .expect("write validation should produce a FirelangError");
    assert_eq!(ferr.kind, ErrorKind::Runtime, "{ferr}");
    assert!(
        ferr.message.contains("unsupported value in write"),
        "{ferr}"
    );
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
fn production_config_denies_firestore_writes() {
    if std::env::var("FIRELANG_FIRESTORE_RULES").as_deref() != Ok("deny") {
        return;
    }

    // 既定の `owner` 資格は emulator の admin で security rules を
    // バイパスするため、deny .rules の検証には admin でない資格が要る。
    // (owner のままだと書き込みが成功し、この検査は Always-green に
    // なってしまう。実測で `Ok` になって確認済み。)
    let fs = FirestoreEmulator::from_env_with_auth(EmulatorAuth::EnforceRules)
        .expect("the deny-rules check must run with an emulator");
    let doc_path = format!("production_guard/{}", unique("write"));
    let script = format!("set(doc(\"{doc_path}\"), {{value: \"blocked\"}})");
    let err = run_script_with_firestore(&script, fs).unwrap_err();
    let message = err.to_string();
    // emulator は rules 違反を gRPC の PermissionDenied(英語メッセージ
    // "The caller does not have permission...")として返すので、
    // PERMISSION_DENIED / permission / "does not have permission" を
    // すべて「拒否された」証拠として扱う。
    let lower = message.to_ascii_lowercase();
    assert!(
        message.contains("PERMISSION_DENIED")
            || lower.contains("permission denied")
            || lower.contains("does not have permission"),
        "expected a permission error, got: {message}"
    );
}

#[test]
fn firestore_global_functions_with_emulator() {
    let Some(fs) = emulator() else {
        return;
    };
    let doc_path = format!("{}/smoke", test_collection("global_functions"));
    let script = format!("set(doc(\"{doc_path}\"), {{value: \"ok\"}})\nget(\"{doc_path}\")");
    let out = run_script_with_firestore(&script, fs).unwrap();
    assert_eq!(out, map! {"value" => Value::Str("ok".to_string())});
}

#[test]
fn emulator_document_name_filters_match_relative_paths() {
    let Some(fs) = emulator() else {
        return;
    };
    // Scope fixtures to a per-run subcollection: the emulator is shared
    // across tests, so an unscoped `!=` would also match other tests' docs.
    let id = unique("name_filter");
    let a = format!("users/{id}/docs/a");
    let b = format!("users/{id}/docs/b");
    let col = format!("users/{id}/docs");
    let script = format!(
        r#"set("{a}", {{n: 1}})
set("{b}", {{n: 2}})
[
  col("{col}").where("__name__", "==", "{a}").get().map {{ |r| r["__name__"] }},
  col("{col}").where("__name__", "!=", "{a}").get().map {{ |r| r["__name__"] }},
  col("{col}").where("__name__", "in", ["{a}", "{b}"]).get().map {{ |r| r["__name__"] }},
  col("{col}").where("__name__", ">", "{a}").get()
]"#
    );
    let out = run_script_with_firestore(&script, fs).unwrap();
    let Value::Array(rows) = out else {
        panic!("expected array");
    };
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0], Value::Array(vec![Value::Str(a.clone())]));
    assert_eq!(rows[1], Value::Array(vec![Value::Str(b.clone())]));
    assert_eq!(rows[2], Value::Array(vec![Value::Str(a), Value::Str(b)]));
    assert_eq!(rows[3], Value::Array(vec![]));
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
    let collection = test_collection("query_where_and_limit");
    let script = format!(
        r#"set("{collection}/z", {{age: 10}})
set("{collection}/a", {{age: 30}})
set("{collection}/m", {{age: 25}})
col("{collection}").where("age", ">=", 20).limit(2).get()"#
    );
    let out = run_script_with_firestore(&script, fs).unwrap();
    let Value::Array(rows) = out else {
        panic!("expected array");
    };
    assert_eq!(rows.len(), 2);
    for (row, suffix) in rows.iter().zip(["a", "m"]) {
        let Value::Map(m) = row else {
            panic!("expected map row");
        };
        assert_eq!(
            m.get("__name__"),
            Some(&Value::Str(format!("{collection}/{suffix}")))
        );
    }
}

#[test]
fn emulator_query_field_paths_match_in_memory() {
    // Simple, implicitly quoted, explicitly quoted, nested, and __name__ paths
    // use the same field-path contract in both backends.
    let Some(fs) = emulator() else {
        eprintln!("skip: no emulator");
        return;
    };
    let run = unique("qfp");
    let one = format!("users/{run}/rows/one");
    let two = format!("users/{run}/rows/two");
    let script = format!(
        r#"set("{one}", {{"a-b": 1, "a.b": 2, profile: {{age: 3}}}})
set("{two}", {{"a-b": 9, "a.b": 8, profile: {{age: 1}}}})
[
  col("users/{run}/rows").where("a-b", "==", 1).get()[0]["__name__"],
  col("users/{run}/rows").where("`a.b`", "==", 2).get()[0]["__name__"],
  col("users/{run}/rows").where("profile.age", ">=", 3).get()[0]["__name__"],
  col("users/{run}/rows").where("__name__", "==", "{one}").get().len(),
  col("users/{run}/rows").where("__name__", "==", doc("{two}")).get().len(),
  col("users/{run}/rows").order_by("a-b", "asc").get()[0]["__name__"]
]"#,
    );
    let out = run_script_with_firestore(&script, fs).unwrap();
    // `order_by("a-b", "asc")` は `one`(a-b: 1)が `two`(a-b: 9)より前。
    // in-memory の parity テストと同じ並びを期待する。
    assert_eq!(
        out,
        Value::Array(vec![
            Value::Str(one.clone()),
            Value::Str(one.clone()),
            Value::Str(one.clone()),
            Value::Int(1),
            Value::Int(1),
            Value::Str(one),
        ])
    );
}

#[test]
fn emulator_rejects_invalid_query_field_paths_before_rpc() {
    let run = unique("qfp_invalid");
    for field in ["", "a.", ".a", "a..b", "`unterminated"] {
        let Some(fs) = emulator() else {
            eprintln!("skip: no emulator");
            return;
        };
        let script = format!(
            r#"col("users/{run}/rows").where("{}", "==", 1).get()"#,
            field
        );
        let err = run_script_with_firestore(&script, fs).unwrap_err();
        assert!(
            format!("{err}").contains("invalid field path"),
            "{field}: {err}"
        );
    }
}

#[test]
fn emulator_query_result_budget_uses_the_bounded_stream_path() {
    let Some(fs) = emulator() else {
        eprintln!("skip: no emulator");
        return;
    };
    let run = unique("budget");
    let script = format!(
        r#"set("users/{run}/rows/a", {{kind: "match"}})
set("users/{run}/rows/b", {{kind: "match"}})
col("users/{run}/rows").where("kind", "==", "match").get()"#
    );
    let limits = RuntimeLimits::default().with_max_query_results(1);
    let err = run_script_with_firestore_and_limits(&script, fs, limits).unwrap_err();
    assert!(
        format!("{err}").contains("query result limit exceeded (max 1)"),
        "{err}"
    );
}

#[test]
fn emulator_collection_group_query() {
    let Some(fs) = emulator() else {
        eprintln!("skip: FIRESTORE_EMULATOR_HOST is not set");
        return;
    };
    let namespace = test_namespace("collection_group_query");
    let group = unique("members");
    let path_x = format!("{namespace}/teams/t1/{group}/x");
    let path_y = format!("{namespace}/teams/t2/{group}/y");
    let script = format!(
        r#"set("{path_x}", {{active: true}})
set("{path_y}", {{active: true}})
col_group("{group}").where("active", "==", true).get()"#
    );
    let out = run_script_with_firestore(&script, fs).unwrap();
    let Value::Array(rows) = out else {
        panic!("expected array");
    };
    let mut names: Vec<String> = rows
        .iter()
        .map(|row| {
            let Value::Map(row) = row else {
                panic!("expected map row");
            };
            let Some(Value::Str(name)) = row.get("__name__") else {
                panic!("expected document name");
            };
            name.clone()
        })
        .collect();
    names.sort();
    let mut expected = vec![path_x, path_y];
    expected.sort();
    assert_eq!(names, expected);
}

#[test]
fn emulator_nested_map_roundtrip() {
    let Some(fs) = emulator() else {
        eprintln!("skip: FIRESTORE_EMULATOR_HOST is not set");
        return;
    };
    let path = format!("{}/nested", test_collection("nested_map_roundtrip"));
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
fn emulator_server_timestamp_is_shared_within_batch_and_transaction_commits() {
    let Some(fs) = emulator() else {
        eprintln!("skip: no emulator");
        return;
    };
    // ドキュメント path はセグメント数が偶数でなければならないため、run ごとの
    // subcollection(`users/{run}/docs/...`)へ fixture を置く。
    let collection = format!("users/{}/docs", unique("stcommit"));
    let script = format!(
        r#"
batch {{
  set("{collection}/batch_1", {{a: server_timestamp(), b: server_timestamp()}})
  set("{collection}/batch_2", {{c: server_timestamp()}})
}}
transaction {{
  set("{collection}/tx_1", {{a: server_timestamp(), b: server_timestamp()}})
  set("{collection}/tx_2", {{c: server_timestamp()}})
}}
[
  get("{collection}/batch_1")["a"],
  get("{collection}/batch_1")["b"],
  get("{collection}/batch_2")["c"],
  get("{collection}/tx_1")["a"],
  get("{collection}/tx_1")["b"],
  get("{collection}/tx_2")["c"],
]
"#
    );
    let out = run_script_with_firestore(&script, fs).unwrap();
    let Value::Array(times) = out else {
        panic!("expected timestamp list, got {out:?}");
    };
    assert_eq!(times.len(), 6);
    assert!(matches!(times[0], Value::Timestamp(_)));
    assert!(times[0..3].iter().all(|value| value == &times[0]));
    assert!(times[3..].iter().all(|value| value == &times[3]));
}

#[test]
fn emulator_batch_atomic_commit() {
    let Some(fs) = emulator() else {
        eprintln!("skip: FIRESTORE_EMULATOR_HOST is not set");
        return;
    };
    let collection = test_collection("batch_atomic_commit");
    let path_a = format!("{collection}/a");
    let path_b = format!("{collection}/b");
    let script = format!(
        r#"batch {{
  set("{path_a}", {{n: 1}})
  set("{path_b}", {{n: 2}})
}}
get("{path_b}")"#
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
    let path = format!("{}/transaction", test_collection("transaction_read_write"));
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
    let collection = test_collection("transaction_multiple_gets");
    let path_a = format!("{collection}/a");
    let path_b = format!("{collection}/b");
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
    let path = format!("{}/missing", test_collection("update_missing"));
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
    let collection = test_collection("where_compare");
    let script = format!(
        r#"set("{collection}/1", {{name: "alice", created: timestamp(100), active: true}})
set("{collection}/2", {{name: "bob", created: timestamp(200), active: false}})
[
  col("{collection}").where("name", ">", "a").get().len(),
  col("{collection}").where("created", ">", timestamp(150)).get().len(),
  col("{collection}").where("active", ">", false).get().len(),
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
    let path = format!("{}/dotted", test_collection("dotted_field_path_update"));
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
    let collection = test_collection("replacement_set_dotted_sentinel");
    let script = format!(
        r#"set("{collection}/names", {{"a-b": 1, "1start_digit": 2, "a`b": 3, "a\\b": 4}})
set("{collection}/names", {{"a-b": increment(1), "1start_digit": increment(2), "a`b": increment(3), "a\\b": increment(4)}})
set("{collection}/dotted", {{"a.b": [1]}})
set("{collection}/dotted", {{"a.b": array_union([2])}})
set("{collection}/remove", {{"x.y": ["a"]}})
set("{collection}/remove", {{"x.y": array_remove(["a"])}})
[
  get("{collection}/names")["a-b"],
  get("{collection}/names")["1start_digit"],
  get("{collection}/names")["a`b"],
  get("{collection}/names")["a\\b"],
  get("{collection}/dotted")["a.b"],
  get("{collection}/remove")["x.y"],
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
    let collection = test_collection("large_string_filter");
    let p = "x".repeat(1500);
    let script = format!(
        r#"set("{collection}/gp1", {{s: "{p}a"}})
col("{collection}").where("s", "==", "{p}").get()"#
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
        r#"set("{collection}/gp2", {{s: "{p}a"}})
col("{collection}").where("s", "==", "{}x").get().len()"#,
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
    let path = format!("{}/d1", test_collection("field_path_validation"));
    for script in [
        format!(r#"update("{path}", {{"`a.b`": 1}})"#),
        format!(r#"update("{path}", {{"a..b": 1}})"#),
        format!(r#"update("{path}", {{"`a.b`": array_union([1])}})"#),
        format!(r#"update("{path}", {{"a..b": array_union([1])}})"#),
        format!(r#"set("{path}", {{"`a.b`": array_union([1])}}, merge: true)"#),
        format!(r#"set("{path}", {{"a..b": array_union([1])}}, merge: true)"#),
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
    let path = format!("{}/sentinel", test_collection("field_sentinels"));
    let script = format!(
        "set(\"{path}\", {{name: \"a\", age: 1, visits: 10}})\nupdate(\"{path}\", {{age: delete_field(), visits: increment(5)}})\nget(\"{path}\")"
    );
    let out = run_script_with_firestore(&script, fs).unwrap();
    // `age` だけが削除されるため `name` は残る(in-memory と同じ結果)。期待値に
    // `name` が無かった版は、このテストが実際に emulator を通ったことが
    // なかったためだけ緑に見えていた。
    // `age` だけが削除されるため `name` は残る(in-memory と同じ結果)。期待値に
    // `name` が無かった版は、このテストが実際に emulator を通ったことが
    // なかったためだけ緑に見えていた。
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
    let collection = test_collection("extended_query");
    let script = format!(
        r#"set("{collection}/r1", {{ord: 1, age: 20, tags: ["a", "b"]}})
set("{collection}/r2", {{ord: 2, age: 30, tags: ["b", "c"]}})
set("{collection}/r3", {{ord: 3, age: 20, tags: ["c"]}})
set("{collection}/r4", {{ord: 4, age: 40}})
set("{collection}/r5", {{ord: 5, age: 50, tags: []}})
set("{collection}/r6", {{other: 1}})
[
  col("{collection}").order_by("ord", "desc").limit(2).get().map {{ |r| r["ord"] }},
  col("{collection}").where("age", "!=", 20).get().len(),
  col("{collection}").where("age", "in", [20, 30]).get().len(),
  col("{collection}").where("tags", "array_contains", "b").get().len(),
  col("{collection}").where("tags", "array_contains_any", ["a", "c"]).get().len(),
  col("{collection}").order_by("ord", "asc").offset(3).get().len(),
  col("{collection}").where("age", ">=", 20).count(),
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
fn emulator_count_and_path_page_handle_large_payloads() {
    let Some(fs) = emulator() else {
        eprintln!("skip: no emulator");
        return;
    };
    let run = unique("payload");
    let payload = "x".repeat(32 * 1024);
    let script = format!(
        r#"set("users/{run}/rows/z", {{kind: "match", payload: "{payload}"}})
set("users/{run}/rows/a", {{kind: "match", payload: "{payload}"}})
set("users/{run}/rows/m", {{kind: "match", payload: "{payload}"}})
set("users/{run}/rows/other", {{kind: "skip", payload: "{payload}"}})
[
  col("users/{run}/rows").where("kind", "==", "match").order_by("payload", "asc").offset(99).limit(1).count(),
  col("users/{run}/rows").where("kind", "==", "match").offset(1).limit(1).get().map {{ |r| [r["__name__"], len(r["payload"])] }},
]"#
    );
    let out = run_script_with_firestore(&script, fs).unwrap();
    // `map` は行ごとにブロックの値を 1 つ返すため、2 番目の要素は行の配列。
    assert_eq!(
        out,
        Value::Array(vec![
            Value::Int(3),
            Value::Array(vec![Value::Array(vec![
                Value::Str(format!("users/{run}/rows/m")),
                Value::Int((32 * 1024) as i64),
            ])]),
        ])
    );
}

#[test]
fn emulator_membership_null_and_empty_array_queries_match_in_memory() {
    // This is the parity matrix for the filters that must not be delegated
    // wholesale to Firestore.  In particular, null is a value in firelang and
    // an array may contain null elements.
    let Some(fs) = emulator() else {
        eprintln!("skip: no emulator");
        return;
    };
    let run = unique("qparity");
    let script = format!(
        r#"set("users/{run}/rows/n1", {{v: 1, tags: [1, nil]}})
set("users/{run}/rows/n2", {{v: nil, tags: [nil]}})
set("users/{run}/rows/n3", {{v: "1", tags: []}})
set("users/{run}/rows/n4", {{tags: [2]}})
set("users/{run}/rows/n5", {{v: 2, tags: [1, 2]}})
[
  col("users/{run}/rows").where("v", "!=", nil).get().len(),
  col("users/{run}/rows").where("v", "!=", 1).get().len(),
  col("users/{run}/rows").where("v", "not-in", [1]).get().len(),
  col("users/{run}/rows").where("v", "in", [nil]).get().len(),
  col("users/{run}/rows").where("v", "not-in", []).get().len(),
  col("users/{run}/rows").where("v", "not-in", [1, nil]).get().len(),
  col("users/{run}/rows").where("tags", "array_contains", nil).get().len(),
  col("users/{run}/rows").where("tags", "array_contains_any", [nil]).get().len(),
  col("users/{run}/rows").where("tags", "array_contains_any", []).get().len(),
  col("users/{run}/rows").where("v", "in", []).get().len(),
  col("users/{run}/rows").where("v", "!=", 1).where("v", "!=", 2).get().len(),
  col("users/{run}/rows").where("v", "in", [1, 2]).where("v", "not-in", [2]).get().len(),
  col("users/{run}/rows").where("tags", "array_contains_any", [1]).where("tags", "array_contains_any", [2]).get().len(),
]"#
    );
    let out = run_script_with_firestore(&script, fs).unwrap();
    assert_eq!(
        out,
        Value::Array(vec![
            Value::Int(3),
            Value::Int(3),
            Value::Int(3),
            Value::Int(1),
            Value::Int(4),
            Value::Int(0),
            Value::Int(2),
            Value::Int(2),
            Value::Int(0),
            Value::Int(0),
            Value::Int(2),
            Value::Int(1),
            Value::Int(1),
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
    let path = format!("{}/transaction", test_collection("failed_transaction"));
    let failing = run_on_shared_runtime(
        &mut rt,
        &format!("transaction {{ tx_local = 1\nset(\"{path}\", {{x: tx_local}})\nundefined_var }}"),
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
            "transaction {{ tx_local = 2\nset(\"{path}\", {{x: tx_local}}) }}\nget(\"{path}\")"
        ),
    )
    .unwrap();
    assert_eq!(out, map! {"x" => Value::Int(2)});
}

#[test]
fn emulator_set_rejects_unsupported_values_recursively() {
    let Some(fs) = emulator() else {
        eprintln!("skip: no emulator");
        return;
    };
    let mut rt = Runtime::new(fs);
    let run = unique("write_set");
    let cases = [
        format!("fn f() {{ 1 }}\nset(\"users/{run}/docs/func\", {{value: f}})"),
        format!("set(\"users/{run}/docs/collection\", {{value: col(\"users\")}})"),
        format!("set(\"users/{run}/docs/group\", {{value: col_group(\"members\")}})"),
        format!(
            "set(\"users/{run}/docs/query\", {{value: col(\"users\").where(\"x\", \"==\", 1)}})"
        ),
        format!("set(\"users/{run}/docs/nested\", {{value: {{nested: [col(\"users\")]}}}})"),
    ];
    for source in cases {
        let err = rt.run(&source).unwrap_err();
        assert_runtime_write_error(err);
    }

    // `doc()` は runtime では許可される(参照は保存可能な値なので)。emulator の
    // write codec だけが拒否し、そのメッセージは backend エラーになる(spec §7)。
    let err = rt
        .run(&format!(
            "set(\"users/{run}/docs/doc\", {{value: doc(\"users/ref\")}})"
        ))
        .unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("unsupported value in emulator write"),
        "{message}"
    );
    assert_eq!(
        rt.run(&format!("get(\"users/{run}/docs/doc\")")).unwrap(),
        Value::Nil
    );
}

#[test]
fn emulator_update_rejects_unsupported_values_before_staging() {
    let Some(fs) = emulator() else {
        eprintln!("skip: no emulator");
        return;
    };
    let mut rt = Runtime::new(fs);
    let run = unique("write_update");
    let path = format!("users/{run}");
    rt.run(&format!("set(\"{path}\", {{value: 1}})")).unwrap();
    let err = rt
        .run(&format!("fn f() {{ 1 }}\nupdate(\"{path}\", {{value: f}})"))
        .unwrap_err();
    assert_runtime_write_error(err);
    assert_eq!(
        rt.run(&format!("get(\"{path}\")")).unwrap(),
        map! {"value" => Value::Int(1)}
    );
}

#[test]
fn emulator_batch_with_unsupported_value_persists_nothing() {
    let Some(fs) = emulator() else {
        eprintln!("skip: no emulator");
        return;
    };
    let mut rt = Runtime::new(fs);
    let run = unique("write_batch");
    let good = format!("users/{run}/docs/good");
    let bad = format!("users/{run}/docs/bad");
    let err = rt
        .run(&format!(
            "batch {{ set(\"{good}\", {{ok: true}})\nset(\"{bad}\", {{value: col(\"users\")}}) }}"
        ))
        .unwrap_err();
    assert_runtime_write_error(err);
    assert_eq!(rt.run(&format!("get(\"{good}\")")).unwrap(), Value::Nil);
    assert_eq!(rt.run(&format!("get(\"{bad}\")")).unwrap(), Value::Nil);
}

#[test]
fn emulator_documented_values_and_direct_transforms_remain_writable() {
    let Some(fs) = emulator() else {
        eprintln!("skip: no emulator");
        return;
    };
    let mut rt = Runtime::new(fs);
    let path = format!("users/{}/docs/allowed", unique("write_allowed"));
    let source = format!(
        "set(\"{path}\", {{int: 1, str: \"s\", bool: true, \"nil\": nil, array: [1, {{nested: timestamp(2)}}], map: {{ok: false}}, ts: timestamp(3), tags: array_union([\"a\"]), items: [\"x\"], remove_me: true, count: 10}})\nupdate(\"{path}\", {{tags: array_union([\"b\"]), items: array_remove([\"x\"]), remove_me: delete_field(), count: increment(2), updated_at: server_timestamp()}})\nget(\"{path}\")"
    );
    let out = rt.run(&source).unwrap();
    let Value::Map(fields) = out else {
        panic!("expected document map");
    };
    assert_eq!(fields.get("int"), Some(&Value::Int(1)));
    assert_eq!(fields.get("str"), Some(&Value::Str("s".to_string())));
    assert_eq!(fields.get("bool"), Some(&Value::Bool(true)));
    assert_eq!(fields.get("nil"), Some(&Value::Nil));
    assert!(matches!(fields.get("ts"), Some(Value::Timestamp(_))));
    assert_eq!(
        fields.get("tags"),
        Some(&Value::Array(vec![
            Value::Str("a".to_string()),
            Value::Str("b".to_string())
        ]))
    );
    assert_eq!(fields.get("items"), Some(&Value::Array(vec![])));
    assert!(!fields.contains_key("remove_me"));
    assert_eq!(fields.get("count"), Some(&Value::Int(12)));
    assert!(matches!(
        fields.get("updated_at"),
        Some(Value::Timestamp(_))
    ));
}
