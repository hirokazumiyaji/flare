//! Firebase Storage emulator adapter.
//!
//! The Firebase Storage REST emulator API is small enough for the MVP to use
//! a synchronous, dependency-free HTTP/1.1 transport. `StorageTransport` is a
//! seam for deterministic fake-transport tests; production/authenticated
//! Storage is intentionally not wired here.

use std::collections::BTreeMap;
use std::error::Error;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

/// Upper bound for establishing the emulator TCP connection. An unreachable
/// or blackholed host must fail fast instead of stalling startup for the
/// operating system's multi-minute TCP timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

use crate::limits::DEFAULT_MAX_BYTES_SIZE;

/// Default maximum allowed body size for storage emulator responses (50 MiB).
pub const DEFAULT_MAX_STORAGE_BODY_BYTES: usize = DEFAULT_MAX_BYTES_SIZE;

/// Additional byte allowance for HTTP status line and response headers (64 KiB).
pub const STORAGE_HEADER_ALLOWANCE_BYTES: usize = 64 * 1024;

/// Default maximum allowed total raw response size (headers + body).
pub const DEFAULT_MAX_STORAGE_RESPONSE_BYTES: usize =
    DEFAULT_MAX_STORAGE_BODY_BYTES + STORAGE_HEADER_ALLOWANCE_BYTES;

use serde_json::Value as JsonValue;

use super::{validate_content_type, validate_storage_path, Storage, StorageError, StorageMetadata};

/// A request made by [`StorageEmulator`]. Paths are already URL escaped and
/// do not include the emulator host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageRequest {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl StorageRequest {
    pub fn new(method: impl Into<String>, target: impl Into<String>, body: Vec<u8>) -> Self {
        Self {
            method: method.into(),
            target: target.into(),
            headers: vec![],
            body,
        }
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// A transport response. The body is kept as bytes because downloads may not
/// be valid UTF-8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl StorageResponse {
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: vec![],
            body: body.into(),
        }
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Synchronous transport seam. A fake can implement this without opening a
/// socket, while the production constructor uses [`HttpStorageTransport`].
pub trait StorageTransport {
    fn send(&self, request: &StorageRequest) -> Result<StorageResponse, Box<dyn Error>>;
    fn set_timeout(&mut self, _timeout: Option<Duration>) {}
    fn set_limits(&mut self, _max_body_bytes: Option<usize>, _header_allowance: usize) {}
}

/// Minimal HTTP transport for `FIREBASE_STORAGE_EMULATOR_HOST` (which is an
/// HTTP endpoint, normally `127.0.0.1:9199`).
pub struct HttpStorageTransport {
    address: String,
    host_header: String,
    timeout: Option<Duration>,
    max_body_bytes: Option<usize>,
    max_response_bytes: Option<usize>,
}

impl HttpStorageTransport {
    pub fn new(host: impl AsRef<str>) -> Result<Self, StorageError> {
        let raw = host.as_ref().trim();
        if raw.is_empty() || raw.chars().any(|c| c.is_control()) {
            return Err(StorageError::backend(
                "storage emulator host must not be empty",
            ));
        }
        let without_scheme = if let Some(rest) = raw.strip_prefix("http://") {
            rest
        } else if raw.starts_with("https://") {
            return Err(StorageError::backend(
                "storage emulator host must use http:// (TLS is not supported by the local emulator transport)",
            ));
        } else {
            raw
        };
        let without_path = without_scheme.trim_end_matches('/');
        if without_path.is_empty() || without_path.contains('/') {
            return Err(StorageError::backend(format!(
                "invalid storage emulator host: {raw} (expected host:port)"
            )));
        }
        // `TcpStream::connect` accepts the familiar host:port spelling,
        // including bracketed IPv6 addresses.
        Ok(Self {
            address: without_path.to_string(),
            host_header: without_path.to_string(),
            timeout: Some(Duration::from_secs(30)),
            max_body_bytes: Some(DEFAULT_MAX_STORAGE_BODY_BYTES),
            max_response_bytes: Some(DEFAULT_MAX_STORAGE_RESPONSE_BYTES),
        })
    }

    pub fn with_limits(
        mut self,
        max_body_bytes: impl Into<Option<usize>>,
        header_allowance: usize,
    ) -> Self {
        self.set_limits(max_body_bytes, header_allowance);
        self
    }

    pub fn set_limits(
        &mut self,
        max_body_bytes: impl Into<Option<usize>>,
        header_allowance: usize,
    ) {
        let max_body_bytes = max_body_bytes.into();
        self.max_body_bytes = max_body_bytes;
        self.max_response_bytes = max_body_bytes.map(|max| max.saturating_add(header_allowance));
    }

    pub fn max_body_bytes(&self) -> Option<usize> {
        self.max_body_bytes
    }

