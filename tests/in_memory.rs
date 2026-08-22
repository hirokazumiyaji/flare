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
    let err = run_script("batch { collection(\"users\").get() }").unwrap_err();
    assert!(format!("{err}").contains("not allowed inside batch"));
    let err = run_script("transaction { collection(\"users\").get() }").unwrap_err();
    assert!(format!("{err}").contains("not allowed inside transaction"));
}

#[test]
fn update_missing_document_fails_in_memory() {
    let err = run_script("update(\"users/nope\", {x: 1})").unwrap_err();
    assert!(format!("{err}").contains("document not found"));
}
