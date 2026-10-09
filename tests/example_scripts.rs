use flare::run_script;
use flare::Value;

#[test]
fn runs_example_scripts_successfully() {
    let cases = vec![
        ("examples/scripts/01_basic.fl", Value::Int(5)),
        ("examples/scripts/02_keywords_and_blocks.fl", Value::Int(30)),
        (
            "examples/scripts/03_interpolation.fl",
            Value::Str("hello flare 3".to_string()),
        ),
        (
            "examples/scripts/04_timestamp.fl",
            Value::Array(vec![Value::Int(1769871600), Value::Int(1769871660)]),
        ),
        (
            "examples/scripts/06_array_ops.fl",
            Value::Map(
                [(
                    "tags".to_string(),
                    Value::Array(vec![
                        Value::Str("b".to_string()),
                        Value::Str("c".to_string()),
                    ]),
                )]
                .into_iter()
                .collect(),
            ),
        ),
        (
            "examples/scripts/07_transaction_batch.fl",
            Value::Map(
                [("kind".to_string(), Value::Str("done".to_string()))]
                    .into_iter()
                    .collect(),
            ),
        ),
        (
            "examples/scripts/10_iteration.fl",
            Value::Array(vec![Value::Int(2), Value::Int(4), Value::Int(6)]),
        ),
        (
            "examples/scripts/11_storage_crud.fl",
            Value::Array(vec![
                Value::Bytes(vec![0, 255, 128, 10]),
                Value::Str("application/octet-stream".to_string()),
                Value::Int(4),
            ]),
        ),
    ];

    for (path, expected) in cases {
        let src = std::fs::read_to_string(path).unwrap();
        let out = run_script(&src).unwrap();
        assert_eq!(out, expected, "failed for {path}");
    }
}

#[test]
fn runs_query_example_with_expected_shape() {
    let src = std::fs::read_to_string("examples/scripts/05_query.fl").unwrap();
    let out = run_script(&src).unwrap();
    let Value::Array(rows) = out else {
        panic!("expected array output from query example");
    };
    assert_eq!(rows.len(), 1);
    let Value::Map(row) = &rows[0] else {
        panic!("expected map row");
    };
    assert_eq!(
        row.get("__name__"),
        Some(&Value::Str("users/2".to_string()))
    );
    assert_eq!(row.get("age"), Some(&Value::Int(30)));
}

#[test]
fn runs_error_example_and_fails_as_expected() {
    let src = std::fs::read_to_string("examples/scripts/08_reassignment_error.fl").unwrap();
    let err = run_script(&src).unwrap_err();
    assert!(format!("{err}").contains("reassignment is not allowed"));
}

#[test]
fn emulator_smoke_example_round_trips_in_memory() {
    let src = std::fs::read_to_string("examples/scripts/09_emulator_smoke.fl").unwrap();
    let out = run_script(&src).unwrap();
    let Value::Map(m) = out else {
        panic!("expected map");
    };
    assert_eq!(m.get("ok"), Some(&Value::Bool(true)));
    assert!(matches!(m.get("checked_at"), Some(Value::Timestamp(_))));
}

#[test]
fn auth_smoke_example_round_trips_in_memory_without_printing_tokens() {
    let src = std::fs::read_to_string("examples/scripts/11_auth_smoke.fl").unwrap();
    // The example prints via `puts`, so capture is not needed: assert only on
    // the final value, and re-check the redaction rule separately below.
    let out = run_script(&src).unwrap();
    assert_eq!(
        out,
        Value::Bool(true),
        "sign-out should leave no current user"
    );

    let session =
        run_script("auth_sign_up(\"example-check@example.com\", \"password123\")").unwrap();
    let inspected = session.inspect();
    assert!(inspected.contains("id_token=<redacted>"), "{inspected}");
    assert!(!inspected.contains("fake-id-token"), "{inspected}");
    assert!(!inspected.contains("fake-refresh-token"), "{inspected}");
}
