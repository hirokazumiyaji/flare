//! Integration tests against the Firestore emulator (`FIRESTORE_EMULATOR_HOST`).
//! Locally: start the emulator or skip when unset. CI: see `.github/workflows/ci.yml`
//! (`firebase emulators:exec` sets the host automatically).

#[macro_use]
mod common;

use firelang::run_script_with_firestore;
use firelang::{FirestoreEmulator, Value};

fn emulator() -> Option<FirestoreEmulator> {
    FirestoreEmulator::from_env()
}
fn unique(prefix: &str) -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{prefix}_{n}")
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
fn emulator_merge_set() {
    let Some(fs) = emulator() else {
        eprintln!("skip: no emulator");
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
        eprintln!("skip: no emulator");
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
fn emulator_collection_group_query() {
    let Some(fs) = emulator() else {
        eprintln!("skip: no emulator");
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
        eprintln!("skip: no emulator");
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
fn emulator_server_timestamp_resolves() {
    let Some(fs) = emulator() else {
        eprintln!("skip: no emulator");
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
        eprintln!("skip: no emulator");
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
        eprintln!("skip: no emulator");
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
        eprintln!("skip: no emulator");
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
        eprintln!("skip: no emulator");
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
        eprintln!("skip: no emulator");
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
fn emulator_dotted_field_path_update_matches_in_memory() {
    // Same case as in-memory `update_supports_dotted_field_path_like_firestore`.
    let Some(fs) = emulator() else {
        eprintln!("skip: no emulator");
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
fn emulator_replacement_set_sentinel_with_dotted_key_matches_in_memory() {
    // Same case as in-memory
    // `replacement_set_resolves_sentinels_against_literal_dotted_keys`:
    // a replacement `set` treats dotted keys as literal field names, so the
    // transform must be sent with a backtick-escaped path.
    let Some(fs) = emulator() else {
        eprintln!("skip: no emulator");
        return;
    };
    let run = unique("gv");
    let script = format!(
        r#"set("users/{run}/docs/d1", {{"a.b": array_union([1])}})
set("users/{run}/docs/d2", {{"x.y": ["a"]}})
set("users/{run}/docs/d2", {{"x.y": array_remove(["a"])}})
[get("users/{run}/docs/d1")["a.b"], get("users/{run}/docs/d2")["x.y"]]"#
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
    let run = unique("gp");
    let p = "x".repeat(1500);
    let script = format!(
        r#"set("users/{run}/docs/gp1", {{s: "{p}a"}})
col("users/{run}/docs").where("s", "==", "{p}").get()"#
    );
    let Some(fs) = emulator() else {
        eprintln!("skip: no emulator");
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
        eprintln!("skip: no emulator");
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
            eprintln!("skip: no emulator");
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
        eprintln!("skip: no emulator");
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
        eprintln!("skip: no emulator");
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
        eprintln!("skip: no emulator");
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
    let out = run_on_shared_runtime(
        &mut rt,
        &format!(
            "transaction {{ tx_local = 2\nset(\"users/{run}\", {{x: tx_local}}) }}\nget(\"users/{run}\")"
        ),
    )
    .unwrap();
    assert_eq!(out, map! {"x" => Value::Int(2)});
}
