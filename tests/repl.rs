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

#[test]
fn repl_loop_handles_continuation_prompt_and_syntax_error_recovery() {
    use flare::repl::run_repl_loop;
    use rustyline::error::ReadlineError;

    let mut rt = Runtime::new(InMemoryFirestore::default());

    let mut prompts = Vec::new();
    let mut history = Vec::new();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let lines = vec![
        Ok("fn broken() {".to_string()),
        Ok("  let invalid = 1 + ;".to_string()),
        Ok("}".to_string()),
        Ok("1 + 1".to_string()),
        Ok("exit".to_string()),
    ];
    let mut line_iter = lines.into_iter();

    run_repl_loop(
        &mut rt,
        |_rt, prompt| {
            prompts.push(prompt.to_string());
            line_iter.next().unwrap_or(Err(ReadlineError::Eof))
        },
        |entry| history.push(entry.to_string()),
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    assert_eq!(prompts, vec!["fl> ", "fl* ", "fl* ", "fl> ", "fl> "]);
    assert_eq!(
        history,
        vec!["fn broken() {\n  let invalid = 1 + ;\n}", "1 + 1"]
    );

    let err_str = String::from_utf8(stderr).unwrap();
    assert!(err_str.contains("error:"));

    let out_str = String::from_utf8(stdout).unwrap();
    assert!(out_str.contains("=> 2"));
}

#[test]
fn repl_loop_handles_ctrl_c_interrupted_branches() {
    use flare::repl::run_repl_loop;
    use rustyline::error::ReadlineError;

    let mut rt = Runtime::new(InMemoryFirestore::default());

    let mut prompts = Vec::new();
    let mut history = Vec::new();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let lines = vec![
        Err(ReadlineError::Interrupted),
        Ok("fn pending() {".to_string()),
        Err(ReadlineError::Interrupted),
        Ok("10 + 20".to_string()),
        Ok("exit".to_string()),
    ];
    let mut line_iter = lines.into_iter();

    run_repl_loop(
        &mut rt,
        |_rt, prompt| {
            prompts.push(prompt.to_string());
            line_iter.next().unwrap_or(Err(ReadlineError::Eof))
        },
        |entry| history.push(entry.to_string()),
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    assert_eq!(prompts, vec!["fl> ", "fl> ", "fl* ", "fl> ", "fl> "]);
    assert_eq!(history, vec!["10 + 20"]);

    let out_str = String::from_utf8(stdout).unwrap();
    assert!(out_str.contains("^C\n^C\n=> 30"));
}

#[test]
fn repl_loop_handles_ctrl_d_eof_branches() {
    use flare::repl::run_repl_loop;
    use rustyline::error::ReadlineError;

    let mut rt = Runtime::new(InMemoryFirestore::default());

    let mut prompts = Vec::new();
    let mut history = Vec::new();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let lines = vec![
        Ok("fn pending() {".to_string()),
        Err(ReadlineError::Eof),
        Ok("42".to_string()),
        Err(ReadlineError::Eof),
    ];
    let mut line_iter = lines.into_iter();

    run_repl_loop(
        &mut rt,
        |_rt, prompt| {
            prompts.push(prompt.to_string());
            line_iter.next().unwrap_or(Err(ReadlineError::Eof))
        },
        |entry| history.push(entry.to_string()),
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    assert_eq!(prompts, vec!["fl> ", "fl* ", "fl> ", "fl> "]);
    assert_eq!(history, vec!["42"]);

    let out_str = String::from_utf8(stdout).unwrap();
    assert!(out_str.contains("=> 42"));
}

#[test]
fn repl_history_file_save_and_load_round_trip() {
    use flare::repl::FlareHelper;
    use rustyline::history::FileHistory;
    use rustyline::Editor;

    let temp_dir = tempfile::tempdir().unwrap();
    let history_path = temp_dir.path().join(".fl_history");

    {
        let mut rl = Editor::<FlareHelper, FileHistory>::new().unwrap();
        rl.add_history_entry("a = 100").unwrap();
        rl.add_history_entry("b = 200").unwrap();
        rl.save_history(&history_path).unwrap();
    }

    {
        let mut rl = Editor::<FlareHelper, FileHistory>::new().unwrap();
        rl.load_history(&history_path).unwrap();
        let entries: Vec<&str> = rl.history().iter().map(String::as_str).collect();
        assert_eq!(entries, vec!["a = 100", "b = 200"]);
    }
}

#[test]
fn sensitive_auth_inputs_are_detected() {
    use flare::repl::is_sensitive_input;

    // Direct builtin calls
    assert!(is_sensitive_input(
        "auth_sign_in(\"a@b.com\", \"password\")"
    ));
    assert!(is_sensitive_input(
        "auth_sign_up(\"a@b.com\", \"password\")"
    ));
    assert!(is_sensitive_input("sign_in(\"a@b.com\", \"password\")"));
    assert!(is_sensitive_input("sign_up(\"a@b.com\", \"password\")"));

    // Module calls
    assert!(is_sensitive_input(
        "Auth.sign_in(\"a@b.com\", \"password\")"
    ));
    assert!(is_sensitive_input(
        "Auth.sign_up(\"a@b.com\", \"password\")"
    ));
    assert!(is_sensitive_input(
        "Auth . sign_in(\"a@b.com\", \"password\")"
    ));
    assert!(is_sensitive_input(
        "Auth.auth_sign_in(\"a@b.com\", \"password\")"
    ));

    // Expressions and variables
    assert!(is_sensitive_input(
        "let res = auth_sign_in(\"a@b.com\", \"secret\")"
    ));
    assert!(is_sensitive_input(
        "res = Auth.sign_in(\"a@b.com\", \"secret\")"
    ));
    assert!(is_sensitive_input(
        "if true {\n  auth_sign_up(\"a@b.com\", \"password\")\n}"
    ));

    // Multi-line
    assert!(is_sensitive_input(
        "auth_sign_in(\n  \"a@b.com\",\n  \"secret\"\n)"
    ));

    // Case insensitivity
    assert!(is_sensitive_input("AUTH_SIGN_IN(\"a@b.com\", \"secret\")"));
    assert!(is_sensitive_input("AUTH.SIGN_IN(\"a@b.com\", \"secret\")"));

    // Non-sensitive inputs
    assert!(!is_sensitive_input("1 + 1"));
    assert!(!is_sensitive_input("puts(\"hello\")"));
    assert!(!is_sensitive_input("auth_current_user()"));
    assert!(!is_sensitive_input("Auth.current_user()"));
    assert!(!is_sensitive_input("auth_sign_out()"));
    assert!(!is_sensitive_input("Auth.sign_out()"));
    assert!(!is_sensitive_input("design_pattern = 1"));
    assert!(!is_sensitive_input("assignment = 2"));
    assert!(!is_sensitive_input("doc(\"users/1\").get()"));
}

#[test]
fn repl_options_resolution_respects_env_and_flags() {
    use flare::repl::ReplOptions;
    use std::path::PathBuf;

    // Default with HOME
    let opts = ReplOptions::resolve_from(|k| match k {
        "HOME" => Some("/tmp/test_home".into()),
        _ => None,
    });
    assert!(!opts.no_history);
    assert_eq!(
        opts.history_file,
        Some(PathBuf::from("/tmp/test_home/.fl_history"))
    );

    // FL_NO_HISTORY=1
    let opts = ReplOptions::resolve_from(|k| match k {
        "HOME" => Some("/tmp/test_home".into()),
        "FL_NO_HISTORY" => Some("1".into()),
        _ => None,
    });
    assert!(opts.no_history);
    assert_eq!(opts.history_file, None);

    // FL_NO_HISTORY=true
    let opts = ReplOptions::resolve_from(|k| match k {
        "FL_NO_HISTORY" => Some("true".into()),
        _ => None,
    });
    assert!(opts.no_history);
    assert_eq!(opts.history_file, None);

    // FL_HISTORY_FILE
    let opts = ReplOptions::resolve_from(|k| match k {
        "HOME" => Some("/tmp/test_home".into()),
        "FL_HISTORY_FILE" => Some("/custom/history".into()),
        _ => None,
    });
    assert!(!opts.no_history);
    assert_eq!(opts.history_file, Some(PathBuf::from("/custom/history")));

    // FL_NO_HISTORY takes precedence over FL_HISTORY_FILE
    let opts = ReplOptions::resolve_from(|k| match k {
        "FL_HISTORY_FILE" => Some("/custom/history".into()),
        "FL_NO_HISTORY" => Some("1".into()),
        _ => None,
    });
    assert!(opts.no_history);
    assert_eq!(opts.history_file, None);

    // with_no_history builder
    let opts = ReplOptions::resolve_from(|k| match k {
        "FL_HISTORY_FILE" => Some("/custom/history".into()),
        _ => None,
    })
    .with_no_history(true);
    assert!(opts.no_history);
    assert_eq!(opts.history_file, None);

    // with_history_file does not override no_history
    let opts = ReplOptions::resolve_from(|k| match k {
        "FL_NO_HISTORY" => Some("1".into()),
        _ => None,
    })
    .with_history_file(Some(PathBuf::from("/custom/history")));
    assert!(opts.no_history);
    assert_eq!(opts.history_file, None);
}

#[cfg(unix)]
#[test]
fn history_file_permissions_are_0600() {
    use flare::repl::{ensure_file_0600, set_file_mode_0600};
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = tempfile::tempdir_in("/tmp").unwrap();
    let history_path = temp_dir.path().join("sub").join(".fl_history");

    // ensure_file_0600 creates parent dir and file with 0600
    ensure_file_0600(&history_path).unwrap();
    let meta = std::fs::metadata(&history_path).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);

    // Change permissions to 0644 then set_file_mode_0600 tightens back to 0600
    std::fs::set_permissions(&history_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let meta = std::fs::metadata(&history_path).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o644);

    set_file_mode_0600(&history_path).unwrap();
    let meta = std::fs::metadata(&history_path).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
}

#[test]
fn repl_history_omits_sensitive_entries() {
    use flare::repl::{is_sensitive_input, FlareHelper};
    use rustyline::history::FileHistory;
    use rustyline::Editor;

    let mut rl = Editor::<FlareHelper, FileHistory>::new().unwrap();

    let inputs = [
        "x = 10",
        "auth_sign_in(\"user@example.com\", \"secret\")",
        "puts(x)",
        "Auth.sign_up(\"new@example.com\", \"pass\")",
        "y = 20",
    ];

    for input in inputs {
        if !is_sensitive_input(input) {
            let _ = rl.add_history_entry(input);
        }
    }

    let history: Vec<String> = rl.history().iter().map(|s| s.to_string()).collect();
    assert_eq!(history, vec!["x = 10", "puts(x)", "y = 20"]);
}

#[test]
fn repl_history_omits_multiline_sensitive_entries() {
    use flare::repl::{is_complete_input, is_sensitive_input, FlareHelper};
    use rustyline::history::FileHistory;
    use rustyline::Editor;

    let mut rl = Editor::<FlareHelper, FileHistory>::new().unwrap();

    let lines = [
        "fn add(a, b) {",
        "  a + b",
        "}",
        "auth_sign_in(",
        "  \"user@example.com\",",
        "  \"secret_pass\"",
        ")",
        "Auth.",
        "  sign_up(",
        "    \"other@example.com\",",
        "    \"secret_pass2\"",
        "  )",
        "result = 42",
    ];

    let mut buffer = String::new();
    for line in lines {
        if !buffer.is_empty() {
            buffer.push('\n');
        }
        buffer.push_str(line);

        if is_complete_input(&buffer) {
            let complete = std::mem::take(&mut buffer);
            let trimmed = complete.trim();
            if !is_sensitive_input(trimmed) {
                let _ = rl.add_history_entry(trimmed);
            }
        }
    }

    let history: Vec<String> = rl.history().iter().map(|s| s.to_string()).collect();
    assert_eq!(history, vec!["fn add(a, b) {\n  a + b\n}", "result = 42",]);
}

#[test]
fn repl_builtins_match_builtin_table() {
    let mut expected = flare::builtin_names();
    expected.sort();
    let mut actual = flare::repl::BUILTINS.to_vec();
    actual.sort();
    assert_eq!(
        actual, expected,
        "repl BUILTINS does not match builtin_specs"
    );
}

#[test]
fn repl_completions_include_all_builtins_and_no_methods() {
    use flare::repl::{default_candidates, FlareHelper};
    use rustyline::completion::Completer;
    use rustyline::history::DefaultHistory;
    use rustyline::Context;

    let helper = FlareHelper::new();
    let history = DefaultHistory::new();
    let ctx = Context::new(&history);
    let candidates = default_candidates();

    // All builtins are present in candidates
    for name in flare::builtin_names() {
        assert!(
            candidates.contains(&name.to_string()),
            "missing builtin completion candidate: {name}"
        );
    }

    // "now" and "len" should be present and completable
    let (_, matches) = helper.complete("no", 2, &ctx).unwrap();
    assert!(matches.iter().any(|m| m.display == "now"));

    let (_, matches) = helper.complete("le", 2, &ctx).unwrap();
    assert!(matches.iter().any(|m| m.display == "len"));

    // Methods like "map", "where", "order_by", "limit", "offset", "count"
    // must not be in the global builtin list or completions
    for not_builtin in &["where", "order_by", "limit", "offset", "count", "map"] {
        assert!(
            !flare::repl::BUILTINS.contains(not_builtin),
            "BUILTINS contains non-builtin method: {not_builtin}"
        );
        assert!(
            !candidates.contains(&not_builtin.to_string()),
            "candidates contains non-builtin method: {not_builtin}"
        );
    }
}
