use std::cell::RefCell;
use std::error::Error;
use std::rc::Rc;
use std::time::Duration;

use flare::value::QueryRef;
use flare::{
    firestore::{QueryCandidates, WriteOp},
    run_script_with_services, run_script_with_services_and_limits, CapabilityError, Firestore,
    InMemoryFirestore, InMemoryStorage, ProjectConfig, Runtime, RuntimeLimits, ServiceId, Services,
    Storage, Value,
};

#[derive(Default)]
struct RecordingFirestore {
    inner: RefCell<InMemoryFirestore>,
    reads: Rc<core::cell::Cell<usize>>,
    writes: Rc<core::cell::Cell<usize>>,
}

impl Firestore for RecordingFirestore {
    fn get(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>> {
        self.reads.set(self.reads.get() + 1);
        self.inner.borrow().get(path)
    }

    fn apply(&mut self, writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>> {
        self.writes.set(self.writes.get() + 1);
        self.inner.borrow_mut().apply(writes)
    }

    fn run_query(&self, query: &QueryRef) -> Result<QueryCandidates, Box<dyn Error>> {
        self.inner.borrow().run_query(query)
    }

    fn begin_transaction(&mut self) -> Result<(), Box<dyn Error>> {
        self.inner.borrow_mut().begin_transaction()
    }

    fn transaction_get(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>> {
        self.inner.borrow().transaction_get(path)
    }

    fn commit_transaction(&mut self, writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>> {
        self.inner.borrow_mut().commit_transaction(writes)
    }

    fn abort_transaction(&mut self) -> Result<(), Box<dyn Error>> {
        self.inner.borrow_mut().abort_transaction()
    }
}

#[test]
fn services_combine_independent_backends_and_keep_borrowed_storage() {
    let reads = Rc::new(core::cell::Cell::new(0));
    let writes = Rc::new(core::cell::Cell::new(0));
    let firestore = RecordingFirestore {
        inner: RefCell::default(),
        reads: Rc::clone(&reads),
        writes: Rc::clone(&writes),
    };
    let mut storage = InMemoryStorage::default();
    let services = Services::in_memory()
        .with_firestore(firestore)
        .with_storage(&mut storage);
    let mut runtime: Runtime<RecordingFirestore, &mut InMemoryStorage> =
        Runtime::from_services(services);

    let result = runtime
        .run(
            r#"
set("users/alice", {name: "Alice"})
auth_sign_up("alice@example.com", "secret1")
storage_upload("avatars/alice", bytes([1, 2]))
[get("users/alice")["name"], storage_download("avatars/alice")]
"#,
        )
        .unwrap();

    assert_eq!(
        result,
        Value::Array(vec![Value::Str("Alice".into()), Value::Bytes(vec![1, 2])])
    );
    assert_eq!(
        runtime.auth().current_user().unwrap().unwrap().email,
        "alice@example.com"
    );
    assert_eq!(reads.get(), 1);
    assert_eq!(writes.get(), 1);
    drop(runtime);
    assert_eq!(storage.download("avatars/alice").unwrap(), vec![1, 2]);
}

#[test]
fn services_entry_points_run_scripts_with_and_without_explicit_limits() {
    let _: Runtime<InMemoryFirestore> = Runtime::from_services(Services::in_memory());
    let services = Services::in_memory();
    assert_eq!(
        run_script_with_services(
            "set(\"users/a\", {n: 1})\nget(\"users/a\")[\"n\"]",
            services
        )
        .unwrap(),
        Value::Int(1)
    );

    assert_eq!(
        run_script_with_services_and_limits(
            "set(\"users/a\", {n: 2})\nget(\"users/a\")[\"n\"]",
            Services::in_memory(),
            RuntimeLimits::default(),
        )
        .unwrap(),
        Value::Int(2)
    );
}

#[test]
fn services_can_replace_firestore_and_auth_independently() {
    let services = Services::in_memory()
        .with_firestore(InMemoryFirestore::default())
        .with_auth(flare::InMemoryAuth::default());

    assert_eq!(
        run_script_with_services("auth_current_user()", services).unwrap(),
        Value::Nil
    );
}

#[test]
fn runtime_keeps_auth_session_and_resets_usage_across_runs() {
    let mut runtime = Runtime::from_services(Services::in_memory());
    runtime
        .run("auth_sign_up(\"alice@example.com\", \"secret1\")")
        .unwrap();

    assert_eq!(
        runtime.run("auth_current_user()[\"email\"]").unwrap(),
        Value::Str("alice@example.com".into())
    );
    assert_eq!(runtime.usage().steps, 1);
    runtime.run("1").unwrap();
    assert_eq!(runtime.usage().steps, 1);
}

#[test]
fn unavailable_services_report_typed_errors_with_source_spans() {
    for (source, service) in [
        ("set(\"users/a\", {n: 1})", ServiceId::Firestore),
        ("auth_current_user()", ServiceId::Auth),
        ("storage_download(\"avatars/a\")", ServiceId::Storage),
        ("col(\"users\").get()", ServiceId::Firestore),
        ("col(\"users\").count()", ServiceId::Firestore),
        ("File.read(\"x\")", ServiceId::IO),
        ("File.write(\"x\", \"y\")", ServiceId::IO),
        ("IO.File.read(\"x\")", ServiceId::IO),
        ("IO.File.write(\"x\", \"y\")", ServiceId::IO),
    ] {
        let err = run_script_with_services(source, Services::empty(ProjectConfig::default()))
            .unwrap_err();
        let capability = err
            .downcast_ref::<CapabilityError>()
            .expect("capability error remains downcastable");
        assert_eq!(capability.service, service, "{source}: {err}");
        assert_eq!(
            capability.span,
            Some(flare::Span::new(1, 1)),
            "{source}: {err}"
        );
        assert_eq!(
            err.to_string(),
            format!("1:1: service unavailable: {service}")
        );
    }
}

#[test]
fn unavailable_auth_and_storage_do_not_evaluate_side_effecting_arguments() {
    let auth_writes = Rc::new(core::cell::Cell::new(0));
    let firestore = RecordingFirestore {
        inner: RefCell::default(),
        reads: Rc::new(core::cell::Cell::new(0)),
        writes: Rc::clone(&auth_writes),
    };
    let err = run_script_with_services(
        "auth_sign_up(set(\"users/a\", {n: 1}), \"secret1\")",
        Services::empty(ProjectConfig::default()).with_firestore(firestore),
    )
    .unwrap_err();
    assert_eq!(
        err.downcast_ref::<CapabilityError>().unwrap().service,
        ServiceId::Auth
    );
    assert_eq!(auth_writes.get(), 0);

    let storage_writes = Rc::new(core::cell::Cell::new(0));
    let firestore = RecordingFirestore {
        inner: RefCell::default(),
        reads: Rc::new(core::cell::Cell::new(0)),
        writes: Rc::clone(&storage_writes),
    };
    let err = run_script_with_services(
        "storage_upload(set(\"users/a\", {n: 1}), bytes([1]))",
        Services::empty(ProjectConfig::default()).with_firestore(firestore),
    )
    .unwrap_err();
    assert_eq!(
        err.downcast_ref::<CapabilityError>().unwrap().service,
        ServiceId::Storage
    );
    assert_eq!(storage_writes.get(), 0);

    let err = run_script_with_services(
        "set(\"users/a\", auth_sign_up(\"nested@example.com\", \"secret1\"))",
        Services::empty(ProjectConfig::default()),
    )
    .unwrap_err();
    assert_eq!(
        err.downcast_ref::<CapabilityError>().unwrap().service,
        ServiceId::Firestore
    );
}

#[test]
fn firestore_references_and_query_builders_do_not_require_a_backend() {
    let services = Services::empty(ProjectConfig::default());
    assert!(matches!(
        run_script_with_services("col(\"users\").where(\"active\", \"==\", true)", services)
            .unwrap(),
        Value::QueryRef(_)
    ));

    assert_eq!(
        run_script_with_services(
            "server_timestamp()",
            Services::empty(ProjectConfig::default()),
        )
        .unwrap(),
        Value::ServerTimestamp
    );
}

#[test]
fn runtime_reports_timeout_support_for_selected_services() {
    let mut runtime = Runtime::from_services_with_limits(
        Services::in_memory(),
        RuntimeLimits {
            backend_timeout: Some(Duration::from_millis(123)),
            ..RuntimeLimits::default()
        },
    );
    assert!(!runtime.service_backend_timeout_supported(ServiceId::Auth));
    assert!(!runtime.service_backend_timeout_supported(ServiceId::Storage));
    assert_eq!(runtime.project_config(), &ProjectConfig::default());
    runtime.set_limits(RuntimeLimits {
        backend_timeout: None,
        ..RuntimeLimits::default()
    });
}

#[test]
fn boxed_firestore_preserves_service_hooks_and_query_methods() {
    let services = Services::in_memory().with_firestore(Box::new(InMemoryFirestore::default()));
    assert_eq!(
        run_script_with_services(
            "set(\"users/a\", {n: 1})\nget(\"users/a\")[\"n\"]",
            services
        )
        .unwrap(),
        Value::Int(1)
    );
}
