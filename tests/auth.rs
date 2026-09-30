//! In-memory Auth backend behavior tests (script-level contract).
//!
//! The same assertions run against the Auth emulator in `auth_emulator.rs`,
//! so both backends must agree on the visible contract.

#[macro_use]
mod common;

use firelang::runtime::Runtime;
use firelang::{run_script, AuthError, AuthErrorKind, ErrorKind, FirelangError, Value};

#[test]
fn sign_up_returns_session_with_redacted_tokens() {
    let out = run_script("auth_sign_up(\"alice@example.com\", \"password\")").unwrap();
    let Value::AuthSession(session) = &out else {
        panic!("expected AuthSession, got {out:?}");
    };
    assert_eq!(session.user.email, "alice@example.com");
    assert_eq!(session.user.provider_id.as_deref(), Some("password"));
    assert!(!session.id_token.is_empty());
    assert!(!session.refresh_token.is_empty());
    assert!(session.expires_at > 0);
}

#[test]
fn session_inspect_never_contains_raw_tokens() {
    let out = run_script("auth_sign_up(\"redact@example.com\", \"password\")").unwrap();
    let inspected = out.inspect();
    assert!(inspected.contains("AuthSession"), "{inspected}");
    assert!(inspected.contains("id_token=<redacted>"), "{inspected}");
    assert!(
        inspected.contains("refresh_token=<redacted>"),
        "{inspected}"
    );

    // The raw token value must not be reachable through any script-visible
    // representation of the session.
    let Value::AuthSession(session) = &out else {
        panic!("expected AuthSession, got {out:?}");
    };
    let id_token = session.id_token.expose().to_string();
    let refresh_token = session.refresh_token.expose().to_string();
    assert!(!inspected.contains(&id_token), "{inspected}");
    assert!(!inspected.contains(&refresh_token), "{inspected}");

    // Rust `Debug` is redacted too, so a `{:?}` in a caller cannot leak it.
    let debugged = format!("{session:?}");
    assert!(!debugged.contains(&id_token), "{debugged}");
    assert!(!debugged.contains(&refresh_token), "{debugged}");
}

#[test]
fn token_fields_project_redacted_values() {
    let out = run_script(
        r#"session = auth_sign_up("tokens@example.com", "password")
[session.id_token, session.refresh_token, session.user.email, session.expires_at > 0]"#,
    )
    .unwrap();
    assert_eq!(
        out,
        Value::Array(vec![
            Value::RedactedToken("id_token"),
            Value::RedactedToken("refresh_token"),
            Value::Str("tokens@example.com".to_string()),
            Value::Bool(true),
        ])
    );
    assert_eq!(
        out.inspect(),
        "[<redacted:id_token>, <redacted:refresh_token>, \"tokens@example.com\", true]"
    );
}

#[test]
fn current_user_tracks_the_session_and_sign_out_clears_it() {
    let mut runtime = Runtime::new(firelang::InMemoryFirestore::default());
    runtime
        .run("auth_sign_up(\"bob@example.com\", \"password\")")
        .unwrap();
    assert_eq!(
        runtime.run("auth_current_user()[\"email\"]").unwrap(),
        Value::Str("bob@example.com".to_string())
    );
    runtime.run("auth_sign_out()").unwrap();
    assert_eq!(runtime.run("auth_current_user()").unwrap(), Value::Nil);
    // sign_out is idempotent.
    runtime.run("auth_sign_out()").unwrap();
    assert_eq!(runtime.run("auth_current_user()").unwrap(), Value::Nil);
}

#[test]
fn sign_in_after_sign_out_restores_the_session() {
    let mut runtime = Runtime::new(firelang::InMemoryFirestore::default());
    runtime
        .run("auth_sign_up(\"carol@example.com\", \"password\")\nauth_sign_out()")
        .unwrap();
    assert_eq!(runtime.run("auth_current_user()").unwrap(), Value::Nil);
    runtime
        .run("auth_sign_in(\"carol@example.com\", \"password\")")
        .unwrap();
    assert_eq!(
        runtime.run("auth_current_user()[\"email\"]").unwrap(),
        Value::Str("carol@example.com".to_string())
    );
}

#[test]
fn email_is_canonicalized_for_sign_in() {
    let mut runtime = Runtime::new(firelang::InMemoryFirestore::default());
    runtime
        .run("auth_sign_up(\"Dave@Example.COM\", \"password\")")
        .unwrap();
    runtime.run("auth_sign_out()").unwrap();
    runtime
        .run("auth_sign_in(\"dave@example.com\", \"password\")")
        .unwrap();
    assert_eq!(
        runtime.run("auth_current_user()[\"email\"]").unwrap(),
        Value::Str("dave@example.com".to_string())
    );
}

#[test]
fn user_not_found_is_a_typed_error_with_position() {
    let err = run_script("auth_sign_in(\"nobody@example.com\", \"password\")").unwrap_err();
    let ferr = err
        .downcast_ref::<FirelangError>()
        .expect("auth failures should be FirelangError");
    assert_eq!(ferr.kind, ErrorKind::Backend);
    assert_eq!(ferr.auth_kind, Some(AuthErrorKind::UserNotFound));
    assert_eq!(ferr.span, Some(firelang::Span::new(1, 1)));
    assert_eq!(format!("{err}"), "1:1: auth user not found");
}

