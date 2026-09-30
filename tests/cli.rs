//! Smoke tests for the `fl` binary.

use std::process::Command;

const FL_EXE: &str = env!("CARGO_BIN_EXE_fl");

#[test]
fn fl_help_exits_zero() {
    let status = Command::new(FL_EXE)
        .args(["--help"])
        .status()
        .expect("spawn fl");
    assert!(status.success(), "fl --help should exit 0");
}

#[test]
fn fl_unknown_backend_fails() {
    let status = Command::new(FL_EXE)
        .args(["--backend", "not-a-backend", "examples/scripts/01_basic.fl"])
        .status()
        .expect("spawn fl");
    assert!(!status.success(), "invalid --backend should fail");
}

fn run_fl_script(name: &str, source: &str) -> std::process::Output {
    let path = std::env::temp_dir().join(format!("fl-test-{}-{}.fl", std::process::id(), name));
    std::fs::write(&path, source).expect("write temp script");
    let out = Command::new(FL_EXE).arg(&path).output().expect("spawn fl");
    let _ = std::fs::remove_file(&path);
    out
}

#[test]
fn fl_final_result_uses_inspect() {
    let out = run_fl_script("inspect", "{b: 2, a: [1, \"x\"]}");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "=> {\"a\" => [1, \"x\"], \"b\" => 2}\n"
    );
    let out = run_fl_script("inspect-str", "\"hi\"");
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "=> \"hi\"\n");
}

#[test]
fn fl_puts_uses_to_s() {
    let out = run_fl_script("puts", "puts(\"hi\")\nputs([1, \"x\"])\nputs(nil)");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "hi\n[1, \"x\"]\nnil\n=> nil\n"
    );
}

#[test]
fn fl_error_goes_to_stderr_with_position() {
    let out = run_fl_script("error", "a = 1\nundefined_var_here");
    assert!(!out.status.success(), "failing script should exit nonzero");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.starts_with("error: 2:1: "),
        "stderr should start with 'error: 2:1: ', got {stderr:?}"
    );
    assert!(stderr.contains("undefined variable"), "{stderr:?}");
}

fn run_fl_with_piped_stdin(args: &[&str], stdin_text: &str) -> std::process::Output {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = Command::new(FL_EXE)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn fl");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(stdin_text.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("wait fl")
}

#[test]
fn fl_inline_script_with_dash_e() {
    let out = Command::new(FL_EXE)
        .args(["-e", "1 + 2"])
        .output()
        .expect("spawn fl");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "=> 3\n");
    // -e のエラーも位置付きで stderr へ。
    let out = Command::new(FL_EXE)
        .args(["-e", "undefined_var_here"])
        .output()
        .expect("spawn fl");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.starts_with("error: 1:1: "), "{stderr:?}");
}

#[test]
fn fl_dash_reads_script_from_stdin() {
    let out = run_fl_with_piped_stdin(&["-"], "puts(\"hi\")\nputs(1 + 1)");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hi\n2\n=> nil\n");
}

#[test]
fn fl_no_args_with_piped_stdin_reads_stdin() {
    let out = run_fl_with_piped_stdin(&[], "[1, 2].len()");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "=> 2\n");
}

#[test]
fn fl_rejects_combined_script_sources() {
    let path = std::env::temp_dir().join(format!("fl-test-{}-combo.fl", std::process::id()));
    std::fs::write(&path, "1").expect("write temp script");
    let out = Command::new(FL_EXE)
        .args(["-e", "1", path.to_str().expect("utf8 path")])
        .output()
        .expect("spawn fl");
    let _ = std::fs::remove_file(&path);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("-e cannot be combined"), "{stderr:?}");
}

#[test]
fn fl_inline_script_accepts_help_argument_without_triggering_usage() {
    // -e の値が "-h" の場合、Usage 表示ではなくスクリプトとして実行され、
    // 未定義変数 h の評価エラーになること(exit 1)。
    let out = Command::new(FL_EXE)
        .args(["-e", "-h"])
        .output()
        .expect("spawn fl");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("undefined variable: h"),
        "expected undefined variable error, got {stderr:?}"
    );

    // 有効なスクリプトで -h を含む場合、正常に評価されること
    let out = Command::new(FL_EXE)
        .args(["-e", "h = 5; -h"])
        .output()
        .expect("spawn fl");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "=> -5\n");
}

