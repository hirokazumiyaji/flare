pub mod ast;
pub mod auth;
pub mod error;
pub mod firestore;
pub mod lexer;
pub mod parser;
pub mod runtime;
pub mod value;

use std::error::Error;

use runtime::Runtime;

pub use auth::{
    Auth, AuthEmulator, AuthError, AuthErrorKind, AuthSession, AuthToken, AuthUser, InMemoryAuth,
};
pub use error::{ErrorKind, FirelangError, Span};
pub use firestore::{Firestore, FirestoreEmulator, InMemoryFirestore};
pub use value::Value;

pub fn run_script(source: &str) -> Result<Value, Box<dyn Error>> {
    run_script_with_firestore(source, InMemoryFirestore::default())
}

pub fn run_script_with_firestore<F: Firestore>(
    source: &str,
    firestore: F,
) -> Result<Value, Box<dyn Error>> {
    Runtime::new(firestore).run(source)
}

/// Run a script with the default in-memory Firestore backend and an explicit
/// Auth backend.
pub fn run_script_with_auth<A: Auth + 'static>(
    source: &str,
    auth: A,
) -> Result<Value, Box<dyn Error>> {
    run_script_with_backends(source, InMemoryFirestore::default(), auth)
}

/// Run a script with explicit Firestore and Auth backends.
pub fn run_script_with_backends<F: Firestore, A: Auth + 'static>(
    source: &str,
    firestore: F,
    auth: A,
) -> Result<Value, Box<dyn Error>> {
    Runtime::new_with_auth(firestore, auth).run(source)
}

/// Descriptive alias for callers that prefer the backend-oriented name.
pub fn run_script_with_firestore_and_auth<F: Firestore, A: Auth + 'static>(
    source: &str,
    firestore: F,
    auth: A,
) -> Result<Value, Box<dyn Error>> {
    run_script_with_backends(source, firestore, auth)
}