    pub fn max_response_bytes(&self) -> Option<usize> {
        self.max_response_bytes
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    /// Verify that the configured emulator endpoint accepts TCP connections.
    /// The actual HTTP request is still deferred until a CRUD operation.
    pub fn probe(&self) -> Result<(), StorageError> {
        self.connect().map(|_| ()).map_err(|e| {
            StorageError::backend(format!(
                "storage emulator connection failed for {}: {e}",
                self.address
            ))
        })
    }

    fn connect(&self) -> Result<TcpStream, std::io::Error> {
        self.connect_with_deadline(Some(Instant::now() + CONNECT_TIMEOUT))
    }

    fn connect_with_deadline(
        &self,
        deadline: Option<Instant>,
    ) -> Result<TcpStream, std::io::Error> {
        let mut last_err = std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("storage emulator host did not resolve: {}", self.address),
        );
        for addr in self.address.to_socket_addrs()? {
            let timeout = deadline
                .map(|deadline| deadline.saturating_duration_since(Instant::now()))
                .unwrap_or(CONNECT_TIMEOUT);
            if timeout.is_zero() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "operation deadline elapsed",
                ));
            }
            match TcpStream::connect_timeout(&addr, timeout.min(CONNECT_TIMEOUT)) {
                Ok(stream) => return Ok(stream),
                Err(e) => last_err = e,
            }
        }
        Err(last_err)
    }

    fn send_http(&self, request: &StorageRequest) -> Result<StorageResponse, StorageError> {
        let deadline = self
            .timeout
            .and_then(|timeout| Instant::now().checked_add(timeout));
        let mut stream = self.connect_with_deadline(deadline).map_err(|e| {
            StorageError::backend(format!(
                "storage emulator connection failed for {}: {e}",
                self.address
            ))
        })?;
        let mut wire = format!(
            "{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
            request.method, request.target, self.host_header
        );
        for (name, value) in &request.headers {
            if name.eq_ignore_ascii_case("host") || name.eq_ignore_ascii_case("connection") {
                continue;
            }
            wire.push_str(name);
            wire.push_str(": ");
            wire.push_str(value);
            wire.push_str("\r\n");
        }
        if !request
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        {
            wire.push_str("Content-Length: ");
            wire.push_str(&request.body.len().to_string());
            wire.push_str("\r\n");
        }
        wire.push_str("\r\n");
        write_with_deadline(&mut stream, wire.as_bytes(), deadline)?;
        write_with_deadline(&mut stream, &request.body, deadline)?;

        let mut raw = Vec::new();
        let mut buffer = [0_u8; 8192];
        loop {
            set_remaining_timeout(&stream, deadline, true)?;
            let read = stream.read(&mut buffer).map_err(|e| {
                if is_timeout_error(&e) {
                    StorageError::backend("storage emulator operation timed out")
                } else {
                    StorageError::backend(format!("storage emulator response read failed: {e}"))
                }
            })?;
            if read == 0 {
                break;
            }
            if let Some(max_response_bytes) = self.max_response_bytes {
                if raw.len().saturating_add(read) > max_response_bytes {
                    return Err(StorageError::backend(format!(
                        "storage emulator response size limit exceeded (max {max_response_bytes} bytes)",
                    )));
                }
            }
            raw.extend_from_slice(&buffer[..read]);
            // A response with Content-Length can be decoded before the peer
            // closes the socket. This also avoids waiting for keep-alive peers.
            match parse_http_response_with_limit(&raw, self.max_body_bytes) {
                Ok((_, complete)) => {
                    if complete {
                        break;
                    }
                }
                Err(e) => {
                    if find_bytes(&raw, b"\r\n\r\n").is_none() {
                        if raw.len() > STORAGE_HEADER_ALLOWANCE_BYTES {
                            return Err(StorageError::backend(
                                "storage emulator response headers exceeded size limit",
                            ));
                        }
                    } else {
                        return Err(e);
                    }
                }
            }
        }
        let (response, _) = parse_http_response_with_limit(&raw, self.max_body_bytes)?;
        Ok(response)
    }
}

fn set_remaining_timeout(
    stream: &TcpStream,
    deadline: Option<Instant>,
    read: bool,
) -> Result<(), StorageError> {
    let timeout = deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
    if timeout.is_some_and(|timeout| timeout.is_zero()) {
        return Err(StorageError::backend(
            "storage emulator operation timed out",
        ));
    }
    let result = if read {
        stream.set_read_timeout(timeout)
    } else {
        stream.set_write_timeout(timeout)
    };
    result.map_err(|error| {
        StorageError::backend(format!("storage emulator timeout setup failed: {error}"))
    })
}

fn is_timeout_error(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    )
}

fn write_with_deadline(
    stream: &mut TcpStream,
    mut bytes: &[u8],
    deadline: Option<Instant>,
) -> Result<(), StorageError> {
    while !bytes.is_empty() {
        set_remaining_timeout(stream, deadline, false)?;
        let written = stream.write(bytes).map_err(|error| {
            StorageError::backend(format!("storage emulator request failed: {error}"))
        })?;
        if written == 0 {
            return Err(StorageError::backend(
                "storage emulator request failed: write returned zero",
            ));
        }
        bytes = &bytes[written..];
    }
    Ok(())
}

