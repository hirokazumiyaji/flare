//! In-memory backend behavior tests (language + Firestore API).

use firelang::run_script;
use firelang::runtime::Runtime;
use firelang::{InMemoryFirestore, Value};

fn run_in(
    runtime: &mut Runtime<InMemoryFirestore>,
    source: &str,
) -> Result<Value, Box<dyn std::error::Error>> {
    let tokens = firelang::lexer::lex(source)?;
    let program = firelang::parser::Parser::new(tokens).parse_program()?;
    runtime.eval_program(&program)
}

#[test]
fn evals_arithmetic() {
    let out = run_script("a = 1 + 2\na + 3").unwrap();
    assert_eq!(out, Value::Int(6));
}

#[test]
fn division_by_zero_returns_error() {
    let err = run_script("1 / 0").unwrap_err();
    assert!(format!("{err}").contains("division by zero"));
}

#[test]
fn unterminated_string_literal_errors() {
    let err = firelang::lexer::lex("\"foo").unwrap_err();
    assert!(format!("{err}").contains("unterminated"));
}

#[test]
fn evals_def_and_call() {
    let out = run_script("fn add(a,b) { return a + b }\nadd(2,3)").unwrap();
    assert_eq!(out, Value::Int(5));
}

#[test]
fn set_requires_map_and_get_returns_map() {
    let out = run_script("set(doc(\"users/u1\"), {name: \"alice\"})\nget(\"users/u1\")").unwrap();
    assert_eq!(format!("{out:?}"), "Map({\"name\": Str(\"alice\")})");
}

#[test]
fn supports_timestamp_and_query() {
    let out = run_script(
        "set(doc(\"users/1\"), {age: 20, created: timestamp(100)})\nset(doc(\"users/2\"), {age: 30, created: now()})\ncol(\"users\").where(\"age\", \">=\", 21).limit(1).get()",
    )
    .unwrap();
    match out {
        Value::Array(arr) => assert_eq!(arr.len(), 1),
        _ => panic!("expected array"),
    }
}

#[test]
fn supports_array_union_remove_and_update() {
    let out = run_script(
        "set(\"users/au\", {tags: [\"a\", \"b\"]})\nupdate(\"users/au\", {tags: array_union([\"b\", \"c\"])})\nupdate(\"users/au\", {tags: array_remove([\"a\"])})\nget(\"users/au\")",
    )
    .unwrap();
    assert!(format!("{out:?}").contains("\"c\""));
}

#[test]
fn supports_array_literal() {
    let out = run_script("[1, 2, \"\"]").unwrap();
    assert_eq!(
        out,
        Value::Array(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Str("".to_string())
        ])
    );
}

#[test]
fn supports_server_timestamp_sentinel() {
    let out = run_script("set(\"users/st\", {updated_at: server_timestamp()})\nget(\"users/st\")")
        .unwrap();
    assert!(format!("{out:?}").contains("Timestamp"));
}

#[test]
fn parses_timestamp_from_rfc3339_string() {
    let out = run_script("timestamp(\"2026-02-01T00:00:00+09:00\").to_unix()").unwrap();
    assert_eq!(out, Value::Int(1769871600));
}

#[test]
fn supports_keyword_arguments_like_ruby() {
    let out = run_script(
        "fn add(a,b) { a + b }\nv = add(a: 10, b: 20)\nset(\"users/kw\", {count: 1})\nset(\"users/kw\", {count: 2}, merge: true)\nv",
    )
    .unwrap();
    assert_eq!(out, Value::Int(30));
}

#[test]
fn supports_transaction_and_batch_brace_blocks() {
    let out = run_script(
        "transaction { set(\"users/t1\", {n: 1}) }\nbatch { set(\"users/t2\", {n: 2}) }\nget(\"users/t2\")",
    )
    .unwrap();
    assert!(format!("{out:?}").contains("\"n\": Int(2)"));
}

#[test]
fn batch_rolls_back_on_runtime_error() {
    let mut runtime = Runtime::new(InMemoryFirestore::default());
    let failing = run_in(
        &mut runtime,
        "batch { set(\"users/batch_rb\", {x: 1})\nundefined_var }",
    );
    assert!(failing.is_err());
    let out = run_in(&mut runtime, "get(\"users/batch_rb\")").unwrap();
    assert_eq!(out, Value::Nil);
}

