//! In-memory backend behavior tests (language + Firestore API).

#[macro_use]
mod common;

use firelang::run_script;
use firelang::runtime::Runtime;
use firelang::{InMemoryFirestore, Value};

/// 同じ `Runtime` で複数スクリプトを続けて評価する(状態保持のテスト用)。
fn run_in(
    runtime: &mut Runtime<InMemoryFirestore>,
    source: &str,
) -> Result<Value, Box<dyn std::error::Error>> {
    runtime.run(source)
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
    assert_eq!(out, map! {"name" => Value::Str("alice".to_string())});
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
    assert_eq!(
        out,
        map! {"tags" => Value::Array(vec![
            Value::Str("b".to_string()),
            Value::Str("c".to_string()),
        ])}
    );
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
    let Value::Map(m) = out else {
        panic!("expected map, got {out:?}");
    };
    assert!(matches!(m.get("updated_at"), Some(Value::Timestamp(_))));
}

#[test]
fn parses_timestamp_from_rfc3339_string() {
    let out = run_script("timestamp(\"2026-02-01T00:00:00+09:00\").to_unix()").unwrap();
    assert_eq!(out, Value::Int(1769871600));
    let out = run_script("timestamp(\"2026-02-01t00:00:00Z\").to_unix()").unwrap();
    assert_eq!(out, Value::Int(1769904000));
    let out = run_script("timestamp(\"2026-02-01t00:00:00z\").to_unix()").unwrap();
    assert_eq!(out, Value::Int(1769904000));
    for value in [
        "2026-02-01T00:00:00.000Z",
        "2026-02-01T00:00:00.999Z",
        "2026-02-01t00:00:00.123z",
    ] {
        let err = run_script(&format!("timestamp(\"{value}\")")).unwrap_err();
        assert!(format!("{err}").contains("fractional seconds are not supported"));
    }
}

#[test]
fn bytes_constructor_comparison_and_interpolation_contract() {
    let out = run_script(
        r#"
a = bytes("fire")
b = bytes([102, 105, 114, 101])
[
  a == b,
  a != bytes("flame"),
  bytes([0, 255]) < bytes([1, 0]),
  bytes([1, 0]) <= bytes([1, 0]),
  bytes([2]) > bytes([1, 255]),
  "payload=#{bytes([0, 255])}",
]
"#,
    )
    .unwrap();
    assert_eq!(
        out,
        Value::Array(vec![
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(true),
            Value::Str("payload=Bytes(00ff)".to_string()),
        ])
    );

    let values = (0u8..40)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let out = run_script(&format!("bytes([{values}])")).unwrap();
    let prefix = (0u8..32).map(|n| format!("{n:02x}")).collect::<String>();
    assert_eq!(out.inspect(), format!("Bytes({prefix}, 8 more bytes)"));
    assert!(out.inspect().len() < 128);

    for source in [
        "bytes()",
        "bytes([0, 256])",
        "bytes([-1])",
        "bytes([\"ff\"])",
        "bytes(\"a\", \"b\")",
        "bytes(value: 1)",
    ] {
        assert!(run_script(source).is_err(), "{source}");
    }
}

