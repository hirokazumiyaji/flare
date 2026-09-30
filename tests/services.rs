//! Service composition, pluggable providers, and capability-error contracts.

use std::error::Error;

use firelang::{
    run_script, run_script_with_firestore, run_script_with_services, Auth, AuthUser, ErrorKind,
    FirelangError, InMemoryAuth, InMemoryFirestore, InMemoryStorage, ProjectConfig, Runtime,
    RuntimeContext, Services, Storage, StorageObject, Value,
};

#[test]
fn default_runtime_uses_all_in_memory_services_without_an_emulator() {
    let out = run_script(
        r#"user = auth_sign_up("local@example.com", "secret")
storage_put("notes/a.txt", "hello", content_type: "text/plain")
[
  auth_current_user()["uid"] == user["uid"],
  storage_get("notes/a.txt"),
  storage_list("notes")[0]["content_type"],
]"#,
    )
    .unwrap();

    assert_eq!(
        out,
        Value::Array(vec![
            Value::Bool(true),
            Value::Str("hello".to_string()),
            Value::Str("text/plain".to_string()),
        ])
    );
}

struct MarkerAuth;

impl Auth for MarkerAuth {
    fn sign_in(&mut self, _email: &str, _password: &str) -> Result<AuthUser, Box<dyn Error>> {
        Ok(AuthUser::new("marker-auth-user").with_email("marker@example.com"))
    }

    fn sign_out(&mut self) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    fn current_user(&self) -> Result<Option<AuthUser>, Box<dyn Error>> {
        Ok(Some(AuthUser::new("marker-auth-user")))
    }
}

struct MarkerStorage;

impl Storage for MarkerStorage {
    fn put(&mut self, path: &str, data: Value) -> Result<StorageObject, Box<dyn Error>> {
        let mut value = match data {
            Value::Map(map) => map,
            other => {
                let mut map = std::collections::BTreeMap::new();
                map.insert("data".to_string(), other);
                map
            }
        };
        value.insert("backend".to_string(), Value::Str("marker".to_string()));
        Ok(StorageObject::new(path, Value::Map(value)))
    }

    fn get(&self, path: &str) -> Result<Option<StorageObject>, Box<dyn Error>> {
        Ok(Some(StorageObject::new(
            path,
            Value::Str("from-marker".to_string()),
        )))
    }

    fn delete(&mut self, _path: &str) -> Result<bool, Box<dyn Error>> {
        Ok(true)
    }

    fn list(&self, _prefix: &str) -> Result<Vec<StorageObject>, Box<dyn Error>> {
        Ok(vec![StorageObject::new(
            "marker/object",
            Value::Str("value".to_string()),
        )])
    }
}

#[test]
fn auth_and_storage_can_be_replaced_independently() {
    let services = Services::builder()
        .firestore(InMemoryFirestore::default())
        .auth(MarkerAuth)
        .storage(MarkerStorage)
        .build();
    let out = run_script_with_services(
        r#"set("users/1", {n: 1})
[
  get("users/1")["n"],
  auth_sign_in("marker@example.com", "secret")["uid"],
  storage_get("objects/a.txt"),
  storage_list()[0]["path"],
]"#,
        services,
    )
    .unwrap();

    assert_eq!(
        out,
        Value::Array(vec![
            Value::Int(1),
            Value::Str("marker-auth-user".to_string()),
            Value::Str("from-marker".to_string()),
            Value::Str("marker/object".to_string()),
        ])
    );
}

#[test]
fn a_service_can_be_configured_on_its_own() {
    let auth = InMemoryAuth::with_user(
        AuthUser::new("u1").with_email("alice@example.com"),
        "secret",
    );
    let auth_only = Services::builder().auth(auth).build();
    let out = run_script_with_services(
        r#"auth_sign_in("alice@example.com", "secret")["uid"]"#,
        auth_only,
    )
    .unwrap();
    assert_eq!(out, Value::Str("u1".to_string()));

    let storage_only = Services::builder()
        .storage(InMemoryStorage::default())
        .build();
    let out = run_script_with_services(
        r#"storage_put("a.txt", "b")
storage_get("a.txt")"#,
        storage_only,
    )
    .unwrap();
    assert_eq!(out, Value::Str("b".to_string()));
}

