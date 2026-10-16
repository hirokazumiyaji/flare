use flare::{
    run_script_with_services, BuiltinModule, CapabilityError, InMemoryAuth, InMemoryFirestore,
    InMemoryStorage, ProjectConfig, Runtime, ServiceId, Services, Value,
};

#[test]
fn firestore_auth_and_storage_modules_share_one_script_runtime() {
    let result = run_script_with_services(
        r#"
session = Auth.sign_up("alice@example.com", "secret1")
Firestore.set("users/alice", {email: session.user.email}, merge: true)
Storage.upload("avatars/alice", bytes([1, 2]), content_type: "application/octet-stream")
[
  Auth.current_user().email,
  Firestore.get("users/alice").email,
  Firestore.col("users").where("email", "==", "alice@example.com").get().len(),
  Storage.download("avatars/alice"),
  Storage.metadata("avatars/alice")["content_type"],
  Auth.sign_out()
]
"#,
        Services::in_memory(),
    )
    .unwrap();

    assert_eq!(
        result,
        Value::Array(vec![
            Value::Str("alice@example.com".into()),
            Value::Str("alice@example.com".into()),
            Value::Int(1),
            Value::Bytes(vec![1, 2]),
            Value::Str("application/octet-stream".into()),
            Value::Nil,
        ])
    );
}

#[test]
fn module_aliases_shadowing_and_function_calls_follow_name_resolution_rules() {
    let result = run_script_with_services(
        r#"
a = Auth
fn signed_in_email() { a.current_user().email }
Auth.sign_up("alice@example.com", "secret1")
signed_in_email()
"#,
        Services::in_memory(),
    )
    .unwrap();
    assert_eq!(result, Value::Str("alice@example.com".into()));

    assert_eq!(
        run_script_with_services("Auth = 3\nAuth", Services::in_memory()).unwrap(),
        Value::Int(3)
    );
    for m in [
        BuiltinModule::Firestore,
        BuiltinModule::Auth,
        BuiltinModule::Storage,
        BuiltinModule::IO,
        BuiltinModule::File,
        BuiltinModule::JSON,
        BuiltinModule::CSV,
        BuiltinModule::YAML,
    ] {
        assert_eq!(Value::Module(m).to_string(), m.name());
        assert_eq!(Value::Module(m).inspect(), m.name());
        assert!(Value::Module(m).truthy());
        assert_eq!(Value::Module(m), Value::Module(m));
    }
}

#[test]
fn module_global_names_share_handlers_and_call_contracts() {
    for (module_call, global_call, expected) in [
        (
            "Firestore.set(\"users/a\", {n: 1}, merge: true)\nFirestore.get(\"users/a\")[\"n\"]",
            "set(\"users/a\", {n: 1}, merge: true)\nget(\"users/a\")[\"n\"]",
            Value::Int(1),
        ),
        (
            "Storage.upload(\"x\", bytes([1]), content_type: \"application/octet-stream\")\nStorage.download(\"x\")",
            "storage_upload(\"x\", bytes([1]), content_type: \"application/octet-stream\")\nstorage_download(\"x\")",
            Value::Bytes(vec![1]),
        ),
        (
            "Firestore.batch { Firestore.set(\"users/a\", {n: 1}) }\nFirestore.get(\"users/a\")[\"n\"]",
            "batch { set(\"users/a\", {n: 1}) }\nget(\"users/a\")[\"n\"]",
            Value::Int(1),
        ),
    ] {
        assert_eq!(
            run_script_with_services(module_call, Services::in_memory()).unwrap(),
            expected
        );
        assert_eq!(
            run_script_with_services(global_call, Services::in_memory()).unwrap(),
            expected
        );
    }

    let module_error = run_script_with_services(
        "Storage.upload(\"x\", bytes([1]), unknown: true)",
        Services::in_memory(),
    )
    .unwrap_err();
    let global_error = run_script_with_services(
        "storage_upload(\"x\", bytes([1]), unknown: true)",
        Services::in_memory(),
    )
    .unwrap_err();
    assert_eq!(module_error.to_string(), global_error.to_string());
}

