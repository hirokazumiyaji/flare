use crate::auth::{Auth, InMemoryAuth, UnavailableAuth};
use crate::firestore::{Firestore, InMemoryFirestore, UnavailableFirestore};
use crate::io::{Io, StdIo, UnavailableIO};
use crate::storage::{InMemoryStorage, Storage, UnavailableStorage};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectConfig {
    pub project_id: Option<String>,
    pub storage_bucket: Option<String>,
}

pub struct Services<F: Firestore, S: Storage = InMemoryStorage> {
    pub(crate) firestore: F,
    pub(crate) storage: S,
    pub(crate) auth: Box<dyn Auth>,
    pub(crate) io: Box<dyn Io>,
    pub(crate) project: ProjectConfig,
}

pub(crate) struct ServiceAccess<'a> {
    pub(crate) firestore: &'a mut dyn Firestore,
    pub(crate) storage: &'a mut dyn Storage,
    pub(crate) auth: &'a mut dyn Auth,
    pub(crate) io: &'a mut dyn Io,
    #[allow(dead_code)]
    pub(crate) project: &'a ProjectConfig,
}

impl Services<InMemoryFirestore, InMemoryStorage> {
    pub fn in_memory() -> Self {
        Self {
            firestore: InMemoryFirestore::default(),
            storage: InMemoryStorage::default(),
            auth: Box::new(InMemoryAuth::default()),
            io: Box::new(StdIo),
            project: ProjectConfig::default(),
        }
    }
}

impl Services<UnavailableFirestore, UnavailableStorage> {
    pub fn empty(project: ProjectConfig) -> Self {
        Self {
            firestore: UnavailableFirestore,
            storage: UnavailableStorage,
            auth: Box::new(UnavailableAuth),
            io: Box::new(UnavailableIO),
            project,
        }
    }
}

impl<F: Firestore, S: Storage> Services<F, S> {
    pub fn with_firestore<F2: Firestore>(self, firestore: F2) -> Services<F2, S> {
        Services {
            firestore,
            storage: self.storage,
            auth: self.auth,
            io: self.io,
            project: self.project,
        }
    }

    pub fn with_storage<S2: Storage>(self, storage: S2) -> Services<F, S2> {
        Services {
            firestore: self.firestore,
            storage,
            auth: self.auth,
            io: self.io,
            project: self.project,
        }
    }

    pub fn with_auth<A: Auth + 'static>(mut self, auth: A) -> Self {
        self.auth = Box::new(auth);
        self
    }

    pub fn with_io<I: Io + 'static>(mut self, io: I) -> Self {
        self.io = Box::new(io);
        self
    }

    pub fn with_project(mut self, project: ProjectConfig) -> Self {
        self.project = project;
        self
    }
}

pub const ALLOW_REMOTE_EMULATOR_ENV: &str = "FLARE_ALLOW_REMOTE_EMULATOR";

/// Check whether remote emulator connections are explicitly permitted
/// via `FLARE_ALLOW_REMOTE_EMULATOR=1` (or true/yes/on).
pub fn is_remote_emulator_allowed() -> bool {
    match std::env::var(ALLOW_REMOTE_EMULATOR_ENV) {
        Ok(val) => {
            val == "1"
                || val.eq_ignore_ascii_case("true")
                || val.eq_ignore_ascii_case("yes")
                || val.eq_ignore_ascii_case("on")
        }
        Err(_) => false,
    }
}

