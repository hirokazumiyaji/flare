//! Provider-neutral object storage capability used by the runtime.
//!
//! Storage values use the language [`Value`] type at the capability boundary.
//! That keeps the synchronous runtime independent of a particular SDK while
//! still allowing text, maps, arrays, and opaque provider values to round-trip
//! in a local fake.  A real adapter can encode the value as JSON, bytes, or a
//! provider-native object before returning from the synchronous method.

use std::collections::BTreeMap;
use std::error::Error;

use crate::value::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct StorageObject {
    pub path: String,
    pub data: Value,
    pub content_type: Option<String>,
    pub metadata: BTreeMap<String, String>,
}

impl StorageObject {
    pub fn new(path: impl Into<String>, data: Value) -> Self {
        Self {
            path: path.into(),
            data,
            content_type: None,
            metadata: BTreeMap::new(),
        }
    }

    pub fn with_content_type(mut self, content_type: impl Into<String>) -> Self {
        self.content_type = Some(content_type.into());
        self
    }

    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Convert an object to the map shape exposed by `storage_list()`.
    pub fn to_value(&self) -> Value {
        let mut fields = BTreeMap::new();
        fields.insert("path".to_string(), Value::Str(self.path.clone()));
        fields.insert("data".to_string(), self.data.clone());
        if let Some(content_type) = &self.content_type {
            fields.insert("content_type".to_string(), Value::Str(content_type.clone()));
        }
        if !self.metadata.is_empty() {
            let metadata = self
                .metadata
                .iter()
                .map(|(k, v)| (k.clone(), Value::Str(v.clone())))
                .collect();
            fields.insert("metadata".to_string(), Value::Map(metadata));
        }
        Value::Map(fields)
    }
}

/// Synchronous object-storage capability.
///
/// The trait is object safe, so adding another provider does not add a
/// parameter to `Runtime` or to its builtin dispatch table.
pub trait Storage {
    fn put(&mut self, path: &str, data: Value) -> Result<StorageObject, Box<dyn Error>>;
    fn get(&self, path: &str) -> Result<Option<StorageObject>, Box<dyn Error>>;
    fn delete(&mut self, path: &str) -> Result<bool, Box<dyn Error>>;

    /// Store a complete object, including optional content type and user
    /// metadata.  Providers that do not have a metadata concept can keep this
    /// default; providers that do should override it so metadata round-trips.
    fn put_object(&mut self, object: StorageObject) -> Result<StorageObject, Box<dyn Error>> {
        let mut stored = self.put(&object.path, object.data)?;
        stored.content_type = object.content_type;
        stored.metadata = object.metadata;
        Ok(stored)
    }

    /// List objects below a directory-style prefix.  Listing is optional for a
    /// provider and unsupported operations fail explicitly instead of looking
    /// like an empty bucket.
    fn list(&self, _prefix: &str) -> Result<Vec<StorageObject>, Box<dyn Error>> {
        Err("storage provider does not support list".into())
    }

    /// Text convenience operation for Rust adapters.  The script runtime uses
    /// `put` directly so maps and arrays remain available as well.
    fn put_text(&mut self, path: &str, text: String) -> Result<StorageObject, Box<dyn Error>> {
        self.put(path, Value::Str(text))
    }

    fn get_text(&self, path: &str) -> Result<Option<String>, Box<dyn Error>> {
        Ok(self.get(path)?.and_then(|object| match object.data {
            Value::Str(text) => Some(text),
            _ => None,
        }))
    }

    /// Common provider-neutral aliases for adapters and embedding hosts.
    fn write(&mut self, path: &str, data: Value) -> Result<StorageObject, Box<dyn Error>> {
        self.put(path, data)
    }

    fn read(&self, path: &str) -> Result<Option<StorageObject>, Box<dyn Error>> {
        self.get(path)
    }

    fn download(&self, path: &str) -> Result<Option<StorageObject>, Box<dyn Error>> {
        self.get(path)
    }

    fn metadata(&self, path: &str) -> Result<Option<StorageObject>, Box<dyn Error>> {
        self.get(path)
    }

