//! Integration tests against the Firestore emulator (`FIRESTORE_EMULATOR_HOST`).
//! Locally: start the emulator or skip when unset. CI: see `.github/workflows/ci.yml`
//! (`firebase emulators:exec` sets the host automatically).

use firelang::run_script_with_firestore;
use firelang::FirestoreEmulator;

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
    assert!(format!("{out:?}").contains("ok"));
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
    let s = format!("{out:?}");
    assert!(s.contains("Int(2)"), "{s}");
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
collection("users").where("age", ">=", 20).limit(2).get()"#
    );
    let out = run_script_with_firestore(&script, fs).unwrap();
    let s = format!("{out:?}");
    assert!(s.contains(&format!("{id}_a")), "lexicographic order: {s}");
    assert!(s.contains(&format!("{id}_m")), "{s}");
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
collection_group("members").where("active", "==", true).get()"#
    );
    let out = run_script_with_firestore(&script, fs).unwrap();
    let s = format!("{out:?}");
    assert!(
        s.contains(&format!("members/{id}_x")) || s.contains(&format!("{id}_x")),
        "{s}"
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
    assert!(format!("{out:?}").contains("Int(42)"));
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
    assert!(format!("{out:?}").contains("Timestamp"));
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
    assert!(format!("{out:?}").contains("Int(2)"));
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
    let s = format!("{out:?}");
    assert!(s.contains("after") && s.contains("Int(2)"), "{s}");
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
    assert!(format!("{out:?}").contains("sum"));
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