impl StorageTransport for HttpStorageTransport {
    fn send(&self, request: &StorageRequest) -> Result<StorageResponse, Box<dyn Error>> {
        self.send_http(request).map_err(Into::into)
    }

    fn set_timeout(&mut self, timeout: Option<Duration>) {
        self.timeout = timeout;
    }

    fn set_limits(&mut self, max_body_bytes: Option<usize>, header_allowance: usize) {
        HttpStorageTransport::set_limits(self, max_body_bytes, header_allowance);
    }
}

/// Storage emulator backend. `T` is replaceable to make the REST contract
/// testable without requiring Firebase CLI in every unit-test environment.
pub struct StorageEmulator<T = HttpStorageTransport> {
    transport: T,
    project_id: String,
    bucket: String,
    auth_token: Option<String>,
    max_bytes_size: Option<usize>,
}

impl StorageEmulator<HttpStorageTransport> {
    /// Build a client for the local Storage emulator. The TCP endpoint is
    /// checked immediately, while the HTTP CRUD request remains lazy.
    pub fn new(host: impl AsRef<str>, project_id: impl Into<String>) -> Result<Self, StorageError> {
        let transport = HttpStorageTransport::new(host)?;
        let client = Self::with_transport(project_id, transport)?;
        client.transport.probe()?;
        Ok(client)
    }

    /// Read `FIREBASE_STORAGE_EMULATOR_HOST` and a project ID. The project
    /// ID lookup intentionally supports the Firebase CLI's common variable
    /// names, with the same local-test default as Firestore.
    pub fn from_env() -> Option<Self> {
        Self::try_from_env().ok().flatten()
    }

    pub fn try_from_env() -> Result<Option<Self>, Box<dyn Error>> {
        let Some(host) = std::env::var("FIREBASE_STORAGE_EMULATOR_HOST").ok() else {
            return Ok(None);
        };
        let project_id = std::env::var("FIREBASE_PROJECT_ID")
            .or_else(|_| std::env::var("GCLOUD_PROJECT"))
            .or_else(|_| std::env::var("FIRESTORE_PROJECT_ID"))
            .unwrap_or_else(|_| "flare-test".to_string());
        let mut client = Self::new(host, project_id)?;
        if let Ok(bucket) = std::env::var("FIREBASE_STORAGE_BUCKET") {
            client = client.with_bucket(bucket)?;
        }
        if let Ok(token) = std::env::var("FIREBASE_STORAGE_AUTH_TOKEN") {
            client = client.with_auth_token(token)?;
        }
        Ok(Some(client))
    }
}

