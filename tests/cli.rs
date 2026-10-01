//! Smoke tests for the `fl` binary.

use std::process::{Command, Stdio};

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

/// Create a temp script exclusively. The name is derived from the pid plus a
/// nanosecond stamp so concurrent runs do not collide, and `create_new` means a
/// pre-existing entry (notably a symlink) fails the test loudly instead of
/// being followed and truncated.
fn create_temp_script(name: &str, source: &[u8]) -> std::path::PathBuf {
    use std::io::Write;

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "fl-test-{}-{}-{name}.fl",
        std::process::id(),
        stamp
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap_or_else(|e| panic!("create temp script {}: {e}", path.display()));
    file.write_all(source)
        .unwrap_or_else(|e| panic!("write temp script {}: {e}", path.display()));
    drop(file);
    path
}

fn run_fl_script(name: &str, source: &str) -> std::process::Output {
    let path = create_temp_script(name, source.as_bytes());
    let out = Command::new(FL_EXE).arg(&path).output().expect("spawn fl");
    let _ = std::fs::remove_file(&path);
    out
}

// macOS/AFS rejects non-UTF-8 file names outright (`EILSEQ`), so the fixture
// can only be created on a file system that accepts arbitrary bytes. CI runs
// Linux, which is where this contract is actually exercised; on macOS the test
// would fail at setup no matter how `fl` behaves.
#[cfg(target_os = "linux")]
#[test]
fn fl_handles_script_with_non_utf8_path() {
    use std::ffi::OsString;
    use std::io::Write;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    // Same exclusive-creation contract as create_temp_script, but the name
    // carries a raw 0xff byte, so it cannot go through that helper's name
    // formatting.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_nanos();
    let base =
        std::env::temp_dir().join(format!("fl-test-{}-{}-non-utf8", std::process::id(), stamp));
    let mut path_bytes = base.as_os_str().as_bytes().to_vec();
    path_bytes.push(0xff);
    path_bytes.extend_from_slice(b".fl");
    let path = std::path::PathBuf::from(OsString::from_vec(path_bytes));
    // A setup failure must not silently downgrade the test: without the
    // fixture, the non-UTF-8 path contract would go untested.
    let mut script = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .expect("create non-UTF-8 temp script");
    script.write_all(b"1").expect("write non-UTF-8 temp script");
    drop(script);

    let out = Command::new(FL_EXE).arg(&path).output().expect("spawn fl");
    let _ = std::fs::remove_file(&path);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "fl should execute a non-UTF-8 path; status: {:?}, stderr: {stderr}",
        out.status
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "=> 1\n");
    assert!(!stderr.contains("panicked"), "stderr: {stderr:?}");
}

#[cfg(unix)]
#[test]
fn fl_rejects_non_utf8_option_values_without_panicking() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let value = OsString::from_vec(vec![0xff]);
    for (option, expected_error) in [
        ("--backend", "--backend value must be valid UTF-8"),
        ("-e", "-e value must be valid UTF-8"),
    ] {
        let out = Command::new(FL_EXE)
            .arg(option)
            .arg(&value)
            .output()
            .expect("spawn fl");
        let stderr = String::from_utf8_lossy(&out.stderr);

        assert_eq!(out.status.code(), Some(1), "{option}: {stderr:?}");
        assert!(stderr.contains(expected_error), "{option}: {stderr:?}");
        assert!(!stderr.contains("panicked"), "{option}: {stderr:?}");
    }
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
fn fl_evaluates_call_receiver_before_arguments() {
    let source = r#"
fn receiver() {
  puts "R"
  timestamp(0)
}
fn argument() {
  puts "A"
  1
}
receiver().add_seconds(argument())
"#;
    let out = run_fl_script("call-order", source);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "R\nA\n=> Timestamp(1)\n"
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

#[test]
fn fl_reports_builtin_keyword_contract_errors_with_position() {
    let out = run_fl_script(
        "builtin-contract-keyword",
        "set(\"users/1\", {old: 1})\nset(\"users/1\", {new: 2}, mearge: true)",
    );
    assert!(!out.status.success(), "invalid builtin call should fail");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.starts_with("error: 2:1: "), "{stderr:?}");
    assert!(
        stderr.contains("unknown keyword arguments: mearge"),
        "{stderr:?}"
    );
}

#[test]
fn fl_reports_excess_arguments_and_disallowed_blocks() {
    let out = run_fl_script("builtin-contract-args", "now(1)");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("too many positional arguments"));

    let out = run_fl_script("builtin-contract-block", "col(\"users\").get() {}");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("block"));
}