    fn upload(&mut self, path: &str, data: Value) -> Result<StorageObject, Box<dyn Error>> {
        self.put(path, data)
    }

    fn remove(&mut self, path: &str) -> Result<bool, Box<dyn Error>> {
        self.delete(path)
    }
}

/// Deterministic in-memory storage fake.
#[derive(Clone, Default)]
pub struct InMemoryStorage {
    objects: BTreeMap<String, StorageObject>,
}

impl InMemoryStorage {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, object: StorageObject) -> Result<StorageObject, Box<dyn Error>> {
        self.put_object(object)
    }

    pub fn len(&self) -> usize {
        self.objects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    pub fn clear(&mut self) {
        self.objects.clear();
    }
}

impl Storage for InMemoryStorage {
    fn put(&mut self, path: &str, data: Value) -> Result<StorageObject, Box<dyn Error>> {
        let path = normalize_path(path)?;
        let object = StorageObject::new(path, data);
        self.objects.insert(object.path.clone(), object.clone());
        Ok(object)
    }

    fn get(&self, path: &str) -> Result<Option<StorageObject>, Box<dyn Error>> {
        let path = normalize_path(path)?;
        Ok(self.objects.get(&path).cloned())
    }

    fn put_object(&mut self, object: StorageObject) -> Result<StorageObject, Box<dyn Error>> {
        let mut object = object;
        object.path = normalize_path(&object.path)?;
        self.objects.insert(object.path.clone(), object.clone());
        Ok(object)
    }

    fn delete(&mut self, path: &str) -> Result<bool, Box<dyn Error>> {
        let path = normalize_path(path)?;
        Ok(self.objects.remove(&path).is_some())
    }

    fn list(&self, prefix: &str) -> Result<Vec<StorageObject>, Box<dyn Error>> {
        let prefix = normalize_prefix(prefix)?;
        Ok(self
            .objects
            .values()
            .filter(|object| {
                prefix.is_empty()
                    || object.path == prefix
                    || object.path.starts_with(&format!("{prefix}/"))
            })
            .cloned()
            .collect())
    }
}

/// In-process Storage emulator-mode adapter.
///
/// Like the Auth emulator adapter, this implementation keeps state in memory
/// and performs **no** HTTP request to a Firebase Storage emulator.  A remote
/// adapter can implement the same trait without changing scripts or `Runtime`.
pub struct StorageEmulator {
    project_id: String,
    inner: InMemoryStorage,
}

impl StorageEmulator {
    pub fn new(project_id: impl Into<String>) -> Self {
        Self {
            project_id: project_id.into(),
            inner: InMemoryStorage::default(),
        }
    }

    pub fn from_config(config: &crate::services::ProjectConfig) -> Self {
        Self::new(config.project_id.clone())
    }

