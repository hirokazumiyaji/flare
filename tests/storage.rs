//! Storage script API and backend contract tests.

use std::cell::RefCell;

use flare::{
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
            "duplicate argument content_type",
        ),
        ("storage_delete(\"missing\")", "storage object not found"),
    ] {
        let err = run_script(script).unwrap_err();
        assert!(format!("{err}").contains(expected), "{script}: {err}");
    }
}

#[test]
fn firestore_accepts_bytes_fields() {
    // `Bytes` is a first-class Firestore value: writes and query filters
    // accept it on both backends.
    let out =
        run_script("set(\"users/a\", {data: bytes([1])})\nget(\"users/a\")[\"data\"]").unwrap();
    assert_eq!(out, Value::Bytes(vec![1]));
    let out = run_script(
        "set(\"users/a\", {data: bytes([1])})\ncol(\"users\").where(\"data\", \"==\", bytes([1])).get().len()",
    )
    .unwrap();
    assert_eq!(out, Value::Int(1));
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
        flare::runtime::Runtime::with_storage(flare::InMemoryFirestore::default(), storage);
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

#[test]
fn storage_emulator_rejects_response_exceeding_max_bytes_budget() {
    struct HugeTransport;
    impl StorageTransport for HugeTransport {
        fn send(
            &self,
            _req: &StorageRequest,
        ) -> Result<StorageResponse, Box<dyn std::error::Error>> {
            Ok(StorageResponse::new(200, vec![0x42; 200]))
        }
    }
    let storage = StorageEmulator::with_transport("p", HugeTransport).unwrap();
    let limits = flare::limits::RuntimeLimits {
        max_bytes_size: Some(100),
        ..flare::limits::RuntimeLimits::default()
    };
    let mut runtime = flare::runtime::Runtime::with_storage_and_limits(
        flare::InMemoryFirestore::default(),
        storage,
        limits,
    );
    let err = runtime
        .run("storage_download(\"file.bin\")")
        .unwrap_err()
        .to_string();
    assert!(err.contains("response body size exceeds limit"), "{err}");
}

#[test]
fn storage_emulator_with_unlimited_runtime_limits_allows_large_response() {
    struct HugeTransport;
    impl StorageTransport for HugeTransport {
        fn send(
            &self,
            _req: &StorageRequest,
        ) -> Result<StorageResponse, Box<dyn std::error::Error>> {
            Ok(StorageResponse::new(200, vec![0x42; 60 * 1024 * 1024]))
        }
    }
    let storage = StorageEmulator::with_transport("p", HugeTransport).unwrap();
    let mut runtime = flare::runtime::Runtime::with_storage_and_limits(
        flare::InMemoryFirestore::default(),
        storage,
        flare::limits::RuntimeLimits::unlimited(),
    );
    let result = runtime.run("storage_download(\"file.bin\")").unwrap();
    match result {
        flare::value::Value::Bytes(bytes) => assert_eq!(bytes.len(), 60 * 1024 * 1024),
        other => panic!("expected bytes value, got {other:?}"),
    }
}

#[test]
fn runtime_set_limits_updates_storage_emulator_limits() {
    struct TestTransport;
    impl StorageTransport for TestTransport {
        fn send(
            &self,
            _req: &StorageRequest,
        ) -> Result<StorageResponse, Box<dyn std::error::Error>> {
            Ok(StorageResponse::new(200, vec![0x42; 500]))
        }
    }
    let storage = StorageEmulator::with_transport("p", TestTransport).unwrap();
    let mut runtime =
        flare::runtime::Runtime::with_storage(flare::InMemoryFirestore::default(), storage);

    // Initial default limits (50 MiB) allow 500 bytes.
    let result = runtime.run("storage_download(\"file.bin\")").unwrap();
    match result {
        flare::value::Value::Bytes(bytes) => assert_eq!(bytes.len(), 500),
        other => panic!("expected bytes value, got {other:?}"),
    }

    // Lowering limit via set_limits constrains the storage emulator.
    runtime.set_limits(flare::limits::RuntimeLimits {
        max_bytes_size: Some(100),
        ..flare::limits::RuntimeLimits::default()
    });
    let err = runtime
        .run("storage_download(\"file.bin\")")
        .unwrap_err()
        .to_string();
    assert!(err.contains("response body size exceeds limit"), "{err}");

    // Disabling limits via set_limits allows downloads of any size.
    runtime.set_limits(flare::limits::RuntimeLimits::unlimited());
    let result = runtime.run("storage_download(\"file.bin\")").unwrap();
    match result {
        flare::value::Value::Bytes(bytes) => assert_eq!(bytes.len(), 500),
        other => panic!("expected bytes value, got {other:?}"),
    }
}
