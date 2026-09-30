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

#[test]
fn fl_validates_auth_backend_before_reading_stdin() {
    // Unknown auth backend must fail fast, before stdin is drained.
    let out = Command::new(FL_EXE)
        .args(["--auth-backend", "not-a-backend", "-"])
        .output()
        .expect("spawn fl");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown auth backend: not-a-backend"),
        "{stderr:?}"
    );

    // `--auth-backend emulator` without FIREBASE_AUTH_EMULATOR_HOST must also
    // fail immediately rather than hanging on stdin.
    let out = Command::new(FL_EXE)
        .args(["--auth-backend", "emulator", "-"])
        .env_remove("FIREBASE_AUTH_EMULATOR_HOST")
        .output()
        .expect("spawn fl");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("FIREBASE_AUTH_EMULATOR_HOST must be set"),
        "{stderr:?}"
    );
}

#[test]
fn fl_auth_in_memory_smoke_keeps_tokens_out_of_output() {
    // Default backend is the in-memory Auth stub, so the whole vertical slice
    // is reachable without any emulator running.
    let out = run_fl_script(
        "auth-vertical",
        "session = auth_sign_up(\"cli@example.com\", \"password\")\n\
         puts(session)\n\
         puts(auth_current_user()[\"email\"])\n\
         auth_sign_out()\n\
         auth_current_user()",
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("AuthSession"), "{stdout}");
    assert!(stdout.contains("id_token=<redacted>"), "{stdout}");
    assert!(stdout.contains("refresh_token=<redacted>"), "{stdout}");
    assert!(stdout.contains("cli@example.com"), "{stdout}");
    // After sign-out there is no current user, so the final result is nil.
    assert!(stdout.trim_end().ends_with("=> nil"), "{stdout}");

    // The fake token must never appear in stdout.
    assert!(!stdout.contains("fake-id-token"), "{stdout}");
    assert!(!stdout.contains("fake-refresh-token"), "{stdout}");
}

#[test]
fn fl_auth_error_is_reported_with_position() {
    let out = run_fl_script(
        "auth-error",
        "x = 1\nauth_sign_in(\"nobody@example.com\", \"password\")",
    );
    assert!(
        !out.status.success(),
        "failing auth script should exit nonzero"
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.starts_with("error: 2:1: "),
        "stderr should start with 'error: 2:1: ', got {stderr:?}"
    );
    assert!(stderr.contains("auth user not found"), "{stderr:?}");
}

#[test]
fn fl_auth_help_mentions_auth_backend() {
    let out = Command::new(FL_EXE)
        .arg("--help")
        .output()
        .expect("spawn fl");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--auth-backend"), "{stderr:?}");
    assert!(stderr.contains("FIREBASE_AUTH_EMULATOR_HOST"), "{stderr:?}");
}
