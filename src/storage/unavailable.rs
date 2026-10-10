use std::error::Error;

use super::{Storage, StorageError, StorageMetadata};

#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableStorage;

impl Storage for UnavailableStorage {
    fn is_available(&self) -> bool {
        false
    }

    fn upload(
        &mut self,
        _path: &str,
        _data: &[u8],
        _content_type: &str,
    ) -> Result<StorageMetadata, Box<dyn Error>> {
        Err(StorageError::backend("Storage service is unavailable").into())
    }

    fn download(&self, _path: &str) -> Result<Vec<u8>, Box<dyn Error>> {
        Err(StorageError::backend("Storage service is unavailable").into())
    }

    fn metadata(&self, _path: &str) -> Result<StorageMetadata, Box<dyn Error>> {
        Err(StorageError::backend("Storage service is unavailable").into())
    }

    fn delete(&mut self, _path: &str) -> Result<(), Box<dyn Error>> {
        Err(StorageError::backend("Storage service is unavailable").into())
    }
}
