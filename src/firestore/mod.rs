//! Firestore backend abstraction.

mod memory;

pub use memory::InMemoryFirestore;

use std::collections::BTreeMap;
use std::error::Error;

use crate::value::{QueryFilter, Value};

#[derive(Debug, Clone)]
pub enum WriteOp {
    Set {
        path: String,
        data: BTreeMap<String, Value>,
        merge: bool,
    },
    Update {
        path: String,
        data: BTreeMap<String, Value>,
    },
    Delete {
        path: String,
    },
}

pub trait Firestore {
    fn get(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>>;
    fn set(
        &mut self,
        path: &str,
        value: BTreeMap<String, Value>,
        merge: bool,
    ) -> Result<(), Box<dyn Error>>;
    fn update(&mut self, path: &str, value: BTreeMap<String, Value>) -> Result<(), Box<dyn Error>>;
    fn delete(&mut self, path: &str) -> Result<(), Box<dyn Error>>;
    fn query_collection(
        &self,
        path: &str,
        filters: &[QueryFilter],
        limit: Option<usize>,
    ) -> Result<Vec<Value>, Box<dyn Error>>;
    fn query_collection_group(
        &self,
        id: &str,
        filters: &[QueryFilter],
        limit: Option<usize>,
    ) -> Result<Vec<Value>, Box<dyn Error>>;

    fn begin_batch(&mut self) -> Result<(), Box<dyn Error>>;
    fn batch_stage(&mut self, op: WriteOp) -> Result<(), Box<dyn Error>>;
    fn commit_batch(&mut self) -> Result<(), Box<dyn Error>>;
    fn abort_batch(&mut self);

    fn begin_transaction(&mut self) -> Result<(), Box<dyn Error>>;
    fn get_in_transaction(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>>;
    fn transaction_stage(&mut self, op: WriteOp) -> Result<(), Box<dyn Error>>;
    fn commit_transaction(&mut self) -> Result<(), Box<dyn Error>>;
    fn abort_transaction(&mut self);
}
