//! Cloud Storage backend contract and the in-memory implementation.
//!
//! Storage object names are deliberately different from Firestore document
//! paths: an object name is a sequence of one or more non-empty slash-
//! separated segments and has no collection/document parity rule.

mod emulator;
mod memory;

pub use emulator::{
    HttpStorageTransport, StorageEmulator, StorageRequest, StorageResponse, StorageTransport,
};

/// Short aliases for callers that name the transport after its role.
pub type HttpTransport = HttpStorageTransport;
pub type StorageHttpTransport = HttpStorageTransport;
pub use memory::InMemoryStorage;

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use crate::value::Value;

/// The stable metadata surface exposed by the MVP Storage API.
///
/// Optional server fields are represented as `Option<String>` in Rust and as
/// `nil` in the script Map. `content_type` and `size` are always populated.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StorageMetadata {
    pub name: String,
    pub bucket: String,
    pub content_type: String,
    pub size: u64,
    pub generation: String,
    pub metageneration: String,
    pub time_created: Option<String>,
    pub updated: Option<String>,
    pub md5_hash: Option<String>,
    pub crc32c: Option<String>,
    pub etag: Option<String>,
    pub cache_control: Option<String>,
    pub content_disposition: Option<String>,
    /// User metadata returned by the Storage API. Values are strings in the
    /// MVP, just as they are in the Firebase JSON representation.
    pub metadata: BTreeMap<String, String>,
}

impl StorageMetadata {
    /// Convert backend metadata into the Map shape returned by scripts.
    pub fn to_value(&self) -> Value {
        let mut out = BTreeMap::new();
        out.insert("name".to_string(), Value::Str(self.name.clone()));
        out.insert("bucket".to_string(), Value::Str(self.bucket.clone()));
        out.insert(
            "content_type".to_string(),
            Value::Str(self.content_type.clone()),
        );
        out.insert("size".to_string(), Value::Int(clamp_i64(self.size)));
        out.insert(
            "generation".to_string(),
            Value::Str(self.generation.clone()),
        );
        out.insert(
            "metageneration".to_string(),
            Value::Str(self.metageneration.clone()),
        );
        out.insert(
            "time_created".to_string(),
            optional_string_value(self.time_created.as_deref()),
        );
        out.insert(
            "updated".to_string(),
            optional_string_value(self.updated.as_deref()),
        );
        out.insert(
            "md5_hash".to_string(),
            optional_string_value(self.md5_hash.as_deref()),
        );
        out.insert(
            "crc32c".to_string(),
            optional_string_value(self.crc32c.as_deref()),
        );
        out.insert(
            "etag".to_string(),
            optional_string_value(self.etag.as_deref()),
        );
        out.insert(
            "cache_control".to_string(),
            optional_string_value(self.cache_control.as_deref()),
        );
        out.insert(
            "content_disposition".to_string(),
            optional_string_value(self.content_disposition.as_deref()),
        );
        out.insert(
            "metadata".to_string(),
            Value::Map(
                self.metadata
                    .iter()
                    .map(|(k, v)| (k.clone(), Value::Str(v.clone())))
                    .collect(),
            ),
        );
        Value::Map(out)
    }
}

fn optional_string_value(value: Option<&str>) -> Value {
    value
        .map(|value| Value::Str(value.to_string()))
        .unwrap_or(Value::Nil)
}

/// Script numbers are i64. A Storage object can theoretically be larger than
/// i64::MAX; metadata still remains lossless in the backend, while the script
/// representation saturates rather than wrapping.
fn clamp_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// A categorized error makes path, content, metadata, and missing-object
/// failures distinguishable without forcing callers to parse a transport's
/// status string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageErrorKind {
    InvalidPath,
    InvalidContentType,
    InvalidData,
    NotFound,
    InvalidMetadata,
    Backend,
}

