//! Integration tests against the Firestore emulator (`FIRESTORE_EMULATOR_HOST`).
//! Locally: start the emulator or skip when unset. CI: see `.github/workflows/ci.yml`
//! (`firebase emulators:exec` sets the host automatically).

#[macro_use]
mod common;

use common::{test_collection, test_namespace, unique};
use firelang::run_script_with_firestore;
use firelang::{FirestoreEmulator, Value};

fn emulator() -> Option<FirestoreEmulator> {
    let Some(host) = std::env::var("FIRESTORE_EMULATOR_HOST").ok() else {
        eprintln!("skip: FIRESTORE_EMULATOR_HOST is not set");
        return None;
    };
    let project_id =
        std::env::var("FIRESTORE_PROJECT_ID").unwrap_or_else(|_| "firelang-test".to_string());
    Some(
        FirestoreEmulator::new(host, project_id).unwrap_or_else(|e| {
            panic!("FIRESTORE_EMULATOR_HOST is set, but connecting to the emulator failed: {e}")
        }),
    )
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
fn emulator_merge_set() {
    let Some(fs) = emulator() else {
        return;
    };
    let path = format!("{}/merge", test_collection("merge_set"));
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
fn emulator_collection_group_query() {
    let Some(fs) = emulator() else {
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
fn emulator_server_timestamp_resolves() {
    let Some(fs) = emulator() else {
        return;
    };
    let path = format!("{}/timestamp", test_collection("server_timestamp"));
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
fn emulator_dotted_field_path_update_matches_in_memory() {
    // Same case as in-memory `update_supports_dotted_field_path_like_firestore`.
    let Some(fs) = emulator() else {
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
fn emulator_replacement_set_sentinel_with_dotted_key_matches_in_memory() {
    // Same case as in-memory
    // `replacement_set_resolves_sentinels_against_literal_dotted_keys`:
    // a replacement `set` treats dotted keys as literal field names, so the
    // transform must be sent with a backtick-escaped path.
    let Some(fs) = emulator() else {
        return;
    };
    let collection = test_collection("replacement_set_dotted_sentinel");
    let script = format!(
        r#"set("{collection}/d1", {{"a.b": array_union([1])}})
set("{collection}/d2", {{"x.y": ["a"]}})
set("{collection}/d2", {{"x.y": array_remove(["a"])}})
[get("{collection}/d1")["a.b"], get("{collection}/d2")["x.y"]]"#
    );
    let out = run_script_with_firestore(&script, fs).unwrap();
    assert_eq!(
        out,
        Value::Array(vec![
            Value::Array(vec![Value::Int(1)]),
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
        return;
    };
    let out = run_script_with_firestore(&script, fs).unwrap();
    assert_eq!(out, Value::Int(0));
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
        return;
    };
    let path = format!("{}/sentinel", test_collection("field_sentinels"));
    let script = format!(
        "set(\"{path}\", {{name: \"a\", age: 1, visits: 10}})\nupdate(\"{path}\", {{age: delete_field(), visits: increment(5)}})\nget(\"{path}\")"
    );
    let out = run_script_with_firestore(&script, fs).unwrap();
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

fn run_on_shared_runtime(
    rt: &mut firelang::runtime::Runtime<FirestoreEmulator>,
    source: &str,
) -> Result<firelang::Value, Box<dyn std::error::Error>> {
    rt.run(source)
}

#[test]
fn emulator_failed_transaction_aborts_and_restarts() {
    // 失敗した transaction は rollback RPC で中断され、同じ接続で
    // 新しい transaction を開始できる("already in transaction" にならない)。
    let Some(fs) = emulator() else {
        return;
    };
    let mut rt = firelang::runtime::Runtime::new(fs);
    let path = format!("{}/transaction", test_collection("failed_transaction"));
    let failing = run_on_shared_runtime(
        &mut rt,
        &format!("transaction {{ set(\"{path}\", {{x: 1}})\nundefined_var }}"),
    );
    assert!(failing.is_err());
    let out = run_on_shared_runtime(
        &mut rt,
        &format!("transaction {{ set(\"{path}\", {{x: 2}}) }}\nget(\"{path}\")"),
    )
    .unwrap();
    assert_eq!(out, map! {"x" => Value::Int(2)});
}