#[test]
fn fl_rejects_non_numeric_budget_values() {
    let out = Command::new(FL_EXE)
        .args(["--max-steps", "abc", "-e", "1"])
        .output()
        .expect("spawn fl");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--max-steps requires a non-negative integer"),
        "{stderr:?}"
    );
}

#[test]
fn fl_rejects_budget_option_without_a_value() {
    // No value at all: reported before any script-source validation.
    let out = Command::new(FL_EXE)
        .args(["--max-call-depth"])
        .output()
        .expect("spawn fl");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--max-call-depth requires a number"),
        "{stderr:?}"
    );

    // A following option is reported as a bad value rather than silently
    // consumed as the budget.
    let out = Command::new(FL_EXE)
        .args(["--max-call-depth", "-e", "1"])
        .output()
        .expect("spawn fl");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--max-call-depth requires a non-negative integer"),
        "{stderr:?}"
    );
}

#[test]
fn fl_default_budgets_stop_infinite_recursion() {
    // A runaway script must exit promptly with a positioned error instead of
    // blowing the stack (issue #82).
    let started = std::time::Instant::now();
    let out = Command::new(FL_EXE)
        .args(["-e", "fn f() { f() } f()"])
        .output()
        .expect("spawn fl");
    assert!(!out.status.success());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "fl should not hang on runaway recursion"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("call depth limit exceeded"), "{stderr:?}");
    // The position of the failing top-level statement (`f()`), not of the fn.
    assert!(stderr.starts_with("error: 1:16: "), "{stderr:?}");
}

#[test]
fn fl_default_budgets_stop_infinite_loops() {
    let started = std::time::Instant::now();
    let out = Command::new(FL_EXE)
        .args(["-e", "while true { }"])
        .output()
        .expect("spawn fl");
    assert!(!out.status.success());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "fl should not hang on an infinite loop"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("loop iteration limit exceeded"),
        "{stderr:?}"
    );
}

#[test]
fn fl_budget_options_override_defaults() {
    // A tiny budget that the default would never hit.
    let out = Command::new(FL_EXE)
        .args(["--max-steps", "1", "-e", "a = 1\nb = 2"])
        .output()
        .expect("spawn fl");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("step limit exceeded"), "{stderr:?}");

    // Raising a limit above the default still works for legitimate scripts.
    let out = Command::new(FL_EXE)
        .args([
            "--max-call-depth",
            "64",
            "-e",
            "fn f(n) { if n <= 0 { 0 } else { f(n - 1) } } f(20)",
        ])
        .output()
        .expect("spawn fl");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "=> 0\n");
}

#[test]
fn fl_unlimited_is_an_explicit_opt_in() {
    // `--unlimited` on its own removes the guards; a later per-budget option
    // re-enables just that one budget.
    let out = Command::new(FL_EXE)
        .args([
            "--unlimited",
            "-e",
            "fn f(n) { if n <= 0 { 0 } else { f(n - 1) } } f(32)",
        ])
        .output()
        .expect("spawn fl");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "=> 0\n");

    let out = Command::new(FL_EXE)
        .args([
            "--unlimited",
            "--max-call-depth",
            "16",
            "-e",
            "fn f(n) { if n <= 0 { 0 } else { f(n - 1) } } f(32)",
        ])
        .output()
        .expect("spawn fl");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("call depth limit exceeded"), "{stderr:?}");
}

#[test]
fn fl_validates_backend_before_reading_stdin() {
    // 未知のバックエンド指定で stdin を要求した場合、stdin を読み取ることなく即時失敗すること
    let out = Command::new(FL_EXE)
        .args(["--backend", "unknown-backend", "-"])
        .output()
        .expect("spawn fl");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown backend: unknown-backend"),
        "{stderr:?}"
    );

    // FIRESTORE_EMULATOR_HOST 未設定で emulator バックエンド + stdin の場合も即時失敗すること
    let out = Command::new(FL_EXE)
        .args(["--backend", "emulator", "-"])
        .env_remove("FIRESTORE_EMULATOR_HOST")
        .output()
        .expect("spawn fl");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("FIRESTORE_EMULATOR_HOST must be set"),
        "{stderr:?}"
    );
}