impl<T: StorageTransport> StorageEmulator<T> {
    pub fn with_transport(
        project_id: impl Into<String>,
        mut transport: T,
    ) -> Result<Self, StorageError> {
        let project_id = project_id.into();
        if project_id.trim().is_empty()
            || project_id.contains('/')
            || project_id
                .chars()
                .any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(StorageError::backend(format!(
                "invalid storage project id: {project_id:?}"
            )));
        }
        transport.set_limits(
            Some(DEFAULT_MAX_STORAGE_BODY_BYTES),
            STORAGE_HEADER_ALLOWANCE_BYTES,
        );
        let bucket = format!("{project_id}.appspot.com");
        Ok(Self {
            transport,
            project_id,
            bucket,
            auth_token: None,
            max_bytes_size: Some(DEFAULT_MAX_STORAGE_BODY_BYTES),
        })
    }

    pub fn new_with_transport(
        project_id: impl Into<String>,
        transport: T,
    ) -> Result<Self, StorageError> {
        Self::with_transport(project_id, transport)
    }

    pub fn from_transport(
        project_id: impl Into<String>,
        transport: T,
    ) -> Result<Self, StorageError> {
        Self::with_transport(project_id, transport)
    }

    pub fn with_max_bytes_size(mut self, max: impl Into<Option<usize>>) -> Self {
        self.set_max_bytes_size(max);
        self
    }

    pub fn set_max_bytes_size(&mut self, max: impl Into<Option<usize>>) {
        let max = max.into();
        self.max_bytes_size = max;
        self.transport
            .set_limits(max, STORAGE_HEADER_ALLOWANCE_BYTES);
    }

    pub fn max_bytes_size(&self) -> Option<usize> {
        self.max_bytes_size
    }

    /// Override the emulator bucket. Normally it is `{project_id}.appspot.com`.
    pub fn with_bucket(mut self, bucket: impl Into<String>) -> Result<Self, StorageError> {
        let bucket = bucket.into();
        if bucket.trim().is_empty()
            || bucket.contains('/')
            || bucket.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(StorageError::backend(format!(
                "invalid storage bucket: {bucket:?}"
            )));
        }
        self.bucket = bucket;
        Ok(self)
    }

    /// Supply an emulator bearer token when a rules test needs an auth
    /// identity. With no token, the request is deliberately unauthenticated
    /// and the local rules decide the result. This is not a production auth
    /// client and does not discover Firebase ID tokens.
    pub fn with_auth_token(mut self, token: impl Into<String>) -> Result<Self, StorageError> {
        let token = token.into();
        if !token.is_ascii()
            || token.trim().is_empty()
            || token != token.trim()
            || token.chars().any(|c| c.is_control())
        {
            return Err(StorageError::backend(
                "storage emulator auth token must be a non-empty header-safe string",
            ));
        }
        self.auth_token = Some(token);
        Ok(self)
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    fn object_target(&self, path: &str, media: bool) -> String {
        let mut target = format!(
            "/v0/b/{}/o/{}",
            percent_encode(&self.bucket),
            percent_encode(path)
        );
        if media {
            target.push_str("?alt=media");
        }
        target
    }

    fn upload_target(&self, path: &str) -> String {
        format!(
            "/v0/b/{}/o?uploadType=media&name={}",
            percent_encode(&self.bucket),
            percent_encode(path)
        )
    }

    fn request_headers(&self, content_type: Option<&str>) -> Vec<(String, String)> {
        let mut headers = vec![
            ("Accept".to_string(), "application/json".to_string()),
            ("X-Goog-Upload-Protocol".to_string(), "raw".to_string()),
        ];
        if let Some(content_type) = content_type {
            headers.push(("Content-Type".to_string(), content_type.to_string()));
        }
        if let Some(token) = &self.auth_token {
            let value = if token.starts_with("Bearer ") {
                token.clone()
            } else {
                format!("Bearer {token}")
            };
            headers.push(("Authorization".to_string(), value));
        }
        headers
    }

    fn send_checked(
        &self,
        request: &StorageRequest,
        path: &str,
    ) -> Result<StorageResponse, Box<dyn Error>> {
        let response = self.transport.send(request)?;
        if let Some(max) = self.max_bytes_size {
            if response.body.len() > max {
                return Err(StorageError::backend(format!(
                    "storage emulator response body size exceeds limit: {} (max {max})",
                    response.body.len()
                ))
                .into());
            }
        }
        if (200..300).contains(&response.status) {
            return Ok(response);
        }
        if response.status == 404 {
            return Err(StorageError::not_found(path).into());
        }
        if response.status == 413 {
            return Err(StorageError::invalid_data(
                "storage object exceeds the emulator size limit",
            )
            .into());
        }
        if response.status == 415 {
            return Err(
                StorageError::invalid_content_type("emulator rejected content type").into(),
            );
        }
        Err(StorageError::backend(format!(
            "storage emulator request failed with HTTP {}",
            response.status
        ))
        .into())
    }
}

impl<T: StorageTransport> Storage for StorageEmulator<T> {
    fn set_timeout(&mut self, timeout: Option<Duration>) {
        self.transport.set_timeout(timeout);
    }

    fn set_max_bytes_size(&mut self, max_bytes_size: Option<usize>) {
        self.set_max_bytes_size(max_bytes_size);
    }

    fn supports_backend_timeout(&self) -> bool {
        false
    }

    fn upload(
        &mut self,
        path: &str,
        data: &[u8],
        content_type: &str,
    ) -> Result<StorageMetadata, Box<dyn Error>> {
        validate_storage_path(path)?;
        validate_content_type(content_type)?;
        let request = StorageRequest {
            method: "POST".to_string(),
            target: self.upload_target(path),
            headers: self.request_headers(Some(content_type)),
            body: data.to_vec(),
        };
        let response = self.send_checked(&request, path)?;
        parse_metadata(
            &response.body,
            path,
            &self.bucket,
            Some(data.len() as u64),
            Some(content_type),
        )
    }

    fn download(&self, path: &str) -> Result<Vec<u8>, Box<dyn Error>> {
        validate_storage_path(path)?;
        let request = StorageRequest {
            method: "GET".to_string(),
            target: self.object_target(path, true),
            headers: self.request_headers(None),
            body: vec![],
        };
        Ok(self.send_checked(&request, path)?.body)
    }

    fn metadata(&self, path: &str) -> Result<StorageMetadata, Box<dyn Error>> {
        validate_storage_path(path)?;
        let request = StorageRequest {
            method: "GET".to_string(),
            target: self.object_target(path, false),
            headers: self.request_headers(None),
            body: vec![],
        };
        let response = self.send_checked(&request, path)?;
        parse_metadata(&response.body, path, &self.bucket, None, None)
    }

    fn delete(&mut self, path: &str) -> Result<(), Box<dyn Error>> {
        validate_storage_path(path)?;
        let request = StorageRequest {
            method: "DELETE".to_string(),
            target: self.object_target(path, false),
            headers: self.request_headers(None),
            body: vec![],
        };
        self.send_checked(&request, path)?;
        Ok(())
    }
}

