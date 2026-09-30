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
use runtime::Runtime;
pub use services::{
    capability_error, ProjectConfig, RuntimeContext, Services, ServicesBuilder, DEFAULT_PROJECT_ID,
    DEFAULT_REQUEST_TIMEOUT,
};
pub use storage::{
    FirebaseStorageEmulator, InMemoryStorage, Storage, StorageEmulator, StorageObject,
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

/// Run a script with a complete runtime context.
pub fn run_script_with_context(
    source: &str,
    context: RuntimeContext,
) -> Result<Value, Box<dyn Error>> {
    Runtime::<InMemoryFirestore>::from_context(context).run(source)
}
