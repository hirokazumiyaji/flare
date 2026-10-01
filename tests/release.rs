//! Tests for the release gate and release archive contract.

#[cfg(unix)]
mod unix {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock should be after the Unix epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "firelang-release-test-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    fn run_script(name: &str, args: &[&str]) -> Output {
        let root = repo_root();
        Command::new(root.join("scripts").join(name))
            .args(args)
            .current_dir(root)
            .output()
            .unwrap_or_else(|error| panic!("failed to run {name}: {error}"))
    }

    fn assert_success(output: &Output) {
        assert!(
            output.status.success(),
            "command failed: {}\nstdout: {}\nstderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn checksum_check(directory: &Path, manifest: &str) -> Output {
        let output = Command::new("sha256sum").args(["--version"]).output();
        if output.is_ok_and(|result| result.status.success()) {
            Command::new("sha256sum")
                .args(["-c", manifest])
                .current_dir(directory)
                .output()
                .expect("run sha256sum")
        } else {
            Command::new("shasum")
                .args(["-a", "256", "-c", manifest])
                .current_dir(directory)
                .output()
                .expect("run shasum")
        }
    }

    #[test]
    fn release_tag_must_match_cargo_metadata() {
        let tag = format!("v{}", env!("CARGO_PKG_VERSION"));
        let output = run_script("validate-release-tag.sh", &[&tag]);
        assert_success(&output);
        assert!(String::from_utf8_lossy(&output.stdout).contains("matches Cargo package version"));

        for invalid in ["v", "release", "v1.2", "v1.2.3-rc1", "v../1.2.3"] {
            let output = run_script("validate-release-tag.sh", &[invalid]);
            assert!(!output.status.success(), "{invalid} should be rejected");
            assert!(String::from_utf8_lossy(&output.stderr)
                .contains("release tag must have the form vX.Y.Z"));
        }

        let mismatch = if env!("CARGO_PKG_VERSION") == "0.0.0" {
            "v0.0.1"
        } else {
            "v0.0.0"
        };
        let output = run_script("validate-release-tag.sh", &[mismatch]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr)
            .contains("does not match Cargo package version"));
    }

    #[test]
    fn release_package_rejects_non_semver_tags_before_creating_paths() {
        let temp = TestDir::new();
        let output_dir = temp.path().join("dist");
        let binary = PathBuf::from(env!("CARGO_BIN_EXE_fl"));
        let output = run_script(
            "package-release.sh",
            &[
                "v../1.2.3",
                binary.to_str().expect("binary path should be UTF-8"),
                output_dir.to_str().expect("output path should be UTF-8"),
            ],
        );
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr)
            .contains("release package tag must have the form vX.Y.Z"));
        assert!(!output_dir.exists(), "invalid tag must not create output paths");
    }

    #[test]
    fn release_archive_contains_executable_docs_license_and_checksums() {
        let temp = TestDir::new();
        let tag = format!("v{}", env!("CARGO_PKG_VERSION"));
        let output_dir = temp.path().join("dist");
        let binary = PathBuf::from(env!("CARGO_BIN_EXE_fl"));
        let output = run_script(
            "package-release.sh",
            &[
                &tag,
                binary.to_str().expect("binary path should be UTF-8"),
                output_dir.to_str().expect("output path should be UTF-8"),
            ],
        );
        assert_success(&output);

        let version = env!("CARGO_PKG_VERSION");
        let package_name = format!("firelang-{version}-linux-x86_64");
        let archive = output_dir.join(format!("{package_name}.tar.gz"));
        let archive_checksum = output_dir.join(format!("{package_name}.tar.gz.sha256"));
        assert!(archive.is_file(), "release archive was not created");
        assert!(
            archive_checksum.is_file(),
            "archive checksum was not created"
        );

        let listing = Command::new("tar")
            .args([
                "-tvzf",
                archive.to_str().expect("archive path should be UTF-8"),
            ])
            .output()
            .expect("list release archive");
        assert_success(&listing);
        let listing = String::from_utf8_lossy(&listing.stdout);
        for required in ["README.md", "LICENSE", "docs/spec.md", "SHA256SUMS"] {
            assert!(
                listing.contains(&format!("{package_name}/{required}")),
                "release archive is missing {required}: {listing}"
            );
        }
        let executable_line = listing
            .lines()
            .find(|line| line.ends_with("/fl"))
            .expect("release archive should contain fl");
        assert!(
            executable_line.starts_with("-rwxr-xr-x"),
            "fl should be executable in the archive: {executable_line}"
        );

        let extracted = temp.path().join("extracted");
        fs::create_dir(&extracted).expect("create extraction directory");
        let extraction = Command::new("tar")
            .args([
                "-xzf",
                archive.to_str().expect("archive path should be UTF-8"),
                "-C",
                extracted.to_str().expect("extracted path should be UTF-8"),
            ])
            .output()
            .expect("extract release archive");
        assert_success(&extraction);

        let package_dir = extracted.join(&package_name);
        for required in ["fl", "README.md", "LICENSE", "docs/spec.md", "SHA256SUMS"] {
            assert!(
                package_dir.join(required).is_file(),
                "extracted release is missing {required}"
            );
        }
        let mode = fs::metadata(package_dir.join("fl"))
            .expect("read extracted executable metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode & 0o111, 0o111, "extracted fl is not executable");

        assert_success(&checksum_check(&package_dir, "SHA256SUMS"));
        assert_success(&checksum_check(
            &output_dir,
            archive_checksum
                .file_name()
                .and_then(|name| name.to_str())
                .expect("checksum path should be UTF-8"),
        ));
    }
}