fn parse_metadata(
    body: &[u8],
    path: &str,
    bucket: &str,
    expected_size: Option<u64>,
    expected_content_type: Option<&str>,
) -> Result<StorageMetadata, Box<dyn Error>> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Err(StorageError::invalid_metadata(
            "invalid storage metadata: response body is empty",
        )
        .into());
    }
    let json: JsonValue = serde_json::from_slice(body).map_err(|e| {
        StorageError::invalid_metadata(format!("invalid storage metadata JSON: {e}"))
    })?;
    let Some(object) = json.as_object() else {
        return Err(StorageError::invalid_metadata(
            "invalid storage metadata: expected a JSON object",
        )
        .into());
    };
    for required in ["name", "contentType", "size"] {
        if object.get(required).is_none() || object.get(required) == Some(&JsonValue::Null) {
            return Err(StorageError::invalid_metadata(format!(
                "invalid storage metadata: missing field {required}"
            ))
            .into());
        }
    }

    let name =
        optional_json_string(object.get("name"), "name")?.unwrap_or_else(|| path.to_string());
    if name != path {
        return Err(StorageError::invalid_metadata(format!(
            "storage metadata name mismatch: requested {path}, got {name}"
        ))
        .into());
    }
    let content_type = optional_json_string(object.get("contentType"), "contentType")?
        .or_else(|| expected_content_type.map(str::to_string))
        .unwrap_or_else(|| "application/octet-stream".to_string());
    if let Some(expected) = expected_content_type {
        if content_type != expected {
            return Err(StorageError::invalid_metadata(format!(
                "storage content type mismatch for {path}: expected {expected}, got {content_type}"
            ))
            .into());
        }
    }
    let size = match object.get("size") {
        Some(value) => parse_size(value, "size")?,
        None => expected_size.unwrap_or(0),
    };
    if size > i64::MAX as u64 {
        return Err(StorageError::invalid_metadata(format!(
            "storage metadata size for {path} exceeds the script Int range"
        ))
        .into());
    }
    if let Some(expected) = expected_size {
        if size != expected {
            return Err(StorageError::invalid_metadata(format!(
                "storage metadata size mismatch for {path}: expected {expected}, got {size}"
            ))
            .into());
        }
    }
    let generation = optional_json_string(object.get("generation"), "generation")?
        .unwrap_or_else(|| "0".to_string());
    let metageneration = optional_json_string(object.get("metageneration"), "metageneration")?
        .unwrap_or_else(|| "1".to_string());
    let custom_metadata = match object.get("metadata") {
        None | Some(JsonValue::Null) => BTreeMap::new(),
        Some(JsonValue::Object(values)) => {
            let mut out = BTreeMap::new();
            for (key, value) in values {
                let value =
                    optional_json_string(Some(value), "metadata value")?.ok_or_else(|| {
                        StorageError::invalid_metadata(format!(
                            "invalid storage metadata value for {key:?}"
                        ))
                    })?;
                out.insert(key.clone(), value);
            }
            out
        }
        Some(_) => {
            return Err(StorageError::invalid_metadata(
                "invalid storage metadata: metadata must be an object",
            )
            .into())
        }
    };
    Ok(StorageMetadata {
        name,
        bucket: optional_json_string(object.get("bucket"), "bucket")?
            .unwrap_or_else(|| bucket.to_string()),
        content_type,
        size,
        generation,
        metageneration,
        time_created: optional_json_string(object.get("timeCreated"), "timeCreated")?,
        updated: optional_json_string(object.get("updated"), "updated")?,
        md5_hash: optional_json_string(object.get("md5Hash"), "md5Hash")?,
        crc32c: optional_json_string(object.get("crc32c"), "crc32c")?,
        etag: optional_json_string(object.get("etag"), "etag")?,
        cache_control: optional_json_string(object.get("cacheControl"), "cacheControl")?,
        content_disposition: optional_json_string(
            object.get("contentDisposition"),
            "contentDisposition",
        )?,
        metadata: custom_metadata,
    })
}

fn parse_size(value: &JsonValue, field: &str) -> Result<u64, Box<dyn Error>> {
    match value {
        JsonValue::Number(number) => number.as_u64().ok_or_else(|| {
            StorageError::invalid_metadata(format!(
                "invalid storage metadata {field}: expected a non-negative integer"
            ))
            .into()
        }),
        JsonValue::String(text) => text.parse::<u64>().map_err(|e| {
            StorageError::invalid_metadata(format!("invalid storage metadata {field}: {e}")).into()
        }),
        _ => Err(StorageError::invalid_metadata(format!(
            "invalid storage metadata {field}: expected a number"
        ))
        .into()),
    }
}