impl fmt::Display for StorageErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::InvalidPath => "invalid-path",
            Self::InvalidContentType => "invalid-content-type",
            Self::InvalidData => "invalid-data",
            Self::NotFound => "not-found",
            Self::InvalidMetadata => "invalid-metadata",
            Self::Backend => "backend",
        };
        f.write_str(name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageError {
    pub kind: StorageErrorKind,
    pub message: String,
    pub path: Option<String>,
}

impl StorageError {
    pub fn new(kind: StorageErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            path: None,
        }
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn invalid_path(path: &str, reason: &str) -> Self {
        Self::new(
            StorageErrorKind::InvalidPath,
            format!("invalid storage object path: {path} ({reason})"),
        )
        .with_path(path)
    }

    pub fn invalid_content_type(content_type: &str) -> Self {
        Self::new(
            StorageErrorKind::InvalidContentType,
            format!(
                "invalid storage content type: {content_type:?} (expected a non-empty media type)"
            ),
        )
    }

    pub fn invalid_data(message: impl Into<String>) -> Self {
        Self::new(StorageErrorKind::InvalidData, message)
    }

    pub fn not_found(path: &str) -> Self {
        Self::new(
            StorageErrorKind::NotFound,
            format!("storage object not found: {path}"),
        )
        .with_path(path)
    }

    pub fn invalid_metadata(message: impl Into<String>) -> Self {
        Self::new(StorageErrorKind::InvalidMetadata, message)
    }

    pub fn backend(message: impl Into<String>) -> Self {
        Self::new(StorageErrorKind::Backend, message)
    }
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for StorageError {}

/// CRUD contract shared by the in-memory backend and the emulator adapter.
pub trait Storage {
    fn upload(
        &mut self,
        path: &str,
        data: &[u8],
        content_type: &str,
    ) -> Result<StorageMetadata, Box<dyn Error>>;

    fn download(&self, path: &str) -> Result<Vec<u8>, Box<dyn Error>>;

    fn metadata(&self, path: &str) -> Result<StorageMetadata, Box<dyn Error>>;

    fn delete(&mut self, path: &str) -> Result<(), Box<dyn Error>>;
}

// This forwarding implementation makes the same helper useful with a
// borrowed test backend (`run_script_with_storage(source, &mut storage)`)
// without forcing callers to give up ownership of their fixture.
impl<T: Storage + ?Sized> Storage for Box<T> {
    fn upload(
        &mut self,
        path: &str,
        data: &[u8],
        content_type: &str,
    ) -> Result<StorageMetadata, Box<dyn Error>> {
        (**self).upload(path, data, content_type)
    }

    fn download(&self, path: &str) -> Result<Vec<u8>, Box<dyn Error>> {
        (**self).download(path)
    }

    fn metadata(&self, path: &str) -> Result<StorageMetadata, Box<dyn Error>> {
        (**self).metadata(path)
    }

    fn delete(&mut self, path: &str) -> Result<(), Box<dyn Error>> {
        (**self).delete(path)
    }
}

impl<T: Storage + ?Sized> Storage for &mut T {
    fn upload(
        &mut self,
        path: &str,
        data: &[u8],
        content_type: &str,
    ) -> Result<StorageMetadata, Box<dyn Error>> {
        (**self).upload(path, data, content_type)
    }

    fn download(&self, path: &str) -> Result<Vec<u8>, Box<dyn Error>> {
        (**self).download(path)
    }

    fn metadata(&self, path: &str) -> Result<StorageMetadata, Box<dyn Error>> {
        (**self).metadata(path)
    }

    fn delete(&mut self, path: &str) -> Result<(), Box<dyn Error>> {
        (**self).delete(path)
    }
}

/// Validate an object name. Unlike `doc()` / `col()`, no segment parity is
/// imposed: `avatar`, `avatars/alice.png`, and `a/b/c` are all valid.
pub fn validate_storage_path(path: &str) -> Result<(), Box<dyn Error>> {
    if path.is_empty() {
        return Err(StorageError::invalid_path(path, "path must not be empty").into());
    }
    if path.len() > 1024 {
        return Err(StorageError::invalid_path(path, "path exceeds 1024 bytes").into());
    }
    if path.starts_with('/') || path.ends_with('/') {
        return Err(
            StorageError::invalid_path(path, "leading or trailing '/' is not allowed").into(),
        );
    }
    if path.contains("//") {
        return Err(StorageError::invalid_path(path, "empty path segment").into());
    }
    if path.contains('\0') {
        return Err(StorageError::invalid_path(path, "NUL is not allowed").into());
    }
    if path.chars().any(|c| c.is_control()) {
        return Err(StorageError::invalid_path(path, "control characters are not allowed").into());
    }
    Ok(())
}

/// Content types are kept deliberately small and deterministic for MVP. A
/// media type may include parameters (`; charset=utf-8`); the essence and
/// control characters are validated, while optional parameter whitespace is
/// allowed.
pub fn validate_content_type(content_type: &str) -> Result<(), Box<dyn Error>> {
    let trimmed = content_type.trim();
    if trimmed.is_empty() {
        return Err(StorageError::invalid_content_type(content_type).into());
    }
    if content_type != trimmed || !trimmed.is_ascii() || trimmed.chars().any(|c| c.is_control()) {
        return Err(StorageError::invalid_content_type(content_type).into());
    }
    let Some((essence, parameters)) = trimmed.split_once(';') else {
        if !trimmed.chars().any(char::is_whitespace) && is_media_essence(trimmed) {
            return Ok(());
        }
        return Err(StorageError::invalid_content_type(content_type).into());
    };
    if essence.chars().any(char::is_whitespace)
        || !is_media_essence(essence)
        || parameters.trim().is_empty()
        || parameters.contains(';')
    {
        return Err(StorageError::invalid_content_type(content_type).into());
    }
    Ok(())
}

/// A media-type essence is exactly one `type/subtype` pair whose parts are
/// RFC 9110 tokens. Values such as `image/png/extra` are not media types and
/// must be rejected so both backends share the same validation contract.
fn is_media_essence(essence: &str) -> bool {
    let mut parts = essence.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(main), Some(sub), None) => is_rfc9110_token(main) && is_rfc9110_token(sub),
        _ => false,
    }
}

fn is_rfc9110_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+' | b'-' | b'.' | b'^'
                        | b'_' | b'`' | b'|' | b'~'
                )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_paths_are_not_firestore_document_paths() {
        for path in ["avatar", "avatars/alice.png", "a/b/c"] {
            validate_storage_path(path).unwrap();
        }
        for path in ["", "/avatar", "avatar/", "a//b", "a/\0b"] {
            let err = validate_storage_path(path).unwrap_err();
            assert!(
                format!("{err}").contains("invalid storage object path"),
                "{err}"
            );
        }
    }

    #[test]
    fn content_type_validation_is_clear() {
        validate_content_type("image/png").unwrap();
        validate_content_type("text/plain; charset=utf-8").unwrap();
        for value in [
            "",
            "image png",
            "image/png;",
            "image/",
            "image/png/extra",
            "image//png",
            "/png",
            "ima ge/png",
        ] {
            let err = validate_content_type(value).unwrap_err();
            assert!(
                format!("{err}").contains("invalid storage content type"),
                "{err}"
            );
        }
    }
}