#[test]
fn bytes_roundtrip_in_maps_arrays_and_queries() {
    let out = run_script(
        r#"
set("files/a", {
  data: bytes([0, 255]),
  parts: [bytes("hi"), bytes([128])],
  meta: {checksum: bytes([1, 2])},
})
set("files/b", {data: bytes([1, 0])})
set("files/c", {data: bytes([0, 255, 1])})
[
  get("files/a"),
  col("files").where("data", "==", bytes([0, 255])).get().map { |row| row["__name__"] },
  col("files").where("data", ">", bytes([0, 255])).get().map { |row| row["__name__"] },
  col("files").order_by("data", "asc").get().map { |row| row["__name__"] },
]
"#,
    )
    .unwrap();
    let expected = Value::Array(vec![
        Value::Map(
            [
                ("data".to_string(), Value::Bytes(vec![0, 255])),
                (
                    "parts".to_string(),
                    Value::Array(vec![Value::Bytes(b"hi".to_vec()), Value::Bytes(vec![128])]),
                ),
                (
                    "meta".to_string(),
                    Value::Map([("checksum".to_string(), Value::Bytes(vec![1, 2]))].into()),
                ),
            ]
            .into(),
        ),
        Value::Array(vec![Value::Str("files/a".to_string())]),
        Value::Array(vec![
            Value::Str("files/b".to_string()),
            Value::Str("files/c".to_string()),
        ]),
        Value::Array(vec![
            Value::Str("files/a".to_string()),
            Value::Str("files/c".to_string()),
            Value::Str("files/b".to_string()),
        ]),
    ]);
    assert_eq!(out, expected);
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
    assert_eq!(out, map! {"n" => Value::Int(2)});
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
    let Value::Array(rows) = out else {
        panic!("expected array");
    };
    assert_eq!(rows.len(), 1);
    let Value::Map(row) = &rows[0] else {
        panic!("expected map row");
    };
    assert_eq!(
        row.get("__name__"),
        Some(&Value::Str("users/qa".to_string()))
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
    assert_eq!(out, map! {"n" => Value::Int(1)});
    let after = run_in(&mut runtime, "get(\"users/s1\")").unwrap();
    assert_eq!(after, map! {"n" => Value::Int(2)});
}

#[test]
fn transaction_can_restart_after_abort_clears_state() {
    // 失敗した transaction の後は abort で状態が戻り、新しい transaction を
    // 開始できる(abort 前の write は残らない)。
    let mut runtime = Runtime::new(InMemoryFirestore::default());
    let failing = run_in(
        &mut runtime,
        "transaction { set(\"users/tx_re\", {x: 1})\nundefined_var }",
    );
    assert!(failing.is_err());
    let out = run_in(
        &mut runtime,
        "transaction { set(\"users/tx_re\", {x: 2}) }\nget(\"users/tx_re\")",
    )
    .unwrap();
    assert_eq!(out, map! {"x" => Value::Int(2)});
}

#[test]
fn transaction_can_restart_after_commit_failure_clears_state() {
    let mut runtime = Runtime::new(InMemoryFirestore::default());
    let failing = run_in(
        &mut runtime,
        "transaction { update(\"users/nonexistent\", {x: 1}) }",
    );
    assert!(failing.is_err());
    let out = run_in(
        &mut runtime,
        "transaction { set(\"users/tx_commit_err\", {x: 2}) }\nget(\"users/tx_commit_err\")",
    )
    .unwrap();
    assert!(format!("{out:?}").contains("Int(2)"), "{out:?}");
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
    let err = run_script("col(\"users\").where(\"a\", \"=~\", 1)").unwrap_err();
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

#[test]
fn each_iterates_query_rows_and_updates_each_document() {
    let out = run_script(
        "set(\"users/it1\", {n: 1, seen: false})\nset(\"users/it2\", {n: 2, seen: false})\nrows = col(\"users\").get()\nrows.each { |row| update(row[\"__name__\"], {seen: true}) }\n[get(\"users/it1\")[\"seen\"], get(\"users/it2\")[\"seen\"]]",
    )
    .unwrap();
    assert_eq!(
        out,
        Value::Array(vec![Value::Bool(true), Value::Bool(true)])
    );
}

#[test]
fn each_returns_nil_and_map_returns_transformed_array() {
    let out = run_script("[1, 2, 3].map { |x| x * 2 }").unwrap();
    assert_eq!(
        out,
        Value::Array(vec![Value::Int(2), Value::Int(4), Value::Int(6)])
    );
    let out = run_script("a = [1]\na.each { |x| x }").unwrap();
    assert_eq!(out, Value::Nil);
}

#[test]
fn map_extracts_fields_from_query_rows() {
    let out = run_script(
        "set(\"users/mp1\", {n: 1})\nset(\"users/mp2\", {n: 2})\ncol(\"users\").get().map { |row| row[\"n\"] }",
    )
    .unwrap();
    assert_eq!(out, Value::Array(vec![Value::Int(1), Value::Int(2)]));
}

#[test]
fn len_supports_array_map_and_string() {
    let out = run_script("[len([1, 2, 3]), len({a: 1, b: 2}), len(\"hello\"), [1].len()]").unwrap();
    assert_eq!(
        out,
        Value::Array(vec![
            Value::Int(3),
            Value::Int(2),
            Value::Int(5),
            Value::Int(1)
        ])
    );
}

#[test]
fn block_param_must_match_yielded_argument_count() {
    let err = run_script("[1].each { |a, b| a }").unwrap_err();
    assert!(
        format!("{err}").contains("block expects 2 argument(s), got 1"),
        "{err}"
    );
}

#[test]
fn each_and_map_require_block() {
    let err = run_script("[1].each()").unwrap_err();
    assert!(
        format!("{err}").contains("each requires { ... } block"),
        "{err}"
    );
    let err = run_script("[1].map()").unwrap_err();
    assert!(
        format!("{err}").contains("map requires { ... } block"),
        "{err}"
    );
}

#[test]
fn block_params_shadow_outer_variables_like_function_params() {
    let out = run_script("x = 1\n[2].map { |x| x * 10 }").unwrap();
    assert_eq!(out, Value::Array(vec![Value::Int(20)]));
    let out = run_script("[1, 2].each { || 0 }").unwrap_err();
    assert!(
        format!("{out}").contains("block expects 0 argument(s), got 1"),
        "{out}"
    );
}

#[test]
fn block_params_may_start_on_the_next_line() {
    let out = run_script("[1, 2, 3].map {\n|x| x * 2\n}").unwrap();
    assert_eq!(
        out,
        Value::Array(vec![Value::Int(2), Value::Int(4), Value::Int(6)])
    );
    let out = run_script("[1].each {\n  |n|\n  n\n}").unwrap();
    assert_eq!(out, Value::Nil);
}

#[test]
fn block_arity_is_checked_even_for_empty_arrays() {
    let err = run_script("[].each { |a, b| a }").unwrap_err();
    assert!(
        format!("{err}").contains("block expects 2 argument(s), got 1"),
        "{err}"
    );
    let err = run_script("[].map { |a, b| a }").unwrap_err();
    assert!(
        format!("{err}").contains("block expects 2 argument(s), got 1"),
        "{err}"
    );
    let err = run_script("[].each { || 0 }").unwrap_err();
    assert!(
        format!("{err}").contains("block expects 0 argument(s), got 1"),
        "{err}"
    );
}

#[test]
fn array_methods_reject_positional_arguments() {
    let err = run_script("[1].map(99) { |x| x }").unwrap_err();
    assert!(
        format!("{err}").contains("does not take arguments"),
        "{err}"
    );
    let err = run_script("[1].len(99)").unwrap_err();
    assert!(
        format!("{err}").contains("does not take arguments"),
        "{err}"
    );
    let err = run_script("[1].each(99) { |x| x }").unwrap_err();
    assert!(
        format!("{err}").contains("does not take arguments"),
        "{err}"
    );
}

#[test]
fn multiline_map_literal_parses() {
    let out = run_script(
        "set(\"users/ml\", {\n  name: \"a\",\n  age: 20,\n})\nget(\"users/ml\")[\"age\"]",
    )
    .unwrap();
    assert_eq!(out, Value::Int(20));
}

#[test]
fn multiline_array_literal_and_args_parse() {
    let out =
        run_script("fn add(a, b) {\n  return a + b\n}\n[\n  1,\n  add(\n    2,\n    3,\n  ),\n]")
            .unwrap();
    assert_eq!(out, Value::Array(vec![Value::Int(1), Value::Int(5)]));
}

#[test]
fn trailing_commas_are_allowed_in_literals_and_args() {
    let out = run_script("m = {a: 1,}\na = [1, 2,]\n[m[\"a\"], a[1]]").unwrap();
    assert_eq!(out, Value::Array(vec![Value::Int(1), Value::Int(2)]));
    let out = run_script("fn f(a, b,) { a + b }\nf(1, 2,)").unwrap();
    assert_eq!(out, Value::Int(3));
}

#[test]
fn multiline_nested_literals_parse() {
    let out = run_script(
        "rows = [\n  {\n    name: \"a\",\n    tags: [\"x\", \"y\"],\n  },\n]\nrows[0][\"tags\"][1]",
    )
    .unwrap();
    assert_eq!(out, Value::Str("y".to_string()));
}

#[test]
fn newlines_still_separate_statements_outside_brackets() {
    let out = run_script("a = 1\nb = a + 1\nb").unwrap();
    assert_eq!(out, Value::Int(2));
}

#[test]
fn newlines_inside_contained_expressions_are_ignored() {
    // delimiter の内側なら式の途中の改行も無視される。
    let out = run_script("[\n1 +\n2\n]").unwrap();
    assert_eq!(out, Value::Array(vec![Value::Int(3)]));
    let out = run_script("{a: 1 +\n2}[\"a\"]").unwrap();
    assert_eq!(out, Value::Int(3));
    let out = run_script("fn add(a, b) { a + b }\nadd(1 +\n0, 2)").unwrap();
    assert_eq!(out, Value::Int(3));
    let out = run_script("(1 +\n2)").unwrap();
    assert_eq!(out, Value::Int(3));
    let out = run_script("m = [1, 2]\nm[1 +\n0]").unwrap();
    assert_eq!(out, Value::Int(2));
    let out = run_script("(1 +\n -2)").unwrap();
    assert_eq!(out, Value::Int(-1));
    let out = run_script("[1 +\n -0]").unwrap();
    assert_eq!(out, Value::Array(vec![Value::Int(1)]));
    let out = run_script("[\n !false]").unwrap();
    assert_eq!(out, Value::Array(vec![Value::Bool(true)]));
}

#[test]
fn functions_do_not_see_caller_locals() {
    let err = run_script("fn f() { x }\nfn g() { x = 1\n  f() }\ng()").unwrap_err();
    assert!(format!("{err}").contains("undefined variable: x"), "{err}");
}

#[test]
fn nested_call_does_not_leak_callee_locals() {
    let err = run_script("fn g() { a }\nfn f() { a = 42\n  g() }\nf()").unwrap_err();
    assert!(format!("{err}").contains("undefined variable: a"), "{err}");
}

#[test]
fn function_local_may_collide_with_caller_local() {
    let out = run_script("fn f() { x = 9\n  x }\nfn g() { x = 1\n  [f(), x] }\ng()").unwrap();
    assert_eq!(out, Value::Array(vec![Value::Int(9), Value::Int(1)]));
}

#[test]
fn functions_read_globals_but_cannot_assign_them() {
    let out = run_script("x = 5\nfn f() { x }\nf()").unwrap();
    assert_eq!(out, Value::Int(5));
    let err = run_script("x = 5\nfn f() { x = 6 }\nf()").unwrap_err();
    assert!(
        format!("{err}").contains("reassignment is not allowed: x"),
        "{err}"
    );
    let err = run_script("fn f() { x = 6\n  x }\nx = 5\nf()").unwrap_err();
    assert!(
        format!("{err}").contains("reassignment is not allowed: x"),
        "{err}"
    );
}

#[test]
fn function_params_may_shadow_globals() {
    let out = run_script("n = 1\nfn f(n) { n }\nf(2)").unwrap();
    assert_eq!(out, Value::Int(2));
}

#[test]
fn semicolons_are_not_treated_as_newlines_inside_delimiters() {
    // `;` は文の区切りのまま。delimiter の内側ではエラーになる。
    assert!(run_script("[;1]").is_err());
    assert!(run_script("{;}").is_err());
    assert!(run_script("fn f(a) { a }\nf(;)").is_err());
    assert!(run_script("m = [1]\nm[;0]").is_err());
}

#[test]
fn semicolons_remain_statement_separators() {
    let out = run_script("a = 1; b = a + 1; b").unwrap();
    assert_eq!(out, Value::Int(2));
    let out = run_script("fn f() { a = 1; a }\nf()").unwrap();
    assert_eq!(out, Value::Int(1));
    let out = run_script("fn f() { return; 1 }\nf()").unwrap();
    assert_eq!(out, Value::Nil);
}

#[test]
fn recursive_functions_still_work() {
    let out =
        run_script("fn fact(n) { if n <= 1 { 1 } else { n * fact(n - 1) } }\nfact(5)").unwrap();
    assert_eq!(out, Value::Int(120));
}

#[test]
fn integer_overflow_is_an_error_not_a_panic() {
    let err = run_script("9223372036854775807 + 1").unwrap_err();
    assert!(format!("{err}").contains("integer overflow"), "{err}");
    let err = run_script("0 - 9223372036854775807 - 2").unwrap_err();
    assert!(format!("{err}").contains("integer overflow"), "{err}");
    let err = run_script("9223372036854775807 * 2").unwrap_err();
    assert!(format!("{err}").contains("integer overflow"), "{err}");
    let err = run_script("(0 - 9223372036854775807 - 1) / (0 - 1)").unwrap_err();
    assert!(format!("{err}").contains("integer overflow"), "{err}");
}

#[test]
fn out_of_range_integer_literal_is_an_error() {
    let err = run_script("99999999999999999999").unwrap_err();
    assert!(
        format!("{err}").contains("integer literal out of range"),
        "{err}"
    );
}

#[test]
fn negative_limit_is_rejected() {
    let err = run_script("col(\"users\").limit(0 - 1).get()").unwrap_err();
    assert!(format!("{err}").contains("limit must be >= 0"), "{err}");
    let out = run_script("set(\"users/lm\", {n: 1})\ncol(\"users\").limit(1).get()").unwrap();
    assert!(matches!(out, Value::Array(_)));
}

#[test]
fn invalid_document_paths_are_rejected() {
    let cases = [
        "set(\"users\", {a: 1})",
        "get(\"users/1/posts\")",
        "set(\"users//1\", {a: 1})",
        "set(\"/users/1\", {a: 1})",
        "set(\"users/1/\", {a: 1})",
        "doc(\"users\")",
        "update(\"users\", {a: 1})",
        "delete(\"teams\")",
        "get(\"teams\")",
    ];
    for src in cases {
        let err = run_script(src).unwrap_err();
        assert!(
            format!("{err}").contains("invalid document path")
                || format!("{err}").contains("invalid path:"),
            "{src}: {err}"
        );
    }
}

#[test]
fn invalid_collection_paths_are_rejected() {
    let cases = [
        ("col(\"users/1\")", "invalid collection path"),
        ("col(\"\")", "invalid path:"),
        ("col(\"users//members\")", "invalid path: users//members"),
        ("col(\"/users\")", "invalid path:"),
        ("col_group(\"a/b\")", "invalid collection group id"),
        ("col_group(\"\")", "invalid collection group id"),
        ("col(\"users\").doc(\"a/b\")", "invalid document id"),
    ];
    for (src, expected) in cases {
        let err = run_script(src).unwrap_err();
        assert!(format!("{err}").contains(expected), "{src}: {err}");
    }
}

#[test]
fn valid_subcollection_paths_still_work() {
    let out = run_script(
        "set(\"teams/t1/members/m1\", {n: 1})\ncol(\"teams/t1/members\").get()[0][\"__name__\"]",
    )
    .unwrap();
    assert_eq!(out, Value::Str("teams/t1/members/m1".to_string()));
    let out = run_script("col(\"users\").doc(\"1\")").unwrap();
    assert!(matches!(out, Value::DocRef(_)));
}

#[test]
fn where_comparison_supports_str_timestamp_and_bool() {
    // emulator テスト(emulator_where_compare_matches_in_memory_semantics)と
    // 同じケース。両バックエンドで同じ結果を返すことを契約とする。
    let out = run_script(
        "set(\"users/cmp1\", {name: \"alice\", created: timestamp(100), active: true})\nset(\"users/cmp2\", {name: \"bob\", created: timestamp(200), active: false})\n[\n  col(\"users\").where(\"name\", \">\", \"a\").get().len(),\n  col(\"users\").where(\"created\", \">\", timestamp(150)).get().len(),\n  col(\"users\").where(\"active\", \">\", false).get().len(),\n  col(\"users\").where(\"name\", \">\", 5).get().len(),\n]",
    )
    .unwrap();
    assert_eq!(
        out,
        Value::Array(vec![
            Value::Int(2),
            Value::Int(1),
            Value::Int(1),
            Value::Int(0)
        ])
    );
}

#[test]
fn update_supports_dotted_field_paths_like_firestore() {
    // emulator テスト(emulator_dotted_field_path_update_matches_in_memory)と
    // 同じケース。{"profile.name": ...} はネストしたフィールドを更新する。
    let out = run_script(
        "set(\"users/fp1\", {profile: {name: \"a\", age: 1}, n: 5})\nupdate(\"users/fp1\", {\"profile.name\": \"b\"})\nset(\"users/fp1\", {\"profile.age\": 2}, merge: true)\nget(\"users/fp1\")",
    )
    .unwrap();
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
fn dotted_update_creates_missing_intermediate_maps() {
    let out = run_script(
        "set(\"users/fp2\", {n: 1})\nupdate(\"users/fp2\", {\"outer.inner\": 42})\nget(\"users/fp2\")[\"outer\"][\"inner\"]",
    )
    .unwrap();
    assert_eq!(out, Value::Int(42));
}

#[test]
fn dotted_update_fails_when_intermediate_is_not_a_map() {
    let err = run_script(
        "set(\"users/fp3\", {profile: 1})\nupdate(\"users/fp3\", {\"profile.name\": \"b\"})",
    )
    .unwrap_err();
    assert!(format!("{err}").contains("not a map"), "{err}");
}

#[test]
fn set_replace_keeps_dotted_keys_literal() {
    let out = run_script("set(\"users/fp4\", {\"a.b\": 1})\nget(\"users/fp4\")[\"a.b\"]").unwrap();
    assert_eq!(out, Value::Int(1));
}

#[test]
fn dotted_update_supports_nested_array_union() {
    let out = run_script(
        "set(\"users/fp5\", {profile: {tags: [\"a\"]}})\nupdate(\"users/fp5\", {\"profile.tags\": array_union([\"b\"])})\nget(\"users/fp5\")[\"profile\"][\"tags\"]",
    )
    .unwrap();
    assert_eq!(
        out,
        Value::Array(vec![
            Value::Str("a".to_string()),
            Value::Str("b".to_string())
        ])
    );
}

#[test]
fn update_field_paths_require_simple_field_grammar() {
    // Firestore の単純フィールドパス([A-Za-z_][A-Za-z0-9_]*)のみ。
    // バッククォートはエスケープとして解釈されないので、in-memory / emulator
    // ともに検証エラーになる。センチネル値(transform)のキーも同じ検証を
    // 受ける(emulator は Firestore が quoted transform パスを受け付けるため
    // RPC 前に検証して挙動を揃える。emulator テスト
    // emulator_update_field_paths_match_in_memory_validation と同じケース)。
    let cases = [
        "set(\"users/gv1\", {n: 1})\nupdate(\"users/gv1\", {\"`a.b`\": 1})",
        "set(\"users/gv2\", {n: 1})\nupdate(\"users/gv2\", {\"a..b\": 1})",
        "set(\"users/gv3\", {n: 1})\nupdate(\"users/gv3\", {\"a b\": 1})",
        "set(\"users/gv4\", {n: 1})\nupdate(\"users/gv4\", {\"日本語\": 1})",
        "set(\"users/gv5\", {\"a..b\": 1}, merge: true)",
        "set(\"users/gv6\", {n: 1})\nupdate(\"users/gv6\", {\"`a.b`\": array_union([1])})",
        "set(\"users/gv7\", {n: 1})\nset(\"users/gv7\", {\"`a.b`\": array_union([1])}, merge: true)",
        "set(\"users/gv8\", {n: 1})\nupdate(\"users/gv8\", {\"a..b\": array_union([1])})",
        "set(\"users/gv9\", {n: 1})\nset(\"users/gv9\", {\"a..b\": array_union([1])}, merge: true)",
    ];
    for src in cases {
        let err = run_script(src).unwrap_err();
        assert!(
            format!("{err}").contains("invalid field path"),
            "{src}: {err}"
        );
    }
}

#[test]
fn replacement_set_resolves_sentinels_against_literal_dotted_keys() {
    // emulator テスト(emulator_replacement_set_sentinel_with_dotted_key)と
    // 同じケース。置換 set ではドット付きキーはリテラルのフィールド名。
    let out =
        run_script("set(\"users/gv2\", {\"a.b\": array_union([1])})\nget(\"users/gv2\")[\"a.b\"]")
            .unwrap();
    assert_eq!(out, Value::Array(vec![Value::Int(1)]));
    let out = run_script(
        "set(\"users/gv3\", {\"x.y\": [\"a\"]})\nset(\"users/gv3\", {\"x.y\": array_remove([\"a\"])})\nget(\"users/gv3\")[\"x.y\"]",
    )
    .unwrap();
    assert_eq!(out, Value::Array(vec![]));
}

#[test]
fn where_rejects_large_string_filter_values() {
    // Firestore はクエリのフィルタ文字列値を 1,500 バイト未満に制限する
    // (1,500 バイト以上はクエリ時にエラー。保存値に制限はない)。実機
    // emulator の Firestore は 1,500 バイトのフィルタ値を
    // `value for s is too large to be used in a query` で却下するため、
    // in-memory も同じメッセージでエラーにする(1499 バイトのフィルタは
    // 両バックエンドで評価できる)。emulator テスト
    // emulator_where_rejects_large_string_filter_values と同じケース。
    let p = "x".repeat(1500);
    let err = run_script(&format!(
        "set(\"users/gp1\", {{s: \"{p}a\"}})\ncol(\"users\").where(\"s\", \"==\", \"{p}\").get()"
    ))
    .unwrap_err();
    assert!(
        format!("{err}").contains("too large to be used in a query"),
        "{err}"
    );
    let out = run_script(&format!(
        "set(\"users/gp2\", {{s: \"{p}a\"}})\ncol(\"users\").where(\"s\", \"==\", \"{}x\").get().len()",
        "x".repeat(1498)
    ))
    .unwrap();
    assert_eq!(out, Value::Int(0));
}

#[test]
fn logical_operators_return_operand_with_short_circuit() {
    // `!` は Bool に正規化、`&&` / `||` は Ruby と同じくオペランドそのものを返す。
    let out = run_script("[!true, !false, !nil, !0, !\"\", !!\"x\"]").unwrap();
    assert_eq!(
        out,
        Value::Array(vec![
            Value::Bool(false),
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(false),
            Value::Bool(false),
            Value::Bool(true),
        ])
    );
    let out =
        run_script("[1 && 2, nil && 2, false && 99, 1 || 2, nil || 2, false || nil]").unwrap();
    assert_eq!(
        out,
        Value::Array(vec![
            Value::Int(2),
            Value::Nil,
            Value::Bool(false),
            Value::Int(1),
            Value::Int(2),
            Value::Nil,
        ])
    );
    // 優先順位: `||` < `&&` < 比較 < `!`。
    let out = run_script("true || false && false").unwrap();
    assert_eq!(out, Value::Bool(true));
    let out = run_script("1 + 1 == 2 && !false").unwrap();
    assert_eq!(out, Value::Bool(true));
    let out = run_script("if 20 >= 20 && true { 1 } else { 2 }").unwrap();
    assert_eq!(out, Value::Int(1));
    // 短絡評価: 右辺が評価されなければエラーにならない。
    let out = run_script("false && undefined_var").unwrap();
    assert_eq!(out, Value::Bool(false));
    let out = run_script("true || undefined_var").unwrap();
    assert_eq!(out, Value::Bool(true));
    // 単体の `&` / `|` はエラー。
    assert!(run_script("true & false").is_err());
    assert!(run_script("true | false").is_err());
}

#[test]
fn unary_minus_negates_ints_with_highest_precedence() {
    let out = run_script("[-5, -(1 + 2), --5, - -5, 0 - 5]").unwrap();
    assert_eq!(
        out,
        Value::Array(vec![
            Value::Int(-5),
            Value::Int(-3),
            Value::Int(5),
            Value::Int(5),
            Value::Int(-5),
        ])
    );
    // 単項マイナスは `*` `/` より強く、`-1 * 2` は `(-1) * 2`。
    let out = run_script("[-1 * 2, -(2 * 3), -2 * -3]").unwrap();
    assert_eq!(
        out,
        Value::Array(vec![Value::Int(-2), Value::Int(-6), Value::Int(6)])
    );
    let out = run_script("timestamp(-86400).to_unix()").unwrap();
    assert_eq!(out, Value::Int(-86400));
    let out = run_script("if -1 < 0 { 1 } else { 2 }").unwrap();
    assert_eq!(out, Value::Int(1));
    // `!` との組み合わせ(右結合)。
    let out = run_script("!-5").unwrap();
    assert_eq!(out, Value::Bool(false));
    let err = run_script("-!false").unwrap_err();
    assert!(
        format!("{err}").contains("unsupported unary - operand"),
        "{err}"
    );
    // 非 Int への単項マイナスはエラー。
    let err = run_script("-\"a\"").unwrap_err();
    assert!(
        format!("{err}").contains("unsupported unary - operand"),
        "{err}"
    );
    let err = run_script("-true").unwrap_err();
    assert!(
        format!("{err}").contains("unsupported unary - operand"),
        "{err}"
    );
}

#[test]
fn else_if_chain_evaluates_first_matching_branch() {
    let src = "if x > 10 { 1 } else if x > 3 { 2 } else { 3 }";
    let out = run_script(&format!("x = 11\n{src}")).unwrap();
    assert_eq!(out, Value::Int(1));
    let out = run_script(&format!("x = 5\n{src}")).unwrap();
    assert_eq!(out, Value::Int(2));
    let out = run_script(&format!("x = 1\n{src}")).unwrap();
    assert_eq!(out, Value::Int(3));
    // 最後の else 省略可。
    let out = run_script("x = 1\nif x > 10 { 1 } else if x > 0 { 2 }").unwrap();
    assert_eq!(out, Value::Int(2));
    let out = run_script("x = -1\nif x > 10 { 1 } else if x > 0 { 2 }").unwrap();
    assert_eq!(out, Value::Nil);
    // 3 段以上も可。従来の else { if ... } ネストと同値。
    let out =
        run_script("x = 2\nif x == 1 { 1 } else if x == 2 { 2 } else if x == 3 { 3 } else { 9 }")
            .unwrap();
    assert_eq!(out, Value::Int(2));
    let out = run_script("x = 2\nif x == 1 { 1 } else { if x == 2 { 2 } else { 9 } }").unwrap();
    assert_eq!(out, Value::Int(2));
}

#[test]
fn errors_carry_kind_and_line_col() {
    use firelang::{ErrorKind, FirelangError, Span};

    // lex エラー: 行・桁付き。
    let err = firelang::lexer::lex("a = 1\nb = 2 &").unwrap_err();
    let ferr = err
        .downcast_ref::<FirelangError>()
        .expect("lex error should be FirelangError");
    assert_eq!(ferr.kind, ErrorKind::Lex);
    assert_eq!(ferr.span, Some(Span::new(2, 7)));
    assert!(format!("{ferr}").contains("unexpected char"), "{ferr}");

    // parse エラー: 行・桁付き。
    let err = run_script("a = 1\nb = }").unwrap_err();
    let ferr = err
        .downcast_ref::<FirelangError>()
        .expect("parse error should be FirelangError");
    assert_eq!(ferr.kind, ErrorKind::Parse);
    assert_eq!(ferr.span, Some(Span::new(2, 5)));
    assert!(format!("{ferr}").starts_with("2:5: "), "{ferr}");

    // runtime エラー: 失敗したトップレベル文の開始位置。
    let err = run_script("a = 1\nb = 2\nc = undefined_var").unwrap_err();
    let ferr = err
        .downcast_ref::<FirelangError>()
        .expect("runtime error should be FirelangError");
    assert_eq!(ferr.kind, ErrorKind::Runtime);
    assert_eq!(ferr.span, Some(Span::new(3, 1)));
    assert!(format!("{ferr}").contains("undefined variable"), "{ferr}");

    // backend エラー(in-memory の document not found)は Backend 種別。
    let err = run_script("update(\"users/nope\", {x: 1})").unwrap_err();
    let ferr = err
        .downcast_ref::<FirelangError>()
        .expect("backend error should be FirelangError");
    assert_eq!(ferr.kind, ErrorKind::Backend);
    assert_eq!(ferr.span, Some(Span::new(1, 1)));

    // 文字列補間内のエラー位置: 式の先頭からの相対位置が正しく反映される
    let err = run_script("\"#{@}\"").unwrap_err();
    let ferr = err
        .downcast_ref::<FirelangError>()
        .expect("lex error in interpolation should be FirelangError");
    assert_eq!(ferr.span, Some(Span::new(1, 4)));

    let err = run_script("\"#{1 + }\"").unwrap_err();
    let ferr = err
        .downcast_ref::<FirelangError>()
        .expect("parse error in interpolation should be FirelangError");
    assert_eq!(ferr.span, Some(Span::new(1, 8)));

    // マップキーでの文字列補間禁止エラーの位置: 拒否されたキートークン自体の位置を指す
    let err = run_script("{\"#{x}\": 1}").unwrap_err();
    let ferr = err
        .downcast_ref::<FirelangError>()
        .expect("parse error in map key should be FirelangError");
    assert_eq!(ferr.span, Some(Span::new(1, 2)));

    // 補間内の単一式制約違反: 仕様(docs/spec.md §10)通り位置が付かない (None)
    let err = run_script("\"#{1; 2}\"").unwrap_err();
    let ferr = err
        .downcast_ref::<FirelangError>()
        .expect("parse error in interpolation should be FirelangError");
    assert_eq!(ferr.span, None);
    assert_eq!(
        format!("{ferr}"),
        "interpolation must contain a single expression"
    );
    let err = run_script("if true { |x| x }").unwrap_err();
    let ferr = err
        .downcast_ref::<FirelangError>()
        .expect("block parameter error should be FirelangError");
    assert_eq!(ferr.span, Some(Span::new(1, 11)));
}

#[test]
fn interpolation_handles_braces_inside_nested_strings() {
    // トークン単位で `#{...}` を切り出すため、中の文字列リテラル内の
    // `{` `}` に惑わされない(旧実装の文字単位カウントでは壊れたケース)。
    let out = run_script("\"#{\"}\" + \"}\"}\"").unwrap();
    assert_eq!(out, Value::Str("}}".to_string()));
    let out = run_script("\"#{{a: 1}[\"a\"]}\"").unwrap();
    assert_eq!(out, Value::Str("1".to_string()));
    let out = run_script("\"#{\"a{b}c\"}\"").unwrap();
    assert_eq!(out, Value::Str("a{b}c".to_string()));
    // 複数補間・式の組み合わせも従来どおり。
    let out = run_script("m = {k: \"v\"}\n\"#{m[\"k\"]}-#{1 + 2}\"").unwrap();
    assert_eq!(out, Value::Str("v-3".to_string()));
}

#[test]
fn inspect_and_to_s_follow_ruby_style() {
    // inspect: Str はクォート付き、Map は "k" => v・キー昇順。
    let out = run_script("[1, \"x\", true, nil]").unwrap();
    assert_eq!(out.inspect(), "[1, \"x\", true, nil]");
    let out = run_script("{name: \"alice\", age: 20}").unwrap();
    assert_eq!(out.inspect(), "{\"age\" => 20, \"name\" => \"alice\"}");
    let out = run_script("{tags: [\"a\", 1]}").unwrap();
    assert_eq!(out.inspect(), "{\"tags\" => [\"a\", 1]}");
    // to_s (Display): Str は素、それ以外は inspect と同じ。
    let out = run_script("\"hi\"").unwrap();
    assert_eq!(out.to_string(), "hi");
    assert_eq!(out.inspect(), "\"hi\"");
    let out = run_script("[1, \"x\"]").unwrap();
    assert_eq!(out.to_string(), "[1, \"x\"]");
    // エスケープ。
    let out = run_script("\"a\\\"b\\nc\"").unwrap();
    assert_eq!(out.inspect(), "\"a\\\"b\\nc\"");
    // 補間マーカー \#{ は inspect でもエスケープを保持する。
    let out = run_script("\"\\#{1}\"").unwrap();
    assert_eq!(out.inspect(), "\"\\#{1}\"");
    // 補間は to_s 形式。
    let out = run_script("\"v=#{[1, 2]}\"").unwrap();
    assert_eq!(out, Value::Str("v=[1, 2]".to_string()));
    // 参照型は path / id をエスケープ・クォートする。
    let out = run_script("doc(\"users/1\")").unwrap();
    assert_eq!(out.inspect(), "#<DocRef path=\"users/1\">");
    let out = run_script("col(\"users\")").unwrap();
    assert_eq!(out.inspect(), "#<CollectionRef path=\"users\">");
    let out = run_script("col_group(\"items\")").unwrap();
    assert_eq!(out.inspect(), "#<CollectionGroupRef id=\"items\">");
}

#[test]
fn delete_field_removes_fields_via_update_and_merge_set() {
    // emulator テスト(emulator_delete_field_and_increment_match_in_memory)と
    // 同じケース。update / merge set でフィールドを削除できる。
    let out = run_script(
        "set(\"users/df1\", {name: \"a\", age: 1, extra: true})\nupdate(\"users/df1\", {age: delete_field()})\nget(\"users/df1\")",
    )
    .unwrap();
    assert_eq!(out.inspect(), "{\"extra\" => true, \"name\" => \"a\"}");
    let out = run_script(
        "set(\"users/df2\", {name: \"a\", age: 1})\nset(\"users/df2\", {age: delete_field()}, merge: true)\nget(\"users/df2\")",
    )
    .unwrap();
    assert_eq!(out.inspect(), "{\"name\" => \"a\"}");
    // ドット区切りのネストパス・存在しないフィールド(no-op)。
    let out = run_script(
        "set(\"users/df3\", {profile: {name: \"a\", age: 1}})\nupdate(\"users/df3\", {\"profile.age\": delete_field(), missing: delete_field()})\nget(\"users/df3\")",
    )
    .unwrap();
    assert_eq!(out.inspect(), "{\"profile\" => {\"name\" => \"a\"}}");
    // 置換 set ではエラー。
    let err = run_script("set(\"users/df4\", {age: delete_field()})").unwrap_err();
    assert!(
        format!("{err}").contains("delete_field() cannot be used with set() without merge"),
        "{err}"
    );
}

#[test]
fn increment_adds_to_numeric_fields() {
    // emulator テスト(emulator_delete_field_and_increment_match_in_memory)と
    // 同じケース。なければ n、あれば加算。
    let out = run_script(
        "set(\"users/ic1\", {visits: 10})\nupdate(\"users/ic1\", {visits: increment(5), fresh: increment(3)})\nget(\"users/ic1\")",
    )
    .unwrap();
    assert_eq!(out.inspect(), "{\"fresh\" => 3, \"visits\" => 15}");
    // ドット区切りのネストパス・merge set でも使える。
    let out = run_script(
        "set(\"users/ic2\", {stats: {n: 1}})\nupdate(\"users/ic2\", {\"stats.n\": increment(2)})\nset(\"users/ic2\", {\"stats.m\": increment(4)}, merge: true)\nget(\"users/ic2\")",
    )
    .unwrap();
    assert_eq!(out.inspect(), "{\"stats\" => {\"m\" => 4, \"n\" => 3}}");
    // 引数なしはエラー。非 Int やオーバーフローは Firestore と同様に上書き・飽和する。
    let err = run_script("increment()").unwrap_err();
    assert!(format!("{err}").contains("expected int argument"), "{err}");
    let out = run_script(
        "set(\"users/ic3\", {name: \"a\"})\nupdate(\"users/ic3\", {name: increment(1)})\nget(\"users/ic3\")",
    )
    .unwrap();
    assert_eq!(out.inspect(), "{\"name\" => 1}");
    let out = run_script(
        "set(\"users/ic4\", {n: 9223372036854775807})\nupdate(\"users/ic4\", {n: increment(1)})\nget(\"users/ic4\")",
    )
    .unwrap();
    assert_eq!(out.inspect(), "{\"n\" => 9223372036854775807}");
    let out = run_script(
        "set(\"users/ic5\", {n: -9223372036854775807 - 1})\nupdate(\"users/ic5\", {n: increment(-1)})\nget(\"users/ic5\")",
    )
    .unwrap();
    assert_eq!(out.inspect(), "{\"n\" => -9223372036854775808}");
}

#[test]
fn nested_sentinels_are_rejected() {
    let err = run_script("set(\"users/ns1\", {nested: {n: increment(1)}})").unwrap_err();
    assert!(
        format!("{err}").contains("field transforms cannot be nested"),
        "{err}"
    );
    let err = run_script("set(\"users/ns2\", {arr: [increment(1)]})").unwrap_err();
    assert!(
        format!("{err}").contains("field transforms cannot be nested"),
        "{err}"
    );
    let err = run_script("set(\"users/ns3\", {nested: {df: delete_field()}})").unwrap_err();
    assert!(
        format!("{err}").contains("field transforms cannot be nested"),
        "{err}"
    );
    let err = run_script("set(\"users/ns4\", {arr: [delete_field()]})").unwrap_err();
    assert!(
        format!("{err}").contains("field transforms cannot be nested"),
        "{err}"
    );
    let err =
        run_script("set(\"users/ns5\", {})\nupdate(\"users/ns5\", {nested: {n: increment(1)}})")
            .unwrap_err();
    assert!(
        format!("{err}").contains("field transforms cannot be nested"),
        "{err}"
    );
    let err =
        run_script("set(\"users/ns6\", {})\nupdate(\"users/ns6\", {nested: {df: delete_field()}})")
            .unwrap_err();
    assert!(
        format!("{err}").contains("field transforms cannot be nested"),
        "{err}"
    );
}

#[test]
fn update_deletes_ancestor_before_descendant_increment() {
    let out = run_script(
        "set(\"users/del_inc\", {a: {b: 10}})\nupdate(\"users/del_inc\", {a: delete_field(), \"a.b\": increment(1)})\nget(\"users/del_inc\")",
    )
    .unwrap();
    assert_eq!(out.inspect(), "{\"a\" => {\"b\" => 1}}");
}

#[test]
fn replacement_set_resolves_increment_under_literal_hyphen_key() {
    let out =
        run_script("set(\"users/lit_hyphen\", {\"a-b\": increment(1)})\nget(\"users/lit_hyphen\")")
            .unwrap();
    assert_eq!(out.inspect(), "{\"a-b\" => 1}");
}

#[test]
fn update_applies_plain_before_descendant_increment() {
    let out = run_script(
        "set(\"users/plain_inc\", {a: {b: 10}})\nupdate(\"users/plain_inc\", {a: {b: 100}, \"a.b\": increment(1)})\nget(\"users/plain_inc\")",
    )
    .unwrap();
    assert_eq!(out.inspect(), "{\"a\" => {\"b\" => 101}}");
}

#[test]
fn timestamp_comparison_diff_and_rfc3339() {
    // 比較: Timestamp 同士は秒で比較できる。
    let out = run_script(
        "t1 = timestamp(100)\nt2 = timestamp(200)\n[t1 < t2, t1 > t2, t1 <= t1, t2 >= t1, t1 == timestamp(100), t1 != t2]",
    )
    .unwrap();
    assert_eq!(
        out,
        Value::Array(vec![
            Value::Bool(true),
            Value::Bool(false),
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(true),
        ])
    );
    // 差分: t2 - t1 は秒差の Int。
    let out =
        run_script("[timestamp(200) - timestamp(100), timestamp(100) - timestamp(200)]").unwrap();
    assert_eq!(out, Value::Array(vec![Value::Int(100), Value::Int(-100)]));
    // 異型との比較・減算は従来どおりエラー。
    let err = run_script("timestamp(1) > 2").unwrap_err();
    assert!(format!("{err}").contains("unsupported > operands"), "{err}");
    let err = run_script("timestamp(1) - 2").unwrap_err();
    assert!(format!("{err}").contains("unsupported - operands"), "{err}");
    // RFC3339 出力(UTC)。
    let out = run_script("timestamp(0).to_rfc3339()").unwrap();
    assert_eq!(out, Value::Str("1970-01-01T00:00:00+00:00".to_string()));
    let out = run_script("timestamp(\"2026-02-01T00:00:00+09:00\").to_rfc3339()").unwrap();
    assert_eq!(out, Value::Str("2026-01-31T15:00:00+00:00".to_string()));
    // RFC3339 の 4 桁年(0000..=9999)の範囲外はエラー。
    let err = run_script("timestamp(253402300800).to_rfc3339()").unwrap_err();
    assert!(format!("{err}").contains("timestamp out of range"), "{err}");
    // if 条件にも使える。
    let out = run_script("if timestamp(200) > timestamp(100) { 1 } else { 2 }").unwrap();
    assert_eq!(out, Value::Int(1));
}

#[test]
fn order_by_sorts_and_excludes_docs_missing_the_field() {
    // emulator テスト(emulator_extended_query_matches_in_memory)と
    // 同じケース。order_by 対象を持たない ob4 は除外、同値はパス辞書順。
    let setup = "set(\"users/ob1\", {age: 30, name: \"c\"})\nset(\"users/ob2\", {age: 20, name: \"a\"})\nset(\"users/ob3\", {age: 20, name: \"b\"})\nset(\"users/ob4\", {other: 1})";
    let out = run_script(&format!(
        "{setup}\ncol(\"users\").order_by(\"age\", \"asc\").get().map {{ |r| r[\"__name__\"] }}"
    ))
    .unwrap();
    assert_eq!(
        out,
        Value::Array(vec![
            Value::Str("users/ob2".to_string()),
            Value::Str("users/ob3".to_string()),
            Value::Str("users/ob1".to_string()),
        ])
    );
    let out = run_script(&format!(
        "{setup}\ncol(\"users\").order_by(\"age\", \"desc\").get().map {{ |r| r[\"__name__\"] }}"
    ))
    .unwrap();
    assert_eq!(
        out,
        Value::Array(vec![
            Value::Str("users/ob1".to_string()),
            Value::Str("users/ob2".to_string()),
            Value::Str("users/ob3".to_string()),
        ])
    );
    // 複数 order_by は順に適用される(name の降順で ob3 が先)。
    let out = run_script(&format!(
        "{setup}\ncol(\"users\").order_by(\"age\", \"asc\").order_by(\"name\", \"desc\").get().map {{ |r| r[\"__name__\"] }}"
    ))
    .unwrap();
    assert_eq!(
        out,
        Value::Array(vec![
            Value::Str("users/ob3".to_string()),
            Value::Str("users/ob2".to_string()),
            Value::Str("users/ob1".to_string()),
        ])
    );
    // __name__ によるソートは全ドキュメントを対象にし、パス順で並ぶ。
    let out = run_script(&format!(
        "{setup}\ncol(\"users\").order_by(\"__name__\", \"desc\").get().map {{ |r| r[\"__name__\"] }}"
    ))
    .unwrap();
    assert_eq!(
        out,
        Value::Array(vec![
            Value::Str("users/ob4".to_string()),
            Value::Str("users/ob3".to_string()),
            Value::Str("users/ob2".to_string()),
            Value::Str("users/ob1".to_string()),
        ])
    );
    // ドット区切りのネストしたフィールドパスでのソートと where
    let setup_nested = "set(\"nested/n1\", {profile: {age: 30}})\nset(\"nested/n2\", {profile: {age: 10}})\nset(\"nested/n3\", {profile: 1})";
    let out = run_script(&format!(
        "{setup_nested}\ncol(\"nested\").order_by(\"profile.age\", \"asc\").get().map {{ |r| r[\"__name__\"] }}"
    ))
    .unwrap();
    assert_eq!(
        out,
        Value::Array(vec![
            Value::Str("nested/n2".to_string()),
            Value::Str("nested/n1".to_string()),
        ])
    );
    let out = run_script(&format!(
        "{setup_nested}\ncol(\"nested\").where(\"profile.age\", \">=\", 20).get().map {{ |r| r[\"__name__\"] }}"
    ))
    .unwrap();
    assert_eq!(out, Value::Array(vec![Value::Str("nested/n1".to_string())]));
    // 不正な向きは呼び出し時にエラー。
    let err = run_script("col(\"users\").order_by(\"age\", \"up\")").unwrap_err();
    assert!(
        format!("{err}").contains("order_by direction must be \"asc\" or \"desc\""),
        "{err}"
    );
}

#[test]
fn where_supports_not_equal_and_membership_operators() {
    // emulator テスト(emulator_extended_query_matches_in_memory)と
    // 同じケース。`!=` は存在 + 不一致、欠損はマッチしない。
    let setup = "set(\"users/mb1\", {age: 20, tag: \"x\"})\nset(\"users/mb2\", {age: 30})\nset(\"users/mb3\", {name: \"n\"})";
    let out = run_script(&format!(
        "{setup}\n[col(\"users\").where(\"age\", \"!=\", 20).get().len(), col(\"users\").where(\"tag\", \"!=\", 5).get().len(), col(\"users\").where(\"missing\", \"!=\", 1).get().len()]"
    ))
    .unwrap();
    assert_eq!(
        out,
        Value::Array(vec![Value::Int(1), Value::Int(1), Value::Int(0)])
    );
    // `in` / `not-in`。not-in に nil を含む場合は常に 0 件。
    let out = run_script(&format!(
        "{setup}\n[col(\"users\").where(\"age\", \"in\", [20, 30]).get().len(), col(\"users\").where(\"age\", \"not-in\", [20]).get().len(), col(\"users\").where(\"age\", \"in\", []).get().len(), col(\"users\").where(\"age\", \"not-in\", []).get().len(), col(\"users\").where(\"age\", \"not-in\", [20, nil]).get().len(), col(\"users\").where(\"age\", \"!=\", 20).where(\"age\", \"!=\", 30).get().len()]"
    ))
    .unwrap();
    assert_eq!(
        out,
        Value::Array(vec![
            Value::Int(2),
            Value::Int(1),
            Value::Int(0),
            Value::Int(2),
            Value::Int(0),
            Value::Int(0),
        ])
    );
    // 配列でなければ実行時エラー、11 要素以上もエラー。
    let setup_nil = "set(\"items/1\", {val: nil})\nset(\"items/2\", {val: 10})";
    let out = run_script(&format!(
        "{setup_nil}\n[col(\"items\").where(\"val\", \"not-in\", []).get().len(), col(\"items\").where(\"val\", \"!=\", nil).get().len(), col(\"items\").where(\"val\", \"not-in\", [nil]).get().len()]"
    ))
    .unwrap();
    assert_eq!(
        out,
        Value::Array(vec![Value::Int(2), Value::Int(1), Value::Int(0)])
    );
    let err = run_script("col(\"users\").where(\"age\", \"in\", 5).get()").unwrap_err();
    assert!(
        format!("{err}").contains("in filter requires an array value"),
        "{err}"
    );
    let err = run_script("col(\"users\").where(\"age\", \"not-in\", \"x\").get()").unwrap_err();
    assert!(
        format!("{err}").contains("not-in filter requires an array value"),
        "{err}"
    );
    let many = (0..11)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let err = run_script(&format!(
        "col(\"users\").where(\"age\", \"in\", [{many}]).get()"
    ))
    .unwrap_err();
    assert!(format!("{err}").contains("at most 10 elements"), "{err}");
}

#[test]
fn where_supports_array_contains_operators() {
    // emulator テスト(emulator_extended_query_matches_in_memory)と
    // 同じケース。
    let setup = "set(\"users/ac1\", {tags: [\"a\", \"b\"]})\nset(\"users/ac2\", {tags: [\"b\", \"c\"]})\nset(\"users/ac3\", {n: 1})";
    let out = run_script(&format!(
        "{setup}\n[col(\"users\").where(\"tags\", \"array_contains\", \"b\").get().len(), col(\"users\").where(\"tags\", \"array_contains\", \"z\").get().len(), col(\"users\").where(\"tags\", \"array_contains_any\", [\"a\", \"c\"]).get().len(), col(\"users\").where(\"tags\", \"array_contains_any\", []).get().len(), col(\"users\").where(\"n\", \"array_contains\", 1).get().len()]"
    ))
    .unwrap();
    assert_eq!(
        out,
        Value::Array(vec![
            Value::Int(2),
            Value::Int(0),
            Value::Int(2),
            Value::Int(0),
            Value::Int(0)
        ])
    );
    let err = run_script("col(\"users\").where(\"tags\", \"array_contains_any\", \"b\").get()")
        .unwrap_err();
    assert!(
        format!("{err}").contains("array_contains_any filter requires an array value"),
        "{err}"
    );
}

#[test]
fn offset_skips_after_sort_and_count_ignores_paging() {
    // emulator テスト(emulator_extended_query_matches_in_memory)と
    // 同じケース。sort → offset → limit の順に適用、count は全件。
    let setup = "set(\"users/pg1\", {ord: 1})\nset(\"users/pg2\", {ord: 2})\nset(\"users/pg3\", {ord: 3})\nset(\"users/pg4\", {ord: 4})\nset(\"users/pg5\", {ord: 5})";
    let out = run_script(&format!(
        "{setup}\ncol(\"users\").order_by(\"ord\", \"asc\").offset(2).get().map {{ |r| r[\"ord\"] }}"
    ))
    .unwrap();
    assert_eq!(
        out,
        Value::Array(vec![Value::Int(3), Value::Int(4), Value::Int(5)])
    );
    let out = run_script(&format!(
        "{setup}\ncol(\"users\").order_by(\"ord\", \"asc\").offset(1).limit(2).get().map {{ |r| r[\"ord\"] }}"
    ))
    .unwrap();
    assert_eq!(out, Value::Array(vec![Value::Int(2), Value::Int(3)]));
    let out = run_script(&format!(
        "{setup}\n[col(\"users\").order_by(\"ord\", \"asc\").offset(99).get().len(), col(\"users\").where(\"ord\", \">=\", 2).limit(1).count(), col(\"users\").order_by(\"ord\", \"desc\").offset(1).count()]"
    ))
    .unwrap();
    assert_eq!(
        out,
        Value::Array(vec![Value::Int(0), Value::Int(4), Value::Int(5)])
    );
    // count は batch/transaction 内で実行できない(get と同じ)。
    let err = run_script("batch { col(\"users\").count() }").unwrap_err();
    assert!(
        format!("{err}").contains("not allowed inside batch"),
        "{err}"
    );
    let err = run_script("transaction { col(\"users\").count() }").unwrap_err();
    assert!(
        format!("{err}").contains("not allowed inside transaction"),
        "{err}"
    );
    // 負の offset はエラー。
    let err = run_script("col(\"users\").offset(0 - 1).get()").unwrap_err();
    assert!(format!("{err}").contains("offset must be >= 0"), "{err}");
}

#[test]
fn overlapping_update_and_delete_paths_are_rejected() {
    let err = run_script(
        "set(\"users/ov1\", {})\nupdate(\"users/ov1\", {a: delete_field(), \"a.b\": 1})",
    )
    .unwrap_err();
    assert!(
        format!("{err}").contains("overlapping field paths"),
        "{err}"
    );

    let err = run_script("set(\"users/ov2\", {a: 1, \"a.b\": 2}, merge: true)").unwrap_err();
    assert!(
        format!("{err}").contains("overlapping field paths"),
        "{err}"
    );

    let err = run_script(
        "set(\"users/ov3\", {})\nupdate(\"users/ov3\", {\"a.b\": delete_field(), a: 1})",
    )
    .unwrap_err();
    assert!(
        format!("{err}").contains("overlapping field paths"),
        "{err}"
    );
}