fn run_fl_with_piped_stdin(args: &[&str], stdin_text: &str) -> std::process::Output {
    use std::io::Write;
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
    let path = create_temp_script("combo", b"1");
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

#[cfg(not(feature = "emulator"))]
#[test]
fn fl_emulator_backend_reports_missing_feature() {
    let out = Command::new(FL_EXE)
        .args(["--backend", "emulator", "-e", "1"])
        .env("FIRESTORE_EMULATOR_HOST", "127.0.0.1:9")
        .output()
        .expect("spawn fl");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("emulator backend requires the `emulator` Cargo feature"),
        "stderr should explain how to enable the feature, got {stderr:?}"
    );
    assert!(stderr.contains("--features emulator"), "stderr: {stderr:?}");
}

#[cfg(feature = "emulator")]
#[test]
fn fl_emulator_timeout_rejects_zero_and_malformed_values() {
    for value in ["0", "-1", "not-a-number"] {
        let out = Command::new(FL_EXE)
            .args([
                "--backend",
                "emulator",
                "--emulator-timeout-ms",
                value,
                "-e",
                "1",
            ])
            .env("FIRESTORE_EMULATOR_HOST", "127.0.0.1:1")
            .output()
            .expect("spawn fl");
        assert!(!out.status.success(), "timeout {value} should fail");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("emulator timeout must be")
                || stderr.contains("positive integer")
                || stderr.contains("greater than zero"),
            "timeout {value}: {stderr:?}"
        );
    }
}

#[cfg(feature = "emulator")]
#[test]
fn fl_timeout_environment_override_and_cli_precedence_are_observable() {
    let out = Command::new(FL_EXE)
        .args(["--backend", "emulator", "-e", "1"])
        .env("FIRESTORE_EMULATOR_HOST", "127.0.0.1:1")
        .env("FIRESTORE_EMULATOR_TIMEOUT_MS", "not-a-number")
        .output()
        .expect("spawn fl");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("emulator timeout must be") || stderr.contains("positive integer"),
        "malformed environment timeout should be reported: {stderr:?}"
    );

    // A valid CLI value takes precedence over the malformed environment value,
    // so execution proceeds to the emulator connection instead of failing while
    // resolving the timeout.
    let out = Command::new(FL_EXE)
        .args([
            "--backend",
            "emulator",
            "--emulator-timeout-ms",
            "25",
            "-e",
            "1",
        ])
        .env("FIRESTORE_EMULATOR_HOST", "127.0.0.1:1")
        .env("FIRESTORE_EMULATOR_TIMEOUT_MS", "not-a-number")
        .output()
        .expect("spawn fl");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("emulator timeout must be"),
        "CLI timeout should override the environment: {stderr:?}"
    );
}

#[cfg(feature = "emulator")]
#[test]
fn fl_timeout_alias_reports_the_alias_and_help_uses_the_default_constant() {
    let out = Command::new(FL_EXE)
        .args(["--backend", "emulator", "--timeout-ms"])
        .output()
        .expect("spawn fl");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--timeout-ms requires a value"),
        "{stderr:?}"
    );

    let out = Command::new(FL_EXE)
        .args(["--help"])
        .output()
        .expect("spawn fl");
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("per-RPC timeout (default: 30000ms)"),
        "{stderr:?}"
    );
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
}

// The emulator validation messages only exist in the `emulator` build: without
// the feature every `--backend emulator` invocation fails first with the
// feature error, so this assertion can only run there.
#[cfg(feature = "emulator")]
#[test]
fn fl_validates_emulator_backend_before_reading_stdin() {
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
fn fl_storage_backend_requires_storage_emulator_host() {
    let out = Command::new(FL_EXE)
        .args(["--storage-backend", "emulator", "-e", "1"])
        .env_remove("FIREBASE_STORAGE_EMULATOR_HOST")
        .output()
        .expect("spawn fl");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("FIREBASE_STORAGE_EMULATOR_HOST must be set"),
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

#[test]
fn fl_auto_storage_falls_back_when_emulator_is_unavailable() {
    let out = Command::new(FL_EXE)
        .args(["-e", "1"])
        .env("FIREBASE_STORAGE_EMULATOR_HOST", "127.0.0.1:1")
        .output()
        .expect("spawn fl");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "=> 1\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("using in-memory"));
}

#[test]
fn fl_in_memory_storage_crud_is_available_by_default() {
    let out = Command::new(FL_EXE)
        .args([
            "-e",
            "storage_upload(\"cli/a\", bytes([0, 255]))\nstorage_download(\"cli/a\")",
        ])
        .env_remove("FIREBASE_STORAGE_EMULATOR_HOST")
        .output()
        .expect("spawn fl");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "=> Bytes(00ff)\n",
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