#[test]
fn modules_report_unknown_methods_and_reject_modules_in_stored_values() {
    let err = run_script_with_services("Auth.unknown()", Services::in_memory()).unwrap_err();
    assert!(err
        .to_string()
        .contains("unknown module method: Auth.unknown"));

    for source in [
        "Firestore.set(\"users/a\", {m: Auth})",
        "Firestore.set(\"users/a\", {items: [Auth]})",
        "Firestore.set(\"users/a\", {m: Auth}, merge: true)",
        "Firestore.update(\"users/a\", {items: array_union([Auth])})",
        "Firestore.col(\"users\").where(\"x\", \"==\", Auth).get()",
    ] {
        assert!(
            run_script_with_services(source, Services::in_memory()).is_err(),
            "module value should not be persisted or used as a query operand: {source}"
        );
    }
}

#[test]
fn unavailable_module_service_is_typed_and_checked_before_arguments() {
    let err = run_script_with_services(
        "Auth.sign_up(Firestore.set(\"users/a\", {n: 1}), \"secret1\")",
        Services::empty(ProjectConfig::default()).with_firestore(InMemoryFirestore::default()),
    )
    .unwrap_err();
    assert_eq!(
        err.downcast_ref::<CapabilityError>().unwrap().service,
        ServiceId::Auth
    );

    let err = run_script_with_services(
        "Firestore.set(\"users/a\", {n: 1})",
        Services::empty(ProjectConfig::default())
            .with_auth(InMemoryAuth::default())
            .with_storage(InMemoryStorage::default()),
    )
    .unwrap_err();
    assert_eq!(
        err.downcast_ref::<CapabilityError>().unwrap().service,
        ServiceId::Firestore
    );
}

#[test]
fn receiver_call_contract_errors_evaluate_receiver_before_arguments() {
    let mut runtime = Runtime::from_services(Services::in_memory());
    let err = runtime
        .run(
            r#"
fn receiver() {
  Firestore.set("effects/receiver", {ran: true})
  [1]
}
fn argument() {
  Firestore.set("effects/argument", {ran: true})
  true
}
receiver().len(nope: argument())
"#,
        )
        .unwrap_err();
    assert!(err.to_string().contains("unknown keyword arguments: nope"));
    assert_eq!(
        runtime
            .run("Firestore.get(\"effects/receiver\")[\"ran\"]")
            .unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        runtime.run("Firestore.get(\"effects/argument\")").unwrap(),
        Value::Nil
    );
}

#[test]
fn duplicate_keyword_validation_precedes_receiver_and_argument_evaluation() {
    let mut runtime = Runtime::from_services(Services::in_memory());
    let err = runtime
        .run(
            r#"
fn receiver() {
  Firestore.set("effects/receiver", {ran: true})
  [1]
}
fn argument() {
  Firestore.set("effects/argument", {ran: true})
  true
}
receiver().len(x: argument(), x: argument())
"#,
        )
        .unwrap_err();
    assert!(err.to_string().contains("duplicate keyword argument: x"));
    assert_eq!(
        runtime.run("Firestore.get(\"effects/receiver\")").unwrap(),
        Value::Nil
    );
    assert_eq!(
        runtime.run("Firestore.get(\"effects/argument\")").unwrap(),
        Value::Nil
    );
}

#[test]
fn execution_context_errors_precede_argument_side_effects() {
    let mut runtime = Runtime::from_services(Services::in_memory());
    let err = runtime
        .run(
            r#"
fn path() {
  Auth.sign_up("context@example.com", "secret1")
  "object"
}
Firestore.transaction { Storage.download(path()) }
"#,
        )
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("storage APIs are not allowed inside transaction"),
        "{err}"
    );
    assert_eq!(runtime.run("Auth.current_user()").unwrap(), Value::Nil);
}

#[test]
fn staging_context_checks_do_not_block_map_key_access() {
    assert_eq!(
        run_script_with_services(
            "m = {transaction: 1, batch: 2}\nFirestore.transaction { m.transaction + m.batch }",
            Services::in_memory(),
        )
        .unwrap(),
        Value::Int(3)
    );
}
