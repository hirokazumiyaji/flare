use flare::repl::{is_complete_input, run_stream};
use flare::runtime::Runtime;
use flare::{InMemoryFirestore, Value};

#[test]
fn is_complete_input_detects_delimiters_and_strings() {
    // Complete inputs
    assert!(is_complete_input("1 + 1"));
    assert!(is_complete_input("a = 10"));
    assert!(is_complete_input(""));
    assert!(is_complete_input("   \n  \t  "));
    assert!(is_complete_input("# a comment only"));
    assert!(is_complete_input("\"complete string\""));
    assert!(is_complete_input("\"string with #{1 + 2} interp\""));

    // Incomplete inputs
    assert!(!is_complete_input("fn add(a, b) {"));
    assert!(!is_complete_input("[1, 2,"));
    assert!(!is_complete_input("{ a: 1,"));
    assert!(!is_complete_input("puts("));
    assert!(!is_complete_input("\"unterminated string"));
    assert!(!is_complete_input("\"unterminated #{1 + "));

    // Multi-line complete inputs
    assert!(is_complete_input("fn add(a, b) {\n  a + b\n}"));
    assert!(is_complete_input("[\n  1,\n  2\n]"));
    assert!(is_complete_input("{\n  a: 1,\n  b: 2\n}"));
    assert!(is_complete_input("puts(\n  1,\n  2\n)"));
}

#[test]
fn runtime_repl_mode_allows_top_level_reassignment_and_redefinition() {
    let mut rt = Runtime::new(InMemoryFirestore::default());

    // Without REPL mode, reassignment is rejected
    rt.run("a = 1").unwrap();
    let err = rt.run("a = 2").unwrap_err();
    assert!(err.to_string().contains("reassignment is not allowed: a"));

    // Enable REPL mode
    rt.set_repl_mode(true);
    assert!(rt.repl_mode());

    // Top-level variable reassignment works
    let out = rt.run("a = 2").unwrap();
    assert_eq!(out, Value::Int(2));
    assert_eq!(rt.run("a").unwrap(), Value::Int(2));

    // Top-level function definition and redefinition work
    rt.run("fn foo() { 10 }").unwrap();
    assert_eq!(rt.run("foo()").unwrap(), Value::Int(10));
    rt.run("fn foo() { 20 }").unwrap();
    assert_eq!(rt.run("foo()").unwrap(), Value::Int(20));

    // Builtins still cannot be redefined
    let err = rt.run("fn puts() { 1 }").unwrap_err();
    assert!(err.to_string().contains("cannot redefine builtin: puts"));

    // Inside function bodies, reassignment is still forbidden
    rt.run("fn bar() { x = 1\n x = 2 }").unwrap();
    let err = rt.run("bar()").unwrap_err();
    assert!(err.to_string().contains("reassignment is not allowed: x"));

    // Global variable names inspection
    let vars = rt.global_variable_names();
    assert!(vars.contains(&"a".to_string()));
    assert!(vars.contains(&"foo".to_string()));
}

#[test]
fn repl_stream_runs_statements_preserves_state_and_handles_errors() {
    let mut rt = Runtime::new(InMemoryFirestore::default());

    let input = b"a = 10\nb = a * 2\nb + 5\nundefined_var\nfn mul(x, y) {\n  x * y\n}\nmul(3, 4)\nexit\nignored_after_exit = 99\n";
    let mut output = Vec::new();

    run_stream(&mut rt, &input[..], &mut output).unwrap();

    let out_str = String::from_utf8(output).unwrap();
    let lines: Vec<&str> = out_str.lines().collect();

    assert_eq!(lines[0], "=> 10");
    assert_eq!(lines[1], "=> 20");
    assert_eq!(lines[2], "=> 25");
    assert!(
        lines[3].contains("undefined variable: undefined_var"),
        "lines[3] was: {}",
        lines[3]
    );
    assert_eq!(lines[4], "=> nil");
    assert_eq!(lines[5], "=> 12");
    assert_eq!(lines.len(), 6);

    // Verify exit stopped execution before reading later lines
    assert_eq!(rt.run("a").unwrap(), Value::Int(10));
    assert!(rt.run("ignored_after_exit").is_err());
}

#[test]
fn repl_completion_handles_unicode_and_cursor_boundaries() {
    use flare::repl::FlareHelper;
    use rustyline::completion::Completer;
    use rustyline::history::DefaultHistory;
    use rustyline::Context;

    let helper = FlareHelper::new();
    let history = DefaultHistory::new();
    let ctx = Context::new(&history);

    // Normal prefix
    let (start, matches) = helper.complete("pu", 2, &ctx).unwrap();
    assert_eq!(start, 0);
    assert!(matches.iter().any(|m| m.display == "puts"));

    // Unicode in line before completion
    let line = "x = \"こんにちは\" + pu";
    let (start, matches) = helper.complete(line, line.len(), &ctx).unwrap();
    assert_eq!(&line[start..], "pu");
    assert!(matches.iter().any(|m| m.display == "puts"));

    // Cursor landing inside multibyte character does not panic
    let jp = "あいうえお";
    for pos in 0..=jp.len() {
        let res = helper.complete(jp, pos, &ctx);
        assert!(res.is_ok());
    }
}

#[test]
fn repl_stream_enforces_source_size_budget_on_pending_buffer() {
    let limits = flare::RuntimeLimits::default().with_max_source_bytes(20);
    let mut rt = Runtime::with_limits(InMemoryFirestore::default(), limits);

    let input = b"\"very long unterminated string that exceeds twenty bytes\n1 + 1\n";
    let mut output = Vec::new();

    run_stream(&mut rt, &input[..], &mut output).unwrap();

    let out_str = String::from_utf8(output).unwrap();
    let lines: Vec<&str> = out_str.lines().collect();

    assert!(lines[0].contains("source size limit exceeded"));
    assert_eq!(lines[1], "=> 2");
}
