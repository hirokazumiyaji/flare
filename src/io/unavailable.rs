use std::error::Error;
use std::path::Path;

use super::Io;
use crate::error::{CapabilityError, ServiceId};

#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableIO;

pub type UnavailableIo = UnavailableIO;

impl Io for UnavailableIO {
    fn is_available(&self) -> bool {
        false
    }

    fn read(&self, _path: &Path, _max_bytes: Option<usize>) -> Result<Vec<u8>, Box<dyn Error>> {
        Err(CapabilityError::new(ServiceId::IO).into())
    }

    fn write(
        &mut self,
        _path: &Path,
        _content: &[u8],
        _max_bytes: Option<usize>,
    ) -> Result<(), Box<dyn Error>> {
        Err(CapabilityError::new(ServiceId::IO).into())
    }
}