/// Parse and extract `(host, port)` from an emulator host string.
/// Accepts forms like:
/// - `127.0.0.1:8080`
/// - `localhost:9099`
/// - `[::1]:9199`
/// - `http://127.0.0.1:8080/`
/// - `127.0.0.1`
/// - `::1`
pub fn extract_host_and_port(raw: &str) -> Result<(String, Option<u16>), String> {
    let s = raw.trim();
    if s.is_empty() {
        return Err("emulator host must not be empty".to_string());
    }
    let without_scheme = if let Some(rest) = s.strip_prefix("http://") {
        rest
    } else if let Some(rest) = s.strip_prefix("https://") {
        rest
    } else if s.contains("://") {
        return Err(format!(
            "emulator host must use http:// or https://, got {raw}"
        ));
    } else {
        s
    };

    if without_scheme.contains('?') || without_scheme.contains('#') {
        return Err(format!(
            "emulator host must not contain query parameters or fragments: {raw}"
        ));
    }

    // Split authority from optional path prefix (e.g. for reverse proxies)
    let authority = if let Some(pos) = without_scheme.find('/') {
        &without_scheme[..pos]
    } else {
        without_scheme
    };

    if authority.contains('@') {
        return Err(format!(
            "emulator host must not contain userinfo or credentials: {raw}"
        ));
    }

    let s = authority.trim();
    if s.is_empty() {
        return Err("emulator host must not be empty".to_string());
    }

    if s.starts_with('[') {
        let closing = s
            .find(']')
            .ok_or_else(|| "missing closing bracket ']' in IPv6 address".to_string())?;
        let ip_str = &s[1..closing];
        let rest = &s[closing + 1..];
        let port = if let Some(rest) = rest.strip_prefix(':') {
            Some(
                rest.parse::<u16>()
                    .map_err(|e| format!("invalid port in emulator host: {e}"))?,
            )
        } else if !rest.is_empty() {
            return Err(format!("unexpected characters after IPv6 bracket: {rest}"));
        } else {
            None
        };
        Ok((ip_str.to_string(), port))
    } else {
        let colon_count = s.chars().filter(|&c| c == ':').count();
        if colon_count == 1 {
            let (host, port_str) = s.split_once(':').unwrap();
            if host.is_empty() {
                return Err("emulator host must not be empty".to_string());
            }
            let port = port_str
                .parse::<u16>()
                .map_err(|e| format!("invalid port in emulator host: {e}"))?;
            Ok((host.to_string(), Some(port)))
        } else if colon_count == 0 || s.parse::<std::net::Ipv6Addr>().is_ok() {
            Ok((s.to_string(), None))
        } else {
            Err(format!(
                "invalid IPv6 emulator host: {s} (use [addr]:port for IPv6 with port)"
            ))
        }
    }
}

/// Validate that an emulator endpoint is restricted to loopback (127.0.0.0/8, ::1,
/// or localhost) to prevent leaking credentials to remote hosts over plaintext HTTP.
///
/// An opt-in environment variable `FLARE_ALLOW_REMOTE_EMULATOR=1` is supported for
/// containerized or CI environments.
pub fn validate_emulator_host(raw: &str) -> Result<(), String> {
    let (host, _port) = extract_host_and_port(raw)?;
    if is_remote_emulator_allowed() {
        return Ok(());
    }

    if host.eq_ignore_ascii_case("localhost") {
        return Ok(());
    }

    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if ip.is_loopback() {
            return Ok(());
        }
    }

    Err(format!(
        "emulator host must be a loopback address (127.0.0.0/8, ::1, or localhost), got {raw}; set {ALLOW_REMOTE_EMULATOR_ENV}=1 to allow remote connections"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_addresses_and_localhost_are_accepted() {
        assert!(validate_emulator_host("127.0.0.1:8080").is_ok());
        assert!(validate_emulator_host("127.0.0.2:9099").is_ok());
        assert!(validate_emulator_host("localhost:8080").is_ok());
        assert!(validate_emulator_host("[::1]:9199").is_ok());
        assert!(validate_emulator_host("::1").is_ok());
        assert!(validate_emulator_host("http://localhost:8080/").is_ok());
        assert!(validate_emulator_host("http://127.0.0.1:9099").is_ok());
        assert!(validate_emulator_host("127.0.0.1").is_ok());
        assert!(validate_emulator_host("http://localhost:8080/firestore").is_ok());
    }

    #[test]
    fn remote_and_wildcard_addresses_are_rejected() {
        let cases = [
            "192.168.1.1:8080",
            "10.0.0.1:9099",
            "8.8.8.8:8080",
            "0.0.0.0:8080",
            "http://192.168.1.5:9099/",
            "[2001:db8::1]:8080",
            "example.com:8080",
        ];
        for case in cases {
            let err = validate_emulator_host(case).unwrap_err();
            assert!(
                err.contains("emulator host must be a loopback address"),
                "expected loopback error for {case}, got {err}"
            );
            assert!(
                err.contains("FLARE_ALLOW_REMOTE_EMULATOR=1"),
                "expected mention of opt-in for {case}, got {err}"
            );
        }
    }

    #[test]
    fn malformed_url_components_are_rejected() {
        assert!(validate_emulator_host("localhost:8080?x=remote").is_err());
        assert!(validate_emulator_host("http://localhost?x=remote").is_err());
        assert!(validate_emulator_host("http://localhost:8080#tag").is_err());
        assert!(validate_emulator_host("user:pass@localhost:8080").is_err());
        assert!(validate_emulator_host("http://user:pass@localhost:8080/firestore").is_err());
    }

    #[test]
    fn empty_host_is_rejected() {
        assert!(validate_emulator_host("").is_err());
        assert!(validate_emulator_host("   ").is_err());
        assert!(validate_emulator_host("http://").is_err());
    }
}
