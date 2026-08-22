pub mod ast;
pub mod firestore;
pub mod lexer;
pub mod parser;
pub mod runtime;
pub mod value;

use std::error::Error;

use runtime::Runtime;

pub use firestore::{Firestore, FirestoreEmulator, InMemoryFirestore};
pub use value::Value;

pub fn run_script(source: &str) -> Result<Value, Box<dyn Error>> {
    run_script_with_firestore(source, InMemoryFirestore::default())
}

pub fn run_script_with_firestore<F: Firestore>(
    source: &str,
    firestore: F,
) -> Result<Value, Box<dyn Error>> {
    let tokens = lexer::lex(source)?;
    let mut parser = parser::Parser::new(tokens);
    let program = parser.parse_program()?;
    let mut runtime = Runtime::new(firestore);
    runtime.eval_program(&program)
}