#[test]
fn transaction_rejects_get_after_write() {
    let err = run_script("transaction { set(\"users/txa\", {n: 1})\nget(\"users/txa\") }");
    assert!(err.is_err());
    assert!(format!("{}", err.unwrap_err()).contains("get is not allowed"));
}

#[test]
fn query_limit_is_deterministic_by_path() {
    let out = run_script(
        "set(\"users/qz\", {ord: 1})\nset(\"users/qa\", {ord: 2})\nset(\"users/qm\", {ord: 3})\ncol(\"users\").where(\"ord\", \">=\", 1).limit(1).get()",
    )
    .unwrap();
    let s = format!("{out:?}");
    assert!(
        s.contains("qa"),
        "expected lexicographically first doc users/qa, got {s}"
    );
}

#[test]
fn supports_ruby_style_string_interpolation() {
    let out = run_script("name = \"fire\"\n\"hello #{name} #{1 + 2}\"").unwrap();
    assert_eq!(out, Value::Str("hello fire 3".to_string()));
}

#[test]
fn disallows_variable_reassignment() {
    let err = run_script("a = 1\na = 2").unwrap_err();
    assert!(format!("{err}").contains("reassignment is not allowed"));
}

#[test]
fn methods_return_immutable_copies() {
    let out =
        run_script("t1 = timestamp(100)\nt2 = t1.add_seconds(10)\n[t1.to_unix(), t2.to_unix()]")
            .unwrap();
    assert_eq!(out, Value::Array(vec![Value::Int(100), Value::Int(110)]));
}

#[test]
fn batch_rejects_get() {
    let err = run_script("batch { get(\"users/x\") }").unwrap_err();
    assert!(format!("{err}").contains("not allowed inside batch"));
}

#[test]
fn batch_with_failing_update_persists_nothing() {
    let mut runtime = Runtime::new(InMemoryFirestore::default());
    let failing = run_in(
        &mut runtime,
        "batch { set(\"users/b1\", {x: 1})\nupdate(\"users/missing\", {y: 2}) }",
    );
    assert!(failing.is_err());
    assert_eq!(
        run_in(&mut runtime, "get(\"users/b1\")").unwrap(),
        Value::Nil
    );
}

#[test]
fn nested_batch_is_rejected() {
    let err = run_script("batch { batch { set(\"users/n\", {x: 1}) } }");
    assert!(err.is_err());
}

#[test]
fn nested_transaction_is_rejected() {
    let err = run_script("transaction { transaction { set(\"users/n\", {x: 1}) } }");
    assert!(err.is_err());
}

#[test]
fn transaction_get_reads_snapshot_and_commit_applies_writes() {
    let mut runtime = Runtime::new(InMemoryFirestore::default());
    run_in(&mut runtime, "set(\"users/s1\", {n: 1})").unwrap();
    let out = run_in(
        &mut runtime,
        "transaction { v = get(\"users/s1\")\nset(\"users/s1\", {n: 2})\nv }",
    )
    .unwrap();
    assert!(format!("{out:?}").contains("Int(1)"), "{out:?}");
    let after = run_in(&mut runtime, "get(\"users/s1\")").unwrap();
    assert!(format!("{after:?}").contains("Int(2)"), "{after:?}");
}

#[test]
fn transaction_rolls_back_on_runtime_error() {
    let mut runtime = Runtime::new(InMemoryFirestore::default());
    let failing = run_in(
        &mut runtime,
        "transaction { set(\"users/tx_rb\", {x: 1})\nundefined_var }",
    );
    assert!(failing.is_err());
    assert_eq!(
        run_in(&mut runtime, "get(\"users/tx_rb\")").unwrap(),
        Value::Nil
    );
}

#[test]
fn query_get_is_rejected_inside_batch_and_transaction() {
    let err = run_script("batch { col(\"users\").get() }").unwrap_err();
    assert!(format!("{err}").contains("not allowed inside batch"));
    let err = run_script("transaction { col(\"users\").get() }").unwrap_err();
    assert!(format!("{err}").contains("not allowed inside transaction"));
}

#[test]
fn update_missing_document_fails_in_memory() {
    let err = run_script("update(\"users/nope\", {x: 1})").unwrap_err();
    assert!(format!("{err}").contains("document not found"));
}

#[test]
fn collection_limit_builds_query_inside_transaction() {
    // クエリの「構築」は batch/transaction 内でも許される。禁止されるのは .get() 実行のみ。
    let out = run_script(
        "q = col(\"users\").limit(1)\ntransaction { q2 = col(\"users\").limit(1)\nset(\"users/l1\", {n: 1}) }\nq",
    );
    assert!(out.is_ok(), "{out:?}");
}

