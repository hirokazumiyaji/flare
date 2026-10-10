use std::error::Error;

use crate::error::{CapabilityError, ServiceId};
use crate::value::{QueryRef, Value};

use super::{Firestore, QueryCandidates, WriteOp};

#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableFirestore;

impl Firestore for UnavailableFirestore {
    fn is_available(&self) -> bool {
        false
    }

    fn get(&self, _path: &str) -> Result<Option<Value>, Box<dyn Error>> {
        Err(CapabilityError::new(ServiceId::Firestore).into())
    }

    fn apply(&mut self, _writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>> {
        Err(CapabilityError::new(ServiceId::Firestore).into())
    }

    fn run_query(&self, _query: &QueryRef) -> Result<QueryCandidates, Box<dyn Error>> {
        Err(CapabilityError::new(ServiceId::Firestore).into())
    }

    fn begin_transaction(&mut self) -> Result<(), Box<dyn Error>> {
        Err(CapabilityError::new(ServiceId::Firestore).into())
    }

    fn transaction_get(&self, _path: &str) -> Result<Option<Value>, Box<dyn Error>> {
        Err(CapabilityError::new(ServiceId::Firestore).into())
    }

    fn commit_transaction(&mut self, _writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>> {
        Err(CapabilityError::new(ServiceId::Firestore).into())
    }

    fn abort_transaction(&mut self) -> Result<(), Box<dyn Error>> {
        Err(CapabilityError::new(ServiceId::Firestore).into())
    }
}