fn optional_json_string(
    value: Option<&JsonValue>,
    field: &str,
) -> Result<Option<String>, Box<dyn Error>> {
    match value {
        None | Some(JsonValue::Null) => Ok(None),
        Some(JsonValue::String(value)) => Ok(Some(value.clone())),
        Some(JsonValue::Number(value)) => Ok(Some(value.to_string())),
        Some(_) => Err(StorageError::invalid_metadata(format!(
            "invalid storage metadata {field}: expected string"
        ))
        .into()),
    }
}

fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

pub fn parse_http_response(raw: &[u8]) -> Result<(StorageResponse, bool), StorageError> {
    parse_http_response_with_limit(raw, Some(DEFAULT_MAX_STORAGE_BODY_BYTES))
}

pub fn parse_http_response_with_limit(
    raw: &[u8],
    max_body_bytes: impl Into<Option<usize>>,
) -> Result<(StorageResponse, bool), StorageError> {
    let max_body_bytes = max_body_bytes.into();
    let Some(header_end) = find_bytes(raw, b"\r\n\r\n") else {
        return Err(StorageError::backend(
            "invalid storage emulator response: missing HTTP header terminator",
        ));
    };
    if header_end > STORAGE_HEADER_ALLOWANCE_BYTES {
        return Err(StorageError::backend(
            "storage emulator response headers exceeded size limit",
        ));
    }
    let header_text = std::str::from_utf8(&raw[..header_end]).map_err(|e| {
        StorageError::backend(format!("invalid storage emulator response headers: {e}"))
    })?;
    let mut lines = header_text.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let mut status_parts = status_line.splitn(3, ' ');
    let version = status_parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/") {
        return Err(StorageError::backend(format!(
            "invalid storage emulator response status line: {status_line:?}"
        )));
    }
    let status = status_parts
        .next()
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| {
            StorageError::backend(format!(
                "invalid storage emulator response status line: {status_line:?}"
            ))
        })?;
    let mut headers = vec![];
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(StorageError::backend(format!(
                "invalid storage emulator response header: {line:?}"
            )));
        };
        headers.push((name.trim().to_string(), value.trim().to_string()));
    }
    let body = &raw[header_end + 4..];
    let chunked = headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("transfer-encoding") && value.contains("chunked")
    });
    if chunked {
        let (decoded, complete) = decode_chunked_body(body, max_body_bytes)?;
        return Ok((
            StorageResponse {
                status,
                headers,
                body: decoded,
            },
            complete,
        ));
    }
    let content_length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .map(|(_, value)| {
            value.parse::<usize>().map_err(|e| {
                StorageError::backend(format!(
                    "invalid storage emulator Content-Length header: {e}"
                ))
            })
        });
    match content_length {
        Some(Ok(length)) => {
            if let Some(max) = max_body_bytes {
                if length > max {
                    return Err(StorageError::backend(format!(
                        "storage emulator response Content-Length exceeds limit: {length} (max {max})"
                    )));
                }
            }
            if body.len() < length {
                return Ok((
                    StorageResponse {
                        status,
                        headers,
                        body: body.to_vec(),
                    },
                    false,
                ));
            }
            Ok((
                StorageResponse {
                    status,
                    headers,
                    body: body[..length].to_vec(),
                },
                true,
            ))
        }
        Some(Err(error)) => Err(error),
        None => {
            if let Some(max) = max_body_bytes {
                if body.len() > max {
                    return Err(StorageError::backend(format!(
                        "storage emulator response body size exceeds limit: {} (max {max})",
                        body.len()
                    )));
                }
            }
            Ok((
                StorageResponse {
                    status,
                    headers,
                    body: body.to_vec(),
                },
                false,
            ))
        }
    }
}