#[test]
fn invalid_credentials_are_a_typed_error() {
    let mut runtime = Runtime::new(firelang::InMemoryFirestore::default());
    runtime
        .run("auth_sign_up(\"erin@example.com\", \"password\")")
        .unwrap();
    let err = runtime
        .run("\nauth_sign_in(\"erin@example.com\", \"wrong-password\")")
        .unwrap_err();
    let ferr = err.downcast_ref::<FirelangError>().unwrap();
    assert_eq!(ferr.auth_kind, Some(AuthErrorKind::InvalidCredentials));
    assert_eq!(ferr.span, Some(firelang::Span::new(2, 1)));
    assert!(
        format!("{err}").contains("invalid auth credentials"),
        "{err}"
    );
}

#[test]
fn duplicate_sign_up_is_a_typed_error() {
    let mut runtime = Runtime::new(firelang::InMemoryFirestore::default());
    runtime
        .run("auth_sign_up(\"frank@example.com\", \"password\")")
        .unwrap();
    let err = runtime
        .run("auth_sign_up(\"frank@example.com\", \"password\")")
        .unwrap_err();
    let ferr = err.downcast_ref::<FirelangError>().unwrap();
    assert_eq!(ferr.auth_kind, Some(AuthErrorKind::UserAlreadyExists));
}

#[test]
fn invalid_email_and_weak_password_are_typed_errors() {
    let err = run_script("auth_sign_up(\"not-an-email\", \"password\")").unwrap_err();
    assert_eq!(
        err.downcast_ref::<FirelangError>().unwrap().auth_kind,
        Some(AuthErrorKind::InvalidEmail)
    );
    let err = run_script("auth_sign_up(\"gina@example.com\", \"short\")").unwrap_err();
    assert_eq!(
        err.downcast_ref::<FirelangError>().unwrap().auth_kind,
        Some(AuthErrorKind::WeakPassword)
    );
}

#[test]
fn failed_sign_up_does_not_start_a_session() {
    let err = run_script("auth_sign_up(\"hank@example.com\", \"short\")").unwrap_err();
    assert_eq!(
        err.downcast_ref::<FirelangError>().unwrap().auth_kind,
        Some(AuthErrorKind::WeakPassword)
    );
    assert_eq!(
        run_script("auth_current_user()").unwrap(),
        Value::Nil,
        "a failed sign-up must not create a session"
    );
}

#[test]
fn auth_builtins_validate_arguments() {
    for (script, expected) in [
        (
            "auth_sign_up(\"a@example.com\")",
            "expected string argument",
        ),
        ("auth_sign_in(1, \"password\")", "expected string argument"),
        (
            "auth_current_user(1)",
            "auth_current_user does not take arguments",
        ),
        ("auth_sign_out(1)", "auth_sign_out does not take arguments"),
        (
            "auth_sign_in(\"a@example.com\", \"password\", true)",
            "auth_sign_in takes at most 2 arguments",
        ),
        (
            "auth_current_user(foo: 1)",
            "auth_current_user does not accept keyword arguments",
        ),
    ] {
        let err = run_script(script).unwrap_err();
        assert!(format!("{err}").contains(expected), "{script}: {err}");
    }
}

#[test]
fn auth_builtin_names_cannot_be_redefined() {
    let err = run_script("fn auth_sign_in(email, password) { nil }").unwrap_err();
    assert!(
        format!("{err}").contains("cannot redefine builtin: auth_sign_in"),
        "{err}"
    );
}

#[test]
fn auth_session_is_not_a_writable_value() {
    // AuthSession is a runtime-only value: it must not be silently stored or
    // treated as a plain document payload.
    let err = run_script(
        r#"session = auth_sign_up("ivan@example.com", "password")
set("users/1", {session: session})"#,
    );
    assert!(
        err.is_err(),
        "storing an AuthSession in a document must fail, got {err:?}"
    );
}

#[test]
fn firestore_scripts_work_without_any_auth_call() {
    // Auth must not regress the Firestore-only surface.
    let out = run_script("set(\"users/1\", {name: \"alice\"})\nget(\"users/1\")").unwrap();
    assert_eq!(out, map! {"name" => Value::Str("alice".to_string())});
    assert_eq!(run_script("auth_current_user()").unwrap(), Value::Nil);
}

#[test]
fn backend_errors_are_typed_without_script_access() {
    // The trait is the public contract; a fake backend can return any
    // `AuthError` and the runtime maps it to a Backend-kind FirelangError.
    struct NetworkAuth;

    impl firelang::Auth for NetworkAuth {
        fn sign_up(
            &mut self,
            _email: &str,
            _password: &str,
        ) -> Result<firelang::AuthSession, AuthError> {
            Err(AuthError::Network("connection refused".into()))
        }
        fn sign_in(
            &mut self,
            _email: &str,
            _password: &str,
        ) -> Result<firelang::AuthSession, AuthError> {
            Err(AuthError::Network("connection refused".into()))
        }
        fn current_user(&self) -> Result<Option<firelang::AuthUser>, AuthError> {
            Ok(None)
        }
        fn sign_out(&mut self) -> Result<(), AuthError> {
            Ok(())
        }
    }

    let mut runtime = Runtime::new_with_auth(firelang::InMemoryFirestore::default(), NetworkAuth);
    let err = runtime
        .run("auth_sign_in(\"jack@example.com\", \"password\")")
        .unwrap_err();
    let ferr = err.downcast_ref::<FirelangError>().unwrap();
    assert_eq!(ferr.kind, ErrorKind::Backend);
    assert_eq!(ferr.auth_kind, Some(AuthErrorKind::Network));
    assert_eq!(ferr.span, Some(firelang::Span::new(1, 1)));
    assert_eq!(
        format!("{err}"),
        "1:1: auth network error: connection refused"
    );
}
