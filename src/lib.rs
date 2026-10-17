pub mod ast;
pub mod auth;
pub mod error;
pub mod firestore;
pub mod io;
pub mod lexer;
pub mod limits;
pub mod parser;
pub mod repl;
pub mod runtime;
pub mod services;
pub mod storage;
pub mod value;

use std::error::Error;

pub use runtime::{builtin_names, Runtime};
pub use services::{ProjectConfig, Services};

pub use auth::{
    Auth, AuthError, AuthErrorKind, AuthSession, AuthToken, AuthUser, InMemoryAuth, UnavailableAuth,
};
#[cfg(feature = "emulator")]
pub use auth::{AuthEmulator, AuthEmulatorOptions, DEFAULT_AUTH_OPERATION_TIMEOUT};
pub use error::{CapabilityError, ErrorKind, FlareError, ServiceId, Span};
#[cfg(feature = "emulator")]
pub use firestore::{
    DEFAULT_OPERATION_TIMEOUT, EmulatorAuth, EmulatorConfig, EmulatorConnectionError,
    FirestoreEmulator, FirestoreEmulatorOptions, OPERATION_TIMEOUT_ENV, parse_timeout_ms,
};
pub use firestore::{Firestore, InMemoryFirestore, RequestClock, UnavailableFirestore};
pub use io::{InMemoryIo, Io, StdIo, UnavailableIO, UnavailableIo};
pub use limits::{
    DEFAULT_BACKEND_TIMEOUT_MS, DEFAULT_MAX_BYTES_SIZE, DEFAULT_MAX_CALL_DEPTH,
    DEFAULT_MAX_FILE_IO_BYTES, DEFAULT_MAX_LOOP_ITERATIONS, DEFAULT_MAX_PARSER_DEPTH,
    DEFAULT_MAX_QUERY_RESULTS, DEFAULT_MAX_STEPS, DEFAULT_MAX_STRING_BYTES,
    DEFAULT_MAX_VALUE_DEPTH, DEFAULT_MAX_WALL_CLOCK_MS, ParserLimits, RuntimeLimits, RuntimeUsage,
};
pub use storage::{
    HttpStorageTransport, HttpTransport, InMemoryStorage, Storage, StorageEmulator, StorageError,
    StorageErrorKind, StorageHttpTransport, StorageMetadata, StorageRequest, StorageResponse,
    StorageTransport, UnavailableStorage,
};
pub use value::{
    BuiltinModule, Value, validate_value_depth, validate_value_limits,
    validate_value_limits_with_sizes,
};

pub fn run_script(source: &str) -> Result<Value, Box<dyn Error>> {
    run_script_with_services(source, Services::in_memory())
}

pub fn run_script_with_firestore<F: Firestore>(
    source: &str,
    firestore: F,
) -> Result<Value, Box<dyn Error>> {
    run_script_with_services(source, Services::in_memory().with_firestore(firestore))
}

/// Run a script with the default in-memory Firestore backend and an explicit
/// Auth backend.
pub fn run_script_with_auth<A: Auth + 'static>(
    source: &str,
    auth: A,
) -> Result<Value, Box<dyn Error>> {
    run_script_with_services(source, Services::in_memory().with_auth(auth))
}

/// Descriptive alias for callers that prefer the backend-oriented name.
pub fn run_script_with_firestore_and_auth<F: Firestore, A: Auth + 'static>(
    source: &str,
    firestore: F,
    auth: A,
) -> Result<Value, Box<dyn Error>> {
    run_script_with_services(
        source,
        Services::in_memory()
            .with_firestore(firestore)
            .with_auth(auth),
    )
}

/// Run a script with the in-memory Firestore backend and the supplied Storage
/// backend. This is the convenient entry point for Storage-only scripts.
pub fn run_script_with_storage<S: Storage>(
    source: &str,
    storage: S,
) -> Result<Value, Box<dyn Error>> {
    run_script_with_services(source, Services::in_memory().with_storage(storage))
}

/// Run a script with independently selected Firestore and Storage backends.
pub fn run_script_with_backends<F: Firestore, S: Storage>(
    source: &str,
    firestore: F,
    storage: S,
) -> Result<Value, Box<dyn Error>> {
    run_script_with_services(
        source,
        Services::in_memory()
            .with_firestore(firestore)
            .with_storage(storage),
    )
}

/// Explicitly named alias for callers that prefer the backend order in the
/// function name.
pub fn run_script_with_firestore_and_storage<F: Firestore, S: Storage>(
    source: &str,
    firestore: F,
    storage: S,
) -> Result<Value, Box<dyn Error>> {
    run_script_with_services(
        source,
        Services::in_memory()
            .with_firestore(firestore)
            .with_storage(storage),
    )
}

/// Run a script with independently selected Firestore, Storage, and Auth
/// backends.
pub fn run_script_with_all_backends<F: Firestore, S: Storage, A: Auth + 'static>(
    source: &str,
    firestore: F,
    storage: S,
    auth: A,
) -> Result<Value, Box<dyn Error>> {
    run_script_with_services(
        source,
        Services::in_memory()
            .with_firestore(firestore)
            .with_storage(storage)
            .with_auth(auth),
    )
}

/// Same as [`run_script_with_all_backends`] with explicit execution budgets.
///
/// The CLI uses this so `--max-*` flags and embedders both apply the same
/// finite defaults that protect untrusted scripts.
pub fn run_script_with_all_backends_and_limits<F: Firestore, S: Storage, A: Auth + 'static>(
    source: &str,
    firestore: F,
    storage: S,
    auth: A,
    limits: RuntimeLimits,
) -> Result<Value, Box<dyn Error>> {
    run_script_with_services_and_limits(
        source,
        Services::in_memory()
            .with_firestore(firestore)
            .with_storage(storage)
            .with_auth(auth),
        limits,
    )
}

/// Run a script with explicit budgets against the in-memory Firestore backend.
pub fn run_script_with_limits(
    source: &str,
    limits: RuntimeLimits,
) -> Result<Value, Box<dyn Error>> {
    run_script_with_services_and_limits(source, Services::in_memory(), limits)
}

/// Run a script against one Firestore backend with explicit budgets.
pub fn run_script_with_firestore_and_limits<F: Firestore>(
    source: &str,
    firestore: F,
    limits: RuntimeLimits,
) -> Result<Value, Box<dyn Error>> {
    run_script_with_services_and_limits(
        source,
        Services::in_memory().with_firestore(firestore),
        limits,
    )
}

pub fn run_script_with_services<F: Firestore, S: Storage>(
    source: &str,
    services: Services<F, S>,
) -> Result<Value, Box<dyn Error>> {
    Runtime::from_services(services).run(source)
}

pub fn run_script_with_services_and_limits<F: Firestore, S: Storage>(
    source: &str,
    services: Services<F, S>,
    limits: RuntimeLimits,
) -> Result<Value, Box<dyn Error>> {
    Runtime::from_services_with_limits(services, limits).run(source)
}
