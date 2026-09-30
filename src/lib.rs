pub mod ast;
pub mod error;
pub mod firestore;
pub mod lexer;
pub mod limits;
pub mod parser;
pub mod runtime;
pub mod value;

use std::error::Error;

pub use error::{ErrorKind, FirelangError, Span};
pub use firestore::{Firestore, FirestoreEmulator, InMemoryFirestore};
pub use limits::{ParserLimits, RuntimeLimits, RuntimeUsage};
pub use runtime::Runtime;
pub use value::{validate_value_depth, validate_value_limits, Value};

pub fn run_script(source: &str) -> Result<Value, Box<dyn Error>> {
    run_script_with_firestore(source, InMemoryFirestore::default())
}

pub fn run_script_with_firestore<F: Firestore>(
    source: &str,
    firestore: F,
) -> Result<Value, Box<dyn Error>> {
    Runtime::new(firestore).run(source)
}

/// Run a script with explicit runtime/parser/value/query budgets.
pub fn run_script_with_limits(
    source: &str,
    limits: RuntimeLimits,
) -> Result<Value, Box<dyn Error>> {
    run_script_with_firestore_and_limits(source, InMemoryFirestore::default(), limits)
}

/// Run a script with an explicit backend and budget configuration.
pub fn run_script_with_firestore_and_limits<F: Firestore>(
    source: &str,
    firestore: F,
    limits: RuntimeLimits,
) -> Result<Value, Box<dyn Error>> {
    Runtime::with_limits(firestore, limits).run(source)
}