#[test]
fn missing_capabilities_produce_typed_errors_at_the_call_site() {
    for (source, service, operation) in [
        ("auth_current_user()", "auth", "current_user"),
        ("storage_get(\"a.txt\")", "storage", "get"),
        ("get(\"users/1\")", "firestore", "get"),
    ] {
        let err = run_script_with_services(source, Services::empty()).unwrap_err();
        let ferr = err
            .downcast_ref::<FirelangError>()
            .expect("capability failures are typed Firelang errors");
        assert_eq!(ferr.kind, ErrorKind::Capability);
        assert_eq!(ferr.span, Some(firelang::Span::new(1, 1)));
        assert!(
            ferr.message
                .contains(&format!("capability unavailable: {service} ({operation})")),
            "{ferr:?}"
        );
    }
}

#[test]
fn legacy_firestore_only_entry_point_still_works_and_has_no_implicit_auth() {
    let out = run_script_with_firestore(
        "set(\"users/legacy\", {n: 1})\nget(\"users/legacy\")[\"n\"]",
        InMemoryFirestore::default(),
    )
    .unwrap();
    assert_eq!(out, Value::Int(1));

    let err =
        run_script_with_firestore("auth_current_user()", InMemoryFirestore::default()).unwrap_err();
    let ferr = err.downcast_ref::<FirelangError>().unwrap();
    assert_eq!(ferr.kind, ErrorKind::Capability);
}

#[test]
fn runtime_context_keeps_config_and_service_state_without_globals() {
    let config = ProjectConfig::new("demo-services")
        .with_firestore_emulator_host("127.0.0.1:8080")
        .with_auth_emulator_host("127.0.0.1:9099")
        .with_storage_emulator_host("127.0.0.1:9199")
        .with_request_timeout(std::time::Duration::from_secs(3));
    let context = RuntimeContext::with_config(config, Services::in_memory());

    // The bare `Runtime` type uses the compatibility default and does not need
    // one generic parameter per configured service.
    let mut first: Runtime = Runtime::from_context(context);
    assert_eq!(first.project_config().project_id(), "demo-services");
    assert_eq!(
        first.project_config().auth_emulator_host.as_deref(),
        Some("127.0.0.1:9099")
    );
    let user = first.run("auth_sign_in_anonymously()[\"uid\"]").unwrap();
    assert!(matches!(user, Value::Str(_)));

    let mut second: Runtime = Runtime::from_context(RuntimeContext::in_memory());
    assert_eq!(
        second.run("auth_current_user()").unwrap(),
        Value::Nil,
        "a separate context must not observe another context's session"
    );
    assert_eq!(
        first.run("auth_current_user()[\"uid\"]").unwrap(),
        user,
        "state owned by one context survives across script runs"
    );
}

#[test]
fn explicit_runtime_type_and_constructor_remain_source_compatible() {
    let mut runtime: Runtime<InMemoryFirestore> = Runtime::new(InMemoryFirestore::default());
    let out = runtime
        .run("set(\"users/1\", {n: 7})\nget(\"users/1\")[\"n\"]")
        .unwrap();
    assert_eq!(out, Value::Int(7));
}

#[test]
fn emulator_mode_adapters_are_drop_in_capability_implementations() {
    let auth = firelang::AuthEmulator::new("demo").with_user(
        AuthUser::new("emu-user").with_email("emu@example.com"),
        "secret",
    );
    let storage = firelang::StorageEmulator::new("demo");
    let out = run_script_with_services(
        r#"auth_sign_in("emu@example.com", "secret")["uid"]
storage_put("emu/a", "ok")"#,
        Services::builder().auth(auth).storage(storage).build(),
    );
    assert!(out.is_ok(), "{out:?}");
}

#[test]
fn storage_rejects_firestore_only_values() {
    for (source, expected) in [
        (
            "storage_put(\"a\", server_timestamp())",
            "storage_put cannot store Firestore field transforms",
        ),
        (
            "storage_put(\"a\", doc(\"users/1\"))",
            "storage_put cannot store a reference or function value",
        ),
    ] {
        let err = run_script_with_services(
            source,
            Services::builder()
                .storage(InMemoryStorage::default())
                .build(),
        )
        .unwrap_err();
        assert!(format!("{err}").contains(expected), "{source}: {err}");
    }
}

#[test]
fn storage_listing_uses_directory_boundaries() {
    let services = Services::builder()
        .storage(InMemoryStorage::default())
        .build();
    let out = run_script_with_services(
        r#"storage_put("images/a.png", "a");
storage_put("images/nested/b.png", "b");
storage_put("images-old/c.png", "c");
storage_list("images").map { |item| item["path"] }"#,
        services,
    )
    .unwrap();
    assert_eq!(
        out,
        Value::Array(vec![
            Value::Str("images/a.png".to_string()),
            Value::Str("images/nested/b.png".to_string()),
        ])
    );
}