#[test]
fn where_rejects_unsupported_operator() {
    let err = run_script("col(\"users\").where(\"a\", \"!=\", 1)").unwrap_err();
    assert!(format!("{err}").contains("unsupported query op"), "{err}");
}

#[test]
fn escaped_interpolation_stays_literal() {
    let out = run_script("\"a \\#{1} b\"").unwrap();
    assert_eq!(out, Value::Str("a #{1} b".to_string()));
}

#[test]
fn interpolation_in_map_key_is_rejected() {
    let err = run_script("{\"k#{1}\": 1}").unwrap_err();
    assert!(
        format!("{err}").contains("interpolation is not allowed in map keys"),
        "{err}"
    );
}

#[test]
fn unterminated_interpolation_fails_at_lex_time() {
    let err = firelang::lexer::lex("\"#{1 + 2\"").unwrap_err();
    assert!(format!("{err}").contains("unterminated"), "{err}");
}

#[test]
fn supports_map_and_array_index_access() {
    let out = run_script(
        "m = {name: \"alice\", nested: {a: [1, 2]}}\n[m[\"name\"], m[\"nested\"][\"a\"][1]]",
    )
    .unwrap();
    assert_eq!(
        out,
        Value::Array(vec![Value::Str("alice".to_string()), Value::Int(2)])
    );
}

#[test]
fn map_dot_access_returns_key_value() {
    let out = run_script("m = {name: \"alice\"}\nm.name").unwrap();
    assert_eq!(out, Value::Str("alice".to_string()));
}

#[test]
fn missing_keys_and_out_of_range_indices_return_nil() {
    let out =
        run_script("m = {name: \"alice\"}\na = [1]\n[m[\"missing\"], m.missing, a[5], a[0 - 1]]")
            .unwrap();
    assert_eq!(
        out,
        Value::Array(vec![Value::Nil, Value::Nil, Value::Nil, Value::Nil])
    );
}

#[test]
fn index_type_mismatch_is_an_error() {
    let err = run_script("[1, 2][\"a\"]").unwrap_err();
    assert!(
        format!("{err}").contains("array index must be an int"),
        "{err}"
    );
    let err = run_script("{a: 1}[1]").unwrap_err();
    assert!(
        format!("{err}").contains("map index must be a string"),
        "{err}"
    );
    let err = run_script("1[0]").unwrap_err();
    assert!(
        format!("{err}").contains("unsupported index receiver"),
        "{err}"
    );
}

#[test]
fn query_rows_support_index_access_including_dunder_name() {
    let out = run_script(
        "set(\"users/ix1\", {n: 1})\nset(\"users/ix2\", {n: 2})\nrows = col(\"users\").get()\n[rows[0][\"__name__\"], rows[1][\"n\"]]",
    )
    .unwrap();
    assert_eq!(
        out,
        Value::Array(vec![Value::Str("users/ix1".to_string()), Value::Int(2),])
    );
}

#[test]
fn index_access_read_value_for_branching() {
    let out = run_script(
        "set(\"users/br\", {active: true})\nu = get(\"users/br\")\nif u[\"active\"] { 1 } else { 2 }",
    )
    .unwrap();
    assert_eq!(out, Value::Int(1));
}

#[test]
fn method_calls_in_conditions_do_not_consume_control_flow_braces() {
    // Map の m.key 形式・関数呼び出し・素の変数いずれも if / while の条件式に置ける。
    let out = run_script("m = {active: true}\nif m.active { 1 } else { 2 }").unwrap();
    assert_eq!(out, Value::Int(1));
    let out = run_script("m = {count: 0}\nwhile m.count > 0 { 9 }").unwrap();
    assert_eq!(out, Value::Nil);
    let out = run_script("fn f() { true }\nif f() { 1 } else { 2 }").unwrap();
    assert_eq!(out, Value::Int(1));
    let out = run_script("x = true\nif x { 5 } else { 6 }").unwrap();
    assert_eq!(out, Value::Int(5));
    let out = run_script("m = {n: 2}\nif m[\"n\"] >= 2 { 1 } else { 2 }").unwrap();
    assert_eq!(out, Value::Int(1));
}

#[test]
fn block_calls_still_work_outside_conditions() {
    let out = run_script("batch { set(\"users/bb\", {n: 1}) }\nget(\"users/bb\")[\"n\"]").unwrap();
    assert_eq!(out, Value::Int(1));
}
