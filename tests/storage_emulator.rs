//! Storage emulator CRUD integration tests.
//!
//! These tests are skipped unless FIREBASE_STORAGE_EMULATOR_HOST is set by
//! `firebase emulators:exec`. The unit tests in `src/storage/emulator.rs`
//! cover the same REST contract with a fake transport when Firebase CLI is
//! unavailable.

use firelang::{run_script_with_storage, StorageEmulator, Value};

fn unique(prefix: &str) -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{prefix}_{n}")
}

#[test]
fn storage_emulator_crud_and_lossless_binary_round_trip() {
    let Some(storage) = StorageEmulator::from_env() else {
        eprintln!("skip: FIREBASE_STORAGE_EMULATOR_HOST is not set");
        return;
    };
    let path = format!("firelang-tests/{}", unique("crud"));
    let script = format!(
        r#"
payload = bytes([0, 255, 128, 10, 0])
uploaded = storage_upload("{path}", payload, content_type: "application/octet-stream")
downloaded = storage_download("{path}")
meta = storage_metadata("{path}")
[downloaded, meta["name"], meta["content_type"], meta["size"]]
"#
    );
    let out = run_script_with_storage(&script, storage).unwrap();
    assert_eq!(
        out,
        Value::Array(vec![
            Value::Bytes(vec![0, 255, 128, 10, 0]),
            Value::Str(path.clone()),
            Value::Str("application/octet-stream".to_string()),
            Value::Int(5),
        ])
    );
}

#[test]
fn storage_emulator_delete_then_not_found() {
    let Some(storage) = StorageEmulator::from_env() else {
        eprintln!("skip: FIREBASE_STORAGE_EMULATOR_HOST is not set");
        return;
    };
    let path = format!("firelang-tests/{}", unique("delete"));
    let script = format!(
        r#"
storage_upload("{path}", bytes([1, 2, 3]), content_type: "application/octet-stream")
storage_delete("{path}")
storage_download("{path}")
"#
    );
    let err = run_script_with_storage(&script, storage).unwrap_err();
    assert!(
        format!("{err}").contains("storage object not found"),
        "{err}"
    );
}
