#![cfg(feature = "emulator")]
//! Integration tests against the Firebase Auth emulator
//! (`FIREBASE_AUTH_EMULATOR_HOST`).
//!
//! Locally: start the emulator or skip when unset. CI: see
//! `.github/workflows/ci.yml` (`firebase emulators:exec` sets the host).
//!
//! The assertions deliberately mirror `tests/auth.rs`: both backends must
//! agree on the visible script contract.

use flare::runtime::Runtime;
use flare::{
    run_script_with_auth, AuthEmulator, AuthErrorKind, FlareError, InMemoryFirestore,
};

fn emulator() -> Option<AuthEmulator> {
    AuthEmulator::from_env().expect("FIREBASE_AUTH_EMULATOR_HOST must be valid UTF-8 when set")
}

fn unique_email(prefix: &str) -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{prefix}-{n}@example.com")
}

#[test]
fn emulator_sign_up_sign_in_and_sign_out() {
    let Some(auth) = emulator() else {
        eprintln!("skip: FIREBASE_AUTH_EMULATOR_HOST is not set");
        return;
    };
    let email = unique_email("vertical");
    let mut runtime = Runtime::new_with_auth(InMemoryFirestore::default(), auth);

    let session_value = runtime
        .run(&format!("auth_sign_up(\"{email}\", \"password\")"))
        .expect("sign-up should succeed against the Auth emulator");
    let flare::Value::AuthSession(session) = &session_value else {
        panic!("expected AuthSession, got {session_value:?}");
    };
    assert_eq!(session.user.email, email);
    assert!(!session.id_token.expose().is_empty());
    assert!(!session.refresh_token.expose().is_empty());
    assert!(session.expires_at > 0);

    // Tokens from the emulator must be redacted in every display path.
    let inspected = session_value.inspect();
    assert!(inspected.contains("id_token=<redacted>"), "{inspected}");
    assert!(
        !inspected.contains(session.id_token.expose()),
        "id token leaked into inspect: {inspected}"
    );
    assert!(
        !inspected.contains(session.refresh_token.expose()),
        "refresh token leaked into inspect: {inspected}"
    );
    assert!(
        !format!("{session:?}").contains(session.id_token.expose()),
        "id token leaked into Debug"
    );

    // current_user reflects the session, and sign-out clears it.
    assert_eq!(
        runtime.run("auth_current_user()[\"email\"]").unwrap(),
        flare::Value::Str(email.clone())
    );
    runtime.run("auth_sign_out()").unwrap();
    assert_eq!(
        runtime.run("auth_current_user()").unwrap(),
        flare::Value::Nil
    );

    // Sign back in with the account created above.
    runtime
        .run(&format!("auth_sign_in(\"{email}\", \"password\")"))
        .expect("sign-in should succeed after sign-up");
    assert_eq!(
        runtime.run("auth_current_user()[\"email\"]").unwrap(),
        flare::Value::Str(email)
    );
}

#[test]
fn emulator_maps_user_not_found() {
    let Some(auth) = emulator() else {
        eprintln!("skip: FIREBASE_AUTH_EMULATOR_HOST is not set");
        return;
    };
    let email = unique_email("missing");
    let err = run_script_with_auth(&format!("auth_sign_in(\"{email}\", \"password\")"), auth)
        .unwrap_err();
    let ferr = err.downcast_ref::<FlareError>().unwrap();
    assert_eq!(ferr.auth_kind, Some(AuthErrorKind::UserNotFound), "{err}");
    assert_eq!(ferr.span, Some(flare::Span::new(1, 1)), "{err}");
}

#[test]
fn emulator_maps_invalid_credentials_and_duplicate_sign_up() {
    let Some(auth) = emulator() else {
        eprintln!("skip: FIREBASE_AUTH_EMULATOR_HOST is not set");
        return;
    };
    let email = unique_email("dup");
    let mut runtime = Runtime::new_with_auth(InMemoryFirestore::default(), auth);

    runtime
        .run(&format!("auth_sign_up(\"{email}\", \"password\")"))
        .unwrap();

    let err = runtime
        .run(&format!("auth_sign_in(\"{email}\", \"wrong-password\")"))
        .unwrap_err();
    assert_eq!(
        err.downcast_ref::<FlareError>().unwrap().auth_kind,
        Some(AuthErrorKind::InvalidCredentials),
        "{err}"
    );

    let err = runtime
        .run(&format!("auth_sign_up(\"{email}\", \"password\")"))
        .unwrap_err();
    assert_eq!(
        err.downcast_ref::<FlareError>().unwrap().auth_kind,
        Some(AuthErrorKind::UserAlreadyExists),
        "{err}"
    );
}

#[test]
fn emulator_validation_matches_in_memory_validation() {
    let Some(auth) = emulator() else {
        eprintln!("skip: FIREBASE_AUTH_EMULATOR_HOST is not set");
        return;
    };
    let mut runtime = Runtime::new_with_auth(InMemoryFirestore::default(), auth);

    for (script, kind) in [
        (
            "auth_sign_up(\"not-an-email\", \"password\")",
            AuthErrorKind::InvalidEmail,
        ),
        (
            "auth_sign_up(\"weak@example.com\", \"12345\")",
            AuthErrorKind::WeakPassword,
        ),
    ] {
        let err = runtime.run(script).unwrap_err();
        assert_eq!(
            err.downcast_ref::<FlareError>().unwrap().auth_kind,
            Some(kind),
            "{script}: {err}"
        );
    }
}

#[test]
fn emulator_firestore_script_is_unaffected_by_auth() {
    // A Firestore-only script must behave exactly as before when an Auth
    // backend is wired in but never used.
    let Some(auth) = emulator() else {
        eprintln!("skip: FIREBASE_AUTH_EMULATOR_HOST is not set");
        return;
    };
    let out = run_script_with_auth(
        "set(\"users/noauth\", {name: \"alice\"})\nget(\"users/noauth\")[\"name\"]",
        auth,
    )
    .unwrap();
    assert_eq!(out, flare::Value::Str("alice".to_string()));
}