fn decode_chunked_body(
    mut body: &[u8],
    max_body_bytes: Option<usize>,
) -> Result<(Vec<u8>, bool), StorageError> {
    let mut out = Vec::new();
    loop {
        let Some(line_end) = find_bytes(body, b"\r\n") else {
            return Ok((out, false));
        };
        let line = std::str::from_utf8(&body[..line_end]).map_err(|e| {
            StorageError::backend(format!("invalid storage emulator chunk header: {e}"))
        })?;
        let size_text = line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16).map_err(|e| {
            StorageError::backend(format!("invalid storage emulator chunk size: {e}"))
        })?;
        if let Some(max) = max_body_bytes {
            if out.len().saturating_add(size) > max {
                return Err(StorageError::backend(format!(
                    "storage emulator chunked response size exceeds limit (max {max} bytes)"
                )));
            }
        }
        body = &body[line_end + 2..];
        if size == 0 {
            return Ok((out, true));
        }
        if body.len() < size {
            return Ok((out, false));
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size..];
        if body.len() < 2 || &body[..2] != b"\r\n" {
            return Err(StorageError::backend(
                "invalid storage emulator chunk terminator",
            ));
        }
        body = &body[2..];
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    struct FakeTransport {
        requests: RefCell<Vec<StorageRequest>>,
        responses: RefCell<Vec<StorageResponse>>,
    }

    impl FakeTransport {
        fn new(responses: Vec<StorageResponse>) -> Self {
            Self {
                requests: RefCell::new(vec![]),
                responses: RefCell::new(responses),
            }
        }
    }

    impl StorageTransport for FakeTransport {
        fn send(&self, request: &StorageRequest) -> Result<StorageResponse, Box<dyn Error>> {
            self.requests.borrow_mut().push(request.clone());
            let mut responses = self.responses.borrow_mut();
            if responses.is_empty() {
                return Err("fake transport ran out of responses".into());
            }
            Ok(responses.remove(0))
        }
    }

    fn metadata_body() -> Vec<u8> {
        br#"{"name":"avatars/a.png","bucket":"p.appspot.com","contentType":"image/png","size":3,"generation":"7","metageneration":"1"}"#.to_vec()
    }

    #[test]
    fn new_fails_fast_without_emulator() {
        let result = StorageEmulator::new("127.0.0.1:9", "p");
        assert!(result.is_err());
    }

    #[test]
    fn fake_transport_preserves_binary_and_builds_crud_requests() {
        let transport = FakeTransport::new(vec![
            StorageResponse::new(200, metadata_body()),
            StorageResponse::new(200, vec![0x00, 0xff, 0x80]),
            StorageResponse::new(200, metadata_body()),
            StorageResponse::new(204, vec![]),
        ]);
        let mut emulator = StorageEmulator::with_transport("p", transport).unwrap();
        let data = [0x00, 0xff, 0x80];
        let metadata = emulator
            .upload("avatars/a.png", &data, "image/png")
            .unwrap();
        assert_eq!(metadata.size, 3);
        assert_eq!(emulator.download("avatars/a.png").unwrap(), data);
        assert_eq!(emulator.metadata("avatars/a.png").unwrap().generation, "7");
        emulator.delete("avatars/a.png").unwrap();
        let requests = emulator.transport().requests.borrow();
        assert_eq!(requests[0].method, "POST");
        assert!(requests[0].target.contains("uploadType=media"));
        assert!(requests[0].target.contains("name=avatars%2Fa.png"));
        assert_eq!(requests[0].body, data);
        assert_eq!(requests[0].header("Content-Type"), Some("image/png"));
        assert_eq!(requests[1].method, "GET");
        assert!(requests[1].target.ends_with("?alt=media"));
        assert_eq!(requests[2].method, "GET");
        assert!(!requests[2].target.contains("alt=media"));
        assert_eq!(requests[3].method, "DELETE");
    }

    #[test]
    fn auth_header_is_only_sent_when_explicitly_configured() {
        let transport = FakeTransport::new(vec![StorageResponse::new(200, metadata_body())]);
        let mut emulator = StorageEmulator::with_transport("p", transport)
            .unwrap()
            .with_auth_token("owner")
            .unwrap();
        emulator
            .upload("avatars/a.png", &[0, 1, 2], "image/png")
            .unwrap();
        assert_eq!(
            emulator.transport().requests.borrow()[0].header("Authorization"),
            Some("Bearer owner")
        );
    }

    #[test]
    fn http_404_is_a_storage_not_found_error() {
        let transport = FakeTransport::new(vec![StorageResponse::new(404, b"missing")]);
        let emulator = StorageEmulator::with_transport("p", transport).unwrap();
        let err = emulator.download("missing").unwrap_err();
        assert!(
            format!("{err}").contains("storage object not found"),
            "{err}"
        );
    }

    #[test]
    fn http_error_bodies_are_not_exposed() {
        let transport = FakeTransport::new(vec![StorageResponse::new(500, "private-token")]);
        let emulator = StorageEmulator::with_transport("p", transport).unwrap();
        let err = emulator.download("object").unwrap_err().to_string();
        assert!(err.contains("HTTP 500"), "{err}");
        assert!(!err.contains("private-token"), "{err}");
    }

    #[test]
    fn incremental_response_uses_one_operation_deadline() {
        let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skip: sandbox does not permit local TCP listeners");
                return;
            }
            Err(error) => panic!("could not bind local test server: {error}"),
        };
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\na")
                .unwrap();
            std::thread::sleep(Duration::from_millis(20));
            let _ = stream.write_all(b"b");
            std::thread::sleep(Duration::from_millis(20));
            let _ = stream.write_all(b"b");
        });
        let mut transport = HttpStorageTransport::new(address.to_string()).unwrap();
        transport.set_timeout(Some(Duration::from_millis(30)));
        let error = transport
            .send(&StorageRequest::new("GET", "/object", vec![]))
            .unwrap_err()
            .to_string();
        assert!(error.contains("timed out"), "{error}");
        server.join().unwrap();
    }

    #[test]
    fn metadata_size_and_name_mismatches_are_rejected() {
        let transport = FakeTransport::new(vec![StorageResponse::new(
            200,
            br#"{"name":"wrong","size":99,"contentType":"image/png"}"#,
        )]);
        let mut emulator = StorageEmulator::with_transport("p", transport).unwrap();
        let err = emulator
            .upload("avatars/a.png", &[0, 1, 2], "image/png")
            .unwrap_err();
        assert!(format!("{err}").contains("metadata name mismatch"), "{err}");
    }

    #[test]
    fn metadata_size_mismatch_is_rejected() {
        let transport = FakeTransport::new(vec![StorageResponse::new(
            200,
            br#"{"name":"avatars/a.png","contentType":"image/png","size":99}"#,
        )]);
        let mut emulator = StorageEmulator::with_transport("p", transport).unwrap();
        let err = emulator
            .upload("avatars/a.png", &[0, 1, 2], "image/png")
            .unwrap_err();
        assert!(format!("{err}").contains("size mismatch"), "{err}");
    }

    #[test]
    fn percent_encoding_preserves_unicode_and_slash_separation() {
        assert_eq!(percent_encode("a/b c"), "a%2Fb%20c");
        assert_eq!(percent_encode("é"), "%C3%A9");
    }

    #[test]
    fn fake_transport_response_exceeding_max_bytes_size_is_rejected() {
        let transport = FakeTransport::new(vec![StorageResponse::new(200, vec![0xaa; 100])]);
        let emulator = StorageEmulator::with_transport("p", transport)
            .unwrap()
            .with_max_bytes_size(50);
        let err = emulator.download("test.bin").unwrap_err().to_string();
        assert!(err.contains("response body size exceeds limit"), "{err}");
    }

    #[test]
    fn parse_http_response_rejects_massive_content_length() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 18446744073709551615\r\n\r\n";
        let err = parse_http_response_with_limit(raw, 1024)
            .unwrap_err()
            .to_string();
        assert!(err.contains("Content-Length exceeds limit"), "{err}");
    }

    #[test]
    fn parse_http_response_rejects_chunked_size_exceeding_limit() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n100\r\n";
        let err = parse_http_response_with_limit(raw, 50)
            .unwrap_err()
            .to_string();
        assert!(err.contains("chunked response size exceeds limit"), "{err}");
    }

    #[test]
    fn parse_http_response_rejects_unbounded_body_without_content_length() {
        let raw = b"HTTP/1.1 200 OK\r\n\r\n12345678901234567890";
        let err = parse_http_response_with_limit(raw, 10)
            .unwrap_err()
            .to_string();
        assert!(err.contains("response body size exceeds limit"), "{err}");
    }

    #[test]
    fn http_transport_aborts_immediately_on_content_length_exceeding_limit() {
        let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skip: sandbox does not permit local TCP listeners");
                return;
            }
            Err(error) => panic!("could not bind local test server: {error}"),
        };
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 999999999\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            // Keep connection open without writing more bytes; client must fail immediately
            std::thread::sleep(Duration::from_millis(50));
        });

        let mut transport = HttpStorageTransport::new(address.to_string())
            .unwrap()
            .with_limits(1024, 1024);
        transport.set_timeout(Some(Duration::from_secs(5)));
        let err = transport
            .send(&StorageRequest::new("GET", "/test", vec![]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("Content-Length exceeds limit"), "{err}");
        server.join().unwrap();
    }

    #[test]
    fn http_transport_aborts_when_raw_stream_exceeds_max_response_bytes() {
        let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skip: sandbox does not permit local TCP listeners");
                return;
            }
            Err(error) => panic!("could not bind local test server: {error}"),
        };
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 200\r\nConnection: close\r\n\r\n")
                .unwrap();
            let _ = stream.write_all(&[b'x'; 200]);
        });

        let transport = HttpStorageTransport::new(address.to_string())
            .unwrap()
            .with_limits(50, 50); // total max_response_bytes = 100
        let err = transport
            .send(&StorageRequest::new("GET", "/test", vec![]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("size limit exceeded"), "{err}");
        server.join().unwrap();
    }

    #[test]
    fn storage_emulator_with_http_transport_configures_limits_without_recursion() {
        let transport = HttpStorageTransport::new("127.0.0.1:9199").unwrap();
        let mut emulator = StorageEmulator::with_transport("project-test", transport)
            .unwrap()
            .with_max_bytes_size(4096);
        assert_eq!(emulator.max_bytes_size(), Some(4096));
        assert_eq!(emulator.transport().max_body_bytes(), Some(4096));
        assert_eq!(
            emulator.transport().max_response_bytes(),
            Some(4096 + STORAGE_HEADER_ALLOWANCE_BYTES)
        );

        emulator.set_max_bytes_size(None);
        assert_eq!(emulator.max_bytes_size(), None);
        assert_eq!(emulator.transport().max_body_bytes(), None);
        assert_eq!(emulator.transport().max_response_bytes(), None);
    }
}
