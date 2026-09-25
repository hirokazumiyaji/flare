//! Storage script API and backend contract tests.

use std::cell::RefCell;

use firelang::{
    run_script, run_script_with_storage, InMemoryStorage, Storage, StorageEmulator, StorageRequest,
    StorageResponse, StorageTransport, Value,
};

#[test]
fn in_memory_storage_crud_round_trips_binary_content() {
    let script = r#"
payload = bytes([0, 255, 128, 10])
uploaded = storage_upload("avatars/alice.bin", payload, content_type: "application/octet-stream")
downloaded = storage_download("avatars/alice.bin")
meta = storage_metadata("avatars/alice.bin")
[downloaded, meta["content_type"], meta["size"], len(downloaded), storage_delete("avatars/alice.bin"), storage_download("avatars/alice.bin")]
"#;
    let err = run_script(script).unwrap_err();
    // The final read is intentionally a not-found contract check.
    assert!(
        format!("{err}").contains("storage object not found"),
        "{err}"
    );

    let script = r#"
payload = bytes([0, 255, 128, 10])
storage_upload("avatars/alice.bin", payload, content_type: "application/octet-stream")
downloaded = storage_download("avatars/alice.bin")
meta = storage_metadata("avatars/alice.bin")
[downloaded, meta["content_type"], meta["size"], len(downloaded)]
"#;
    let out = run_script(script).unwrap();
    assert_eq!(
        out,
        Value::Array(vec![
            Value::Bytes(vec![0, 255, 128, 10]),
            Value::Str("application/octet-stream".to_string()),
            Value::Int(4),
            Value::Int(4),
        ])
    );
}

#[test]
fn storage_paths_and_content_have_clear_errors() {
    for (script, expected) in [
        (
            "storage_download(\"avatars//alice.png\")",
            "invalid storage object path",
        ),
        (
            "storage_upload(\"avatars/a\", bytes([1]), content_type: \"bad type\")",
            "invalid storage content type",
        ),
        (
            "storage_upload(\"avatars/a\", \"not bytes\", content_type: \"image/png\")",
            "bytes",
        ),
        (
            "storage_upload(\"avatars/a\", bytes([1]), \"text/plain\", content_type: \"image/png\")",
            "content_type twice",
        ),
        ("storage_delete(\"missing\")", "storage object not found"),
    ] {
        let err = run_script(script).unwrap_err();
        assert!(format!("{err}").contains(expected), "{script}: {err}");
    }
}

#[test]
fn firestore_keeps_its_bytes_fields_read_only() {
    for script in [
        "set(\"users/a\", {data: bytes([1])})",
        "set(\"users/a\", {data: array_union([bytes([1])])})",
        "set(\"users/a\", {data: array_remove([bytes([1])])})",
    ] {
        let err = run_script(script).unwrap_err();
        assert!(
            format!("{err}").contains("Bytes values are only supported by Storage"),
            "{script}: {err}"
        );
    }
}

#[test]
fn storage_can_be_selected_without_changing_firestore() {
    let mut storage = InMemoryStorage::default();
    let out = run_script_with_storage(
        "storage_upload(\"x\", bytes([1, 2]))\nstorage_download(\"x\")",
        &mut storage,
    )
    .unwrap();
    assert_eq!(out, Value::Bytes(vec![1, 2]));
    assert_eq!(storage.download("x").unwrap(), vec![1, 2]);
}

struct RecordingTransport {
    requests: RefCell<Vec<StorageRequest>>,
}

impl StorageTransport for RecordingTransport {
    fn send(
        &self,
        request: &StorageRequest,
    ) -> Result<StorageResponse, Box<dyn std::error::Error>> {
        self.requests.borrow_mut().push(request.clone());
        if request.method == "POST" {
            Ok(StorageResponse::new(
                200,
                br#"{"name":"x","bucket":"p.appspot.com","contentType":"application/octet-stream","size":2}"#,
            ))
        } else {
            Ok(StorageResponse::new(200, vec![1, 2]))
        }
    }
}

#[test]
fn fake_transport_can_drive_the_emulator_contract() {
    let transport = RecordingTransport {
        requests: RefCell::new(vec![]),
    };
    let storage = StorageEmulator::with_transport("p", transport).unwrap();
    let mut runtime =
        firelang::runtime::Runtime::with_storage(firelang::InMemoryFirestore::default(), storage);
    let out = runtime
        .run("storage_upload(\"x\", bytes([1, 2]))\nstorage_download(\"x\")")
        .unwrap();
    assert_eq!(out, Value::Bytes(vec![1, 2]));
    // The transport is owned by the runtime, so the fake's request log is
    // checked through a fresh direct client below rather than reaching into a
    // private Runtime field.
    let transport = RecordingTransport {
        requests: RefCell::new(vec![]),
    };
    let mut direct = StorageEmulator::with_transport("p", transport).unwrap();
    direct
        .upload("x", &[1, 2], "application/octet-stream")
        .unwrap();
    // This assertion mainly documents the public transport seam; the runtime
    // path above is the script-level vertical slice.
    assert_eq!(direct.transport().requests.borrow().len(), 1);
}
