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
