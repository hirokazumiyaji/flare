pub mod ast;
pub mod emulator;
pub mod firestore;
pub mod lexer;
pub mod parser;
pub mod runtime;
pub mod value;

pub use emulator::FirestoreEmulator;

use std::error::Error;

use runtime::Runtime;

pub use firestore::{Firestore, InMemoryFirestore};
pub use value::Value;

pub fn run_script(source: &str) -> Result<Value, Box<dyn Error>> {
    run_script_with_firestore(source, InMemoryFirestore::default())
}

pub fn run_script_with_firestore<F: Firestore>(
    source: &str,
    firestore: F,
) -> Result<Value, Box<dyn Error>> {
    let tokens = lexer::lex(source)?;
    let mut parser = parser::Parser::new(tokens);
    let program = parser.parse_program()?;
    let mut runtime = Runtime::new(firestore);
    runtime.eval_program(&program)
}

#[cfg(test)]
mod tests {
    use super::run_script;
    use crate::lexer;
    use crate::parser::Parser;
    use crate::runtime::Runtime;
    use crate::{InMemoryFirestore, Value};

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
        let err = lexer::lex("\"foo").unwrap_err();
        assert!(format!("{err}").contains("unterminated"));
    }

    #[test]
    fn evals_def_and_call() {
        let out = run_script("fn add(a,b) { return a + b }\nadd(2,3)").unwrap();
        assert_eq!(out, Value::Int(5));
    }

    #[test]
    fn set_requires_map_and_get_returns_map() {
        let out =
            run_script("set(doc(\"users/u1\"), {name: \"alice\"})\nget(\"users/u1\")").unwrap();
        assert_eq!(format!("{out:?}"), "Map({\"name\": Str(\"alice\")})");
    }

    #[test]
    fn supports_timestamp_and_query() {
        let out = run_script(
            "set(doc(\"users/1\"), {age: 20, created: timestamp(100)})\nset(doc(\"users/2\"), {age: 30, created: now()})\ncollection(\"users\").where(\"age\", \">=\", 21).limit(1).get()",
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
        let out =
            run_script("set(\"users/st\", {updated_at: server_timestamp()})\nget(\"users/st\")")
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

    fn run_in(
        runtime: &mut Runtime<InMemoryFirestore>,
        source: &str,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let tokens = lexer::lex(source)?;
        let program = Parser::new(tokens).parse_program()?;
        runtime.eval_program(&program)
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
            "set(\"users/qz\", {ord: 1})\nset(\"users/qa\", {ord: 2})\nset(\"users/qm\", {ord: 3})\ncollection(\"users\").where(\"ord\", \">=\", 1).limit(1).get()",
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
        let out = run_script(
            "t1 = timestamp(100)\nt2 = t1.add_seconds(10)\n[t1.to_unix(), t2.to_unix()]",
        )
        .unwrap();
        assert_eq!(out, Value::Array(vec![Value::Int(100), Value::Int(110)]));
    }
}
