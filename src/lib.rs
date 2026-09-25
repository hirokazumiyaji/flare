pub mod ast;
pub mod error;
pub mod firestore;
pub mod lexer;
pub mod parser;
pub mod runtime;
pub mod storage;
pub mod value;

use std::error::Error;

use runtime::Runtime;

pub use error::{ErrorKind, FirelangError, Span};
pub use firestore::{Firestore, FirestoreEmulator, InMemoryFirestore};
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
