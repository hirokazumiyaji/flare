pub mod ast;
pub mod error;
pub mod firestore;
pub mod lexer;
pub mod parser;
pub mod runtime;
pub mod value;

use std::error::Error;

use runtime::Runtime;

pub use error::{ErrorKind, FirelangError, Span};
pub use firestore::{
    parse_timeout_ms, Firestore, FirestoreEmulator, FirestoreEmulatorOptions, InMemoryFirestore,
    DEFAULT_OPERATION_TIMEOUT, OPERATION_TIMEOUT_ENV,
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
