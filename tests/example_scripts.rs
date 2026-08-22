use firelang::run_script;
use firelang::Value;

#[test]
fn runs_example_scripts_successfully() {
    let cases = vec![
        ("examples/scripts/01_basic.fl", Value::Int(5)),
        ("examples/scripts/02_keywords_and_blocks.fl", Value::Int(30)),
        (
            "examples/scripts/03_interpolation.fl",
            Value::Str("hello firelang 3".to_string()),
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
        ("examples/scripts/09_emulator_smoke.fl", Value::Int(5)),
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
    match out {
        Value::Array(rows) => {
            assert_eq!(rows.len(), 1);
            let debug = format!("{:?}", rows[0]);
            assert!(debug.contains("users/2"));
            assert!(debug.contains("age"));
        }
        _ => panic!("expected array output from query example"),
    }
}

#[test]
fn runs_error_example_and_fails_as_expected() {
    let src = std::fs::read_to_string("examples/scripts/08_reassignment_error.fl").unwrap();
    let err = run_script(&src).unwrap_err();
    assert!(format!("{err}").contains("reassignment is not allowed"));
}