    /// Detect non-secret emulator configuration without opening a connection.
    pub fn from_env() -> Option<Self> {
        let project_id = std::env::var("FIREBASE_PROJECT_ID")
            .or_else(|_| std::env::var("GCLOUD_PROJECT"))
            .or_else(|_| std::env::var("FIRESTORE_PROJECT_ID"))
            .unwrap_or_else(|_| "firelang-test".to_string());
        std::env::var("FIREBASE_STORAGE_EMULATOR_HOST")
            .ok()
            .map(|_| Self::new(project_id))
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

impl Storage for StorageEmulator {
    fn put(&mut self, path: &str, data: Value) -> Result<StorageObject, Box<dyn Error>> {
        self.inner.put(path, data)
    }

    fn put_object(&mut self, object: StorageObject) -> Result<StorageObject, Box<dyn Error>> {
        self.inner.put_object(object)
    }

    fn get(&self, path: &str) -> Result<Option<StorageObject>, Box<dyn Error>> {
        self.inner.get(path)
    }

    fn delete(&mut self, path: &str) -> Result<bool, Box<dyn Error>> {
        self.inner.delete(path)
    }

    fn list(&self, prefix: &str) -> Result<Vec<StorageObject>, Box<dyn Error>> {
        self.inner.list(prefix)
    }
}

/// Alias for hosts that name their in-process emulator-mode adapter with the
/// Firebase provider.
pub type FirebaseStorageEmulator = StorageEmulator;

/// Validate a Storage object path without performing I/O.
///
/// Storage paths are directory-style names, not Firestore document paths, so
/// they have no even/odd segment rule.
pub fn validate_storage_path(path: &str) -> Result<(), Box<dyn Error>> {
    normalize_path(path).map(|_| ())
}

/// Validate a media type string used by a provider adapter.
pub fn validate_content_type(content_type: &str) -> Result<(), Box<dyn Error>> {
    let invalid = content_type.trim().is_empty()
        || content_type.trim() != content_type
        || content_type.chars().any(char::is_control);
    let essence = content_type.split(';').next().unwrap_or_default().trim();
    let valid_essence = essence.split('/').count() == 2
        && !essence.starts_with('/')
        && !essence.ends_with('/')
        && !essence.chars().any(char::is_whitespace);
    if invalid || !valid_essence {
        return Err(format!("invalid storage content type: {content_type}").into());
    }
    Ok(())
}

fn normalize_path(path: &str) -> Result<String, Box<dyn Error>> {
    if path.is_empty()
        || path.starts_with('/')
        || path.ends_with('/')
        || path.split('/').any(|segment| segment.is_empty())
    {
        return Err(format!("invalid storage path: {path}").into());
    }
    Ok(path.to_string())
}

fn normalize_prefix(prefix: &str) -> Result<String, Box<dyn Error>> {
    if prefix.is_empty() {
        return Ok(String::new());
    }
    // Accept a directory form such as `images/` and normalize it to the same
    // boundary used by `list`.
    let trimmed = prefix.strip_suffix('/').unwrap_or(prefix);
    normalize_path(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_storage_round_trips_and_lists_directory_boundaries() {
        let mut storage = InMemoryStorage::new();
        let object = StorageObject::new("images/a.png", Value::Str("a".to_string()))
            .with_content_type("image/png")
            .with_metadata("owner", "alice");
        assert_eq!(
            storage.put_object(object).unwrap().content_type.as_deref(),
            Some("image/png")
        );

        storage.put("images/nested/b.png", Value::Int(2)).unwrap();
        storage.put("images-old/c.png", Value::Int(3)).unwrap();

        assert_eq!(storage.list("images").unwrap().len(), 2);
        assert_eq!(storage.list("images/").unwrap().len(), 2);
        assert_eq!(storage.list("").unwrap().len(), 3);
        assert_eq!(
            storage.get("images/a.png").unwrap().unwrap().metadata["owner"],
            "alice"
        );
        assert!(storage.delete("images/a.png").unwrap());
        assert!(!storage.delete("images/a.png").unwrap());
        assert!(storage.get("images/a.png").unwrap().is_none());
    }

    #[test]
    fn public_path_and_content_type_validators_are_provider_neutral() {
        validate_storage_path("avatars/alice.png").unwrap();
        assert!(validate_storage_path("/avatars").is_err());
        validate_content_type("image/png").unwrap();
        assert!(validate_content_type("").is_err());
        assert!(validate_content_type("image").is_err());
        assert!(validate_content_type("image/png\n").is_err());
    }

    #[test]
    fn storage_paths_are_validated_before_state_changes() {
        let mut storage = InMemoryStorage::new();
        for path in ["", "/a", "a/", "a//b"] {
            let err = storage.put(path, Value::Nil).unwrap_err();
            assert!(format!("{err}").contains("invalid storage path"), "{path}");
        }
        assert!(storage.is_empty());
    }

    #[test]
    fn unsupported_list_is_an_explicit_error() {
        struct PutGetOnly;
        impl Storage for PutGetOnly {
            fn put(&mut self, path: &str, data: Value) -> Result<StorageObject, Box<dyn Error>> {
                Ok(StorageObject::new(path, data))
            }

            fn get(&self, _path: &str) -> Result<Option<StorageObject>, Box<dyn Error>> {
                Ok(None)
            }

            fn delete(&mut self, _path: &str) -> Result<bool, Box<dyn Error>> {
                Ok(false)
            }
        }

        let err = PutGetOnly.list("").unwrap_err();
        assert!(format!("{err}").contains("does not support list"));
    }
}
