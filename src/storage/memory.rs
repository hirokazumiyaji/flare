use std::collections::BTreeMap;
use std::error::Error;

use super::{validate_content_type, validate_storage_path, Storage, StorageError, StorageMetadata};

/// Deterministic in-memory Storage backend used by the default runtime and
/// by unit tests. It stores bytes as `Vec<u8>`; no UTF-8 conversion occurs.
#[derive(Debug, Clone, Default)]
pub struct InMemoryStorage {
    objects: BTreeMap<String, StoredObject>,
    next_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredObject {
    data: Vec<u8>,
    metadata: StorageMetadata,
}

impl Storage for InMemoryStorage {
    fn upload(
        &mut self,
        path: &str,
        data: &[u8],
        content_type: &str,
    ) -> Result<StorageMetadata, Box<dyn Error>> {
        validate_storage_path(path)?;
        validate_content_type(content_type)?;
        let size = u64::try_from(data.len())
            .map_err(|_| StorageError::invalid_data("storage object is too large"))?;
        self.next_generation = self.next_generation.saturating_add(1);
        let generation = self.next_generation.max(1).to_string();
        let metadata = StorageMetadata {
            name: path.to_string(),
            bucket: "flare-memory".to_string(),
            content_type: content_type.to_string(),
            size,
            generation,
            metageneration: "1".to_string(),
            ..StorageMetadata::default()
        };
        self.objects.insert(
            path.to_string(),
            StoredObject {
                data: data.to_vec(),
                metadata: metadata.clone(),
            },
        );
        Ok(metadata)
    }

    fn download(&self, path: &str) -> Result<Vec<u8>, Box<dyn Error>> {
        validate_storage_path(path)?;
        self.objects
            .get(path)
            .map(|object| object.data.clone())
            .ok_or_else(|| StorageError::not_found(path).into())
    }

    fn metadata(&self, path: &str) -> Result<StorageMetadata, Box<dyn Error>> {
        validate_storage_path(path)?;
        self.objects
            .get(path)
            .map(|object| object.metadata.clone())
            .ok_or_else(|| StorageError::not_found(path).into())
    }

    fn delete(&mut self, path: &str) -> Result<(), Box<dyn Error>> {
        validate_storage_path(path)?;
        self.objects
            .remove(path)
            .map(|_| ())
            .ok_or_else(|| StorageError::not_found(path).into())
    }
}

impl InMemoryStorage {
    /// Number of stored objects. This is useful for tests but is not exposed
    /// as a script API (listing is outside the MVP).
    pub fn len(&self) -> usize {
        self.objects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crud_round_trips_arbitrary_bytes_and_metadata() {
        let mut storage = InMemoryStorage::default();
        let bytes = vec![0x00, 0xff, 0x80, b'a', 0x00];
        let metadata = storage
            .upload("avatars/alice.bin", &bytes, "application/octet-stream")
            .unwrap();
        assert_eq!(metadata.name, "avatars/alice.bin");
        assert_eq!(metadata.content_type, "application/octet-stream");
        assert_eq!(metadata.size, bytes.len() as u64);
        assert_eq!(storage.download("avatars/alice.bin").unwrap(), bytes);
        assert_eq!(storage.metadata("avatars/alice.bin").unwrap(), metadata);
        storage.delete("avatars/alice.bin").unwrap();
        assert!(storage.is_empty());
    }

    #[test]
    fn missing_objects_have_a_specific_error() {
        let mut storage = InMemoryStorage::default();
        let err = storage.download("missing").unwrap_err();
        assert!(
            format!("{err}").contains("storage object not found"),
            "{err}"
        );
        let err = storage.metadata("missing").unwrap_err();
        assert!(
            format!("{err}").contains("storage object not found"),
            "{err}"
        );
        let err = storage.delete("missing").unwrap_err();
        assert!(
            format!("{err}").contains("storage object not found"),
            "{err}"
        );
    }

    #[test]
    fn replacement_keeps_metadata_consistent() {
        let mut storage = InMemoryStorage::default();
        let first = storage.upload("objects/a", b"old", "text/plain").unwrap();
        let second = storage
            .upload("objects/a", &[0, 1, 2], "image/png")
            .unwrap();
        assert_ne!(first.generation, second.generation);
        assert_eq!(second.size, 3);
        assert_eq!(storage.download("objects/a").unwrap(), vec![0, 1, 2]);
        assert_eq!(storage.metadata("objects/a").unwrap(), second);
    }

    #[test]
    fn invalid_content_type_is_rejected_before_storing() {
        let mut storage = InMemoryStorage::default();
        let err = storage.upload("objects/a", b"x", "not a type").unwrap_err();
        assert!(
            format!("{err}").contains("invalid storage content type"),
            "{err}"
        );
        assert!(storage.is_empty());
    }
}
