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

    /// List objects below a prefix.  Providers that cannot list may keep the
    /// default and return an empty list; the in-memory implementation returns
    /// a deterministic path-sorted list.
    fn list(&self, prefix: &str) -> Result<Vec<StorageObject>, Box<dyn Error>> {
        let _ = prefix;
        Ok(vec![])
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

    fn delete(&mut self, path: &str) -> Result<bool, Box<dyn Error>> {
        let path = normalize_path(path)?;
        Ok(self.objects.remove(&path).is_some())
    }

    fn list(&self, prefix: &str) -> Result<Vec<StorageObject>, Box<dyn Error>> {
        let prefix = normalize_prefix(prefix)?;
        Ok(self
            .objects
            .values()
            .filter(|object| object.path.starts_with(&prefix))
            .cloned()
            .collect())
    }
}

/// Local Firebase Storage emulator adapter.
///
/// Like the Auth emulator adapter, this implementation is a local, in-memory
/// adapter with the same public contract as a network adapter.  It is useful
/// in emulator-free tests and can be replaced by an HTTP implementation
/// without changing scripts or `Runtime`.
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

    pub fn from_env() -> Option<Self> {
        let project_id = std::env::var("FIREBASE_PROJECT_ID")
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

/// Explicit name for code that wants to document that it is using the local
/// Firebase emulator adapter rather than the generic fake.
pub type FirebaseStorageEmulator = StorageEmulator;

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
    normalize_path(prefix)
}
