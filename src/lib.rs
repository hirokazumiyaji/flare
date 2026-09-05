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
pub use firestore::{Firestore, FirestoreEmulator, InMemoryFirestore};
pub use value::Value;

pub fn run_script(source: &str) -> Result<Value, Box<dyn Error>> {
    run_script_with_firestore(source, InMemoryFirestore::default())
}

pub fn run_script_with_firestore<F: Firestore>(
    source: &str,
    firestore: F,
) -> Result<Value, Box<dyn Error>> {
    let spanned = lexer::lex_spanned(source)?;
    let mut parser = parser::Parser::from_spanned(spanned);
    let (program, spans) = parser.parse_program_with_spans()?;
    let mut runtime = Runtime::new(firestore);
    runtime.eval_program_with_spans(&program, &spans)
}
