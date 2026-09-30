pub mod ast;
pub mod auth;
pub mod error;
pub mod firestore;
pub mod lexer;
pub mod parser;
pub mod runtime;
pub mod services;
pub mod storage;
pub mod value;

use std::error::Error;

pub use auth::{Auth, AuthEmulator, AuthUser, FirebaseAuthEmulator, InMemoryAuth};
pub use error::{CapabilityError, ErrorKind, FirelangError, Span};
pub use firestore::{Firestore, FirestoreEmulator, InMemoryFirestore};
pub use runtime::Runtime;
pub use services::{
    capability_error, ProjectConfig, RuntimeContext, RuntimeContextBuilder, Services,
    ServicesBuilder, DEFAULT_PROJECT_ID, DEFAULT_REQUEST_TIMEOUT,
};
pub use storage::{
    validate_content_type, validate_storage_path, FirebaseStorageEmulator, InMemoryStorage,
    Storage, StorageEmulator, StorageObject,
};
pub use value::Value;

/// Run a script with all local in-memory capabilities.
pub fn run_script(source: &str) -> Result<Value, Box<dyn Error>> {
    run_script_with_context(source, RuntimeContext::default())
}

/// Compatibility entry point for Firestore-only callers.
///
/// The old generic function remains available and adapts the supplied
/// provider into a [`RuntimeContext`].  Auth and Storage are intentionally not
/// implicitly installed here; use [`run_script_with_services`] when a script
/// needs those capabilities.
pub fn run_script_with_firestore<F: Firestore + 'static>(
    source: &str,
    firestore: F,
) -> Result<Value, Box<dyn Error>> {
    Runtime::new(firestore).run(source)
}

/// Run a script with an explicitly selected set of services.
pub fn run_script_with_services<S: Into<Services>>(
    source: &str,
    services: S,
) -> Result<Value, Box<dyn Error>> {
    run_script_with_context(source, RuntimeContext::new(services))
}

/// Run a script with an explicit project config and service set.
pub fn run_script_with_services_and_config<S: Into<Services>>(
    source: &str,
    config: ProjectConfig,
    services: S,
) -> Result<Value, Box<dyn Error>> {
    run_script_with_context(source, RuntimeContext::with_config(config, services))
}

/// Alias for [`run_script_with_services_and_config`] with the project first in
/// the configuration name.
pub fn run_script_with_config<S: Into<Services>>(
    source: &str,
    config: ProjectConfig,
    services: S,
) -> Result<Value, Box<dyn Error>> {
    run_script_with_services_and_config(source, config, services)
}

/// Convenience entry point for an Auth-only script with in-memory Firestore.
pub fn run_script_with_auth<A: Auth + 'static>(
    source: &str,
    auth: A,
) -> Result<Value, Box<dyn Error>> {
    let services = Services::from_firestore(InMemoryFirestore::default()).with_auth(auth);
    run_script_with_services(source, services)
}

/// Convenience entry point for a Storage-only script with in-memory Firestore.
pub fn run_script_with_storage<S: Storage + 'static>(
    source: &str,
    storage: S,
) -> Result<Value, Box<dyn Error>> {
    let services = Services::from_firestore(InMemoryFirestore::default()).with_storage(storage);
    run_script_with_services(source, services)
}

/// Convenience entry point with independently selected Firestore and Storage.
pub fn run_script_with_backends<F: Firestore + 'static, S: Storage + 'static>(
    source: &str,
    firestore: F,
    storage: S,
) -> Result<Value, Box<dyn Error>> {
    let services = Services::from_firestore(firestore).with_storage(storage);
    run_script_with_services(source, services)
}

/// Explicit name matching the argument order of [`run_script_with_backends`].
pub fn run_script_with_firestore_and_storage<F: Firestore + 'static, S: Storage + 'static>(
    source: &str,
    firestore: F,
    storage: S,
) -> Result<Value, Box<dyn Error>> {
    run_script_with_backends(source, firestore, storage)
}

/// Convenience entry point with independently selected Firestore and Auth.
pub fn run_script_with_firestore_and_auth<F: Firestore + 'static, A: Auth + 'static>(
    source: &str,
    firestore: F,
    auth: A,
) -> Result<Value, Box<dyn Error>> {
    let services = Services::from_firestore(firestore).with_auth(auth);
    run_script_with_services(source, services)
}

/// Convenience entry point with Firestore, Auth, and Storage selected together.
pub fn run_script_with_firestore_auth_and_storage<
    F: Firestore + 'static,
    A: Auth + 'static,
    S: Storage + 'static,
>(
    source: &str,
    firestore: F,
    auth: A,
    storage: S,
) -> Result<Value, Box<dyn Error>> {
    let services = Services::from_firestore(firestore)
        .with_auth(auth)
        .with_storage(storage);
    run_script_with_services(source, services)
}

/// Convenience entry point with Auth and Storage over in-memory Firestore.
pub fn run_script_with_auth_and_storage<A: Auth + 'static, S: Storage + 'static>(
    source: &str,
    auth: A,
    storage: S,
) -> Result<Value, Box<dyn Error>> {
    run_script_with_firestore_auth_and_storage(source, InMemoryFirestore::default(), auth, storage)
}

/// Run a script with a complete runtime context.
pub fn run_script_with_context(
    source: &str,
    context: RuntimeContext,
) -> Result<Value, Box<dyn Error>> {
    Runtime::<InMemoryFirestore>::from_context(context).run(source)
}
