//! Smoke tests for the `fl` binary.

use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

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

/// Poll until the child exits or the deadline passes.
///
/// Note: stdout/stderr are still piped and undrained while polling, so a child
/// writing more than the pipe buffer (~64 KiB) before exiting would block on
/// the write and surface as a spurious timeout. Every test here asserts on
/// small, bounded diagnostics from a process that is expected to exit
/// immediately, so that ceiling is not reached in practice.
fn wait_for_exit(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("poll fl") {
            return Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn read_child_output(child: &mut Child) -> std::process::Output {
    use std::io::Read;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(mut pipe) = child.stdout.take() {
        pipe.read_to_end(&mut stdout).expect("read fl stdout");
    }
    if let Some(mut pipe) = child.stderr.take() {
        pipe.read_to_end(&mut stderr).expect("read fl stderr");
    }
    std::process::Output {
        status: child.wait().expect("wait fl"),
        stdout,
        stderr,
    }
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
fn fl_rejects_empty_emulator_host_before_reading_stdin() {
    let (status, out) = reject_emulator_config_before_stdin(
        "FIRESTORE_EMULATOR_HOST must not be empty",
        &[("FIRESTORE_EMULATOR_HOST", "")],
    );
    assert!(!status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("FIRESTORE_EMULATOR_HOST must not be empty"),
        "{stderr:?}"
    );
}

#[test]
fn fl_rejects_empty_emulator_project_id_before_reading_stdin() {
    // A valid host is required, otherwise the missing-host error would fire
    // first and the project-id message would never be reached.
    let (status, out) = reject_emulator_config_before_stdin(
        "FIRESTORE_PROJECT_ID must not be empty",
        &[
            ("FIRESTORE_EMULATOR_HOST", "127.0.0.1:8080"),
            ("FIRESTORE_PROJECT_ID", ""),
        ],
    );
    assert!(!status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("FIRESTORE_PROJECT_ID must not be empty"),
        "{stderr:?}"
    );
}

/// Run `fl --backend emulator -` with exactly the given emulator environment
/// and stdin held open, and require the process to exit without reading stdin.
/// Returns the exit status and captured output.
fn reject_emulator_config_before_stdin(
    expected: &str,
    env: &[(&str, &str)],
) -> (ExitStatus, std::process::Output) {
    let mut command = Command::new(FL_EXE);
    command.args(["--backend", "emulator", "-"]);
    // `env_remove` first so an inherited variable cannot decide which
    // misconfiguration is reported.
    for (key, _) in env {
        command.env_remove(key);
    }
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn fl");
    let _stdin = child.stdin.take().expect("keep stdin open");
    let status = wait_for_exit(&mut child, Duration::from_secs(2))
        .unwrap_or_else(|| panic!("fl read stdin before rejecting: {expected}"));
    let out = read_child_output(&mut child);
    (status, out)
}

#[cfg(unix)]
#[test]
fn fl_rejects_non_utf8_emulator_host_before_reading_stdin() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let host = OsString::from_vec(vec![0xff]);
    let mut child = Command::new(FL_EXE)
        .args(["--backend", "emulator", "-"])
        .env("FIRESTORE_EMULATOR_HOST", host)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn fl");
    let _stdin = child.stdin.take().expect("keep stdin open");
    let status = wait_for_exit(&mut child, Duration::from_secs(2))
        .expect("fl read stdin before rejecting a non-UTF-8 emulator host");
    assert!(!status.success());
    let out = read_child_output(&mut child);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("FIRESTORE_EMULATOR_HOST must be valid UTF-8"),
        "{stderr:?}"
    );
}
