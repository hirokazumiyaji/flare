pub mod ast;
pub mod auth;
pub mod error;
pub mod firestore;
pub mod lexer;
pub mod parser;
pub mod runtime;
pub mod storage;
pub mod value;

use std::error::Error;

use runtime::Runtime;

#[cfg(feature = "emulator")]
pub use auth::AuthEmulator;
pub use auth::{Auth, AuthError, AuthErrorKind, AuthSession, AuthToken, AuthUser, InMemoryAuth};
pub use error::{ErrorKind, FirelangError, Span};
#[cfg(feature = "emulator")]
pub use firestore::{
    parse_timeout_ms, EmulatorAuth, EmulatorConfig, EmulatorConnectionError, FirestoreEmulator,
    FirestoreEmulatorOptions, DEFAULT_OPERATION_TIMEOUT, OPERATION_TIMEOUT_ENV,
};
pub use firestore::{Firestore, InMemoryFirestore, RequestClock};
pub use storage::{
    HttpStorageTransport, HttpTransport, InMemoryStorage, Storage, StorageEmulator, StorageError,
    StorageErrorKind, StorageHttpTransport, StorageMetadata, StorageRequest, StorageResponse,
    StorageTransport,
};

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
    Runtime::new_with_auth(InMemoryFirestore::default(), auth).run(source)
}

/// Descriptive alias for callers that prefer the backend-oriented name.
pub fn run_script_with_firestore_and_auth<F: Firestore, A: Auth + 'static>(
    source: &str,
    firestore: F,
    auth: A,
) -> Result<Value, Box<dyn Error>> {
    Runtime::new_with_auth(firestore, auth).run(source)
}

/// Run a script with the in-memory Firestore backend and the supplied Storage
/// backend. This is the convenient entry point for Storage-only scripts.
pub fn run_script_with_storage<S: Storage>(
    source: &str,
    storage: S,
) -> Result<Value, Box<dyn Error>> {
    run_script_with_backends(source, InMemoryFirestore::default(), storage)
}

/// Run a script with independently selected Firestore and Storage backends.
pub fn run_script_with_backends<F: Firestore, S: Storage>(
    source: &str,
    firestore: F,
    storage: S,
) -> Result<Value, Box<dyn Error>> {
    Runtime::with_storage(firestore, storage).run(source)
}

/// Explicitly named alias for callers that prefer the backend order in the
/// function name.
pub fn run_script_with_firestore_and_storage<F: Firestore, S: Storage>(
    source: &str,
    firestore: F,
    storage: S,
) -> Result<Value, Box<dyn Error>> {
    run_script_with_backends(source, firestore, storage)
}

/// Run a script with independently selected Firestore, Storage, and Auth
/// backends.
pub fn run_script_with_all_backends<F: Firestore, S: Storage, A: Auth + 'static>(
    source: &str,
    firestore: F,
    storage: S,
    auth: A,
) -> Result<Value, Box<dyn Error>> {
    Runtime::with_storage_and_auth(firestore, storage, auth).run(source)
}
