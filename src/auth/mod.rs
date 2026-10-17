//! Firebase Authentication backend contract and value types.
//!
//! The language runtime deliberately talks to this small trait instead of a
//! particular Firebase SDK.  The in-memory implementation is used by the
//! default runtime, while `AuthEmulator` talks to the local Firebase Auth
//! Emulator over its Identity Toolkit HTTP API.

#[cfg(feature = "emulator")]
mod emulator;
mod memory;
mod unavailable;

#[cfg(feature = "emulator")]
pub use emulator::{AuthEmulator, AuthEmulatorOptions, DEFAULT_AUTH_OPERATION_TIMEOUT};
pub use memory::InMemoryAuth;
pub use unavailable::UnavailableAuth;

use std::error::Error;
use std::fmt;

use crate::value::Value;

pub const DEFAULT_PROJECT_ID: &str = "flare-test";
pub const DEFAULT_ID_TOKEN_TTL_SECONDS: i64 = 60 * 60;

/// Broad, stable categories for authentication failures.
///
/// The human-readable message is deliberately separate from the category so
/// callers can make decisions without parsing Firebase's HTTP error strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthErrorKind {
    Configuration,
    UserNotFound,
    UserAlreadyExists,
    InvalidCredentials,
    InvalidEmail,
    WeakPassword,
    Network,
    InvalidResponse,
    NotSignedIn,
    SessionExpired,
}

/// Errors returned by an authentication backend.
///
/// In particular, the emulator's Firebase error codes are translated into
/// these categories before they cross the backend boundary.  This makes the
/// in-memory fake and the HTTP adapter expose the same contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    Configuration(String),
    UserNotFound,
    UserAlreadyExists,
    InvalidCredentials,
    InvalidEmail,
    WeakPassword,
    Network(String),
    InvalidResponse(String),
    NotSignedIn,
    SessionExpired,
}

impl AuthError {
    pub fn kind(&self) -> AuthErrorKind {
        match self {
            Self::Configuration(_) => AuthErrorKind::Configuration,
            Self::UserNotFound => AuthErrorKind::UserNotFound,
            Self::UserAlreadyExists => AuthErrorKind::UserAlreadyExists,
            Self::InvalidCredentials => AuthErrorKind::InvalidCredentials,
            Self::InvalidEmail => AuthErrorKind::InvalidEmail,
            Self::WeakPassword => AuthErrorKind::WeakPassword,
            Self::Network(_) => AuthErrorKind::Network,
            Self::InvalidResponse(_) => AuthErrorKind::InvalidResponse,
            Self::NotSignedIn => AuthErrorKind::NotSignedIn,
            Self::SessionExpired => AuthErrorKind::SessionExpired,
        }
    }

    pub fn is(&self, kind: AuthErrorKind) -> bool {
        self.kind() == kind
    }
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(message) => write!(f, "auth configuration error: {message}"),
            Self::UserNotFound => write!(f, "auth user not found"),
            Self::UserAlreadyExists => write!(f, "auth user already exists"),
            Self::InvalidCredentials => write!(f, "invalid auth credentials"),
            Self::InvalidEmail => write!(f, "invalid auth email"),
            Self::WeakPassword => write!(f, "auth password must be at least 6 characters"),
            Self::Network(message) => write!(f, "auth network error: {message}"),
            Self::InvalidResponse(message) => write!(f, "invalid auth response: {message}"),
            Self::NotSignedIn => write!(f, "auth user is not signed in"),
            Self::SessionExpired => write!(f, "auth session has expired"),
        }
    }
}

impl Error for AuthError {}

/// The user record returned by Firebase Auth.
///
/// Password hashes and provider secrets are never part of this type.  The
/// optional fields mirror the useful fields in an Auth emulator user record;
/// callers should use the normalized map returned by [`AuthUser::to_value`]
/// when interacting with scripts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthUser {
    pub uid: String,
    pub email: String,
    pub email_verified: bool,
    pub display_name: Option<String>,
    pub photo_url: Option<String>,
    pub disabled: bool,
    pub created_at: Option<String>,
    pub last_login_at: Option<String>,
    pub provider_id: Option<String>,
}

impl AuthUser {
    pub fn to_value(&self) -> Value {
        let mut map = std::collections::BTreeMap::new();
        map.insert("uid".to_string(), Value::Str(self.uid.clone()));
        map.insert("email".to_string(), Value::Str(self.email.clone()));
        map.insert(
            "emailVerified".to_string(),
            Value::Bool(self.email_verified),
        );
        map.insert("disabled".to_string(), Value::Bool(self.disabled));
        if let Some(display_name) = &self.display_name {
            map.insert("displayName".to_string(), Value::Str(display_name.clone()));
        }
        if let Some(photo_url) = &self.photo_url {
            map.insert("photoUrl".to_string(), Value::Str(photo_url.clone()));
        }
        if let Some(created_at) = &self.created_at {
            map.insert("createdAt".to_string(), Value::Str(created_at.clone()));
        }
        if let Some(last_login_at) = &self.last_login_at {
            map.insert("lastLoginAt".to_string(), Value::Str(last_login_at.clone()));
        }
        if let Some(provider_id) = &self.provider_id {
            map.insert("providerId".to_string(), Value::Str(provider_id.clone()));
        }
        Value::Map(map)
    }
}

/// A token held by an Auth session.
///
/// `Debug` and `Display` are redacted intentionally.  Rust callers that
/// explicitly need to forward a token can use [`AuthToken::expose`], but the
/// normal script and Rust display paths never do so.
#[derive(Clone, PartialEq, Eq)]
pub struct AuthToken(String);

impl AuthToken {
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for AuthToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl fmt::Display for AuthToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Result of a successful email/password authentication operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthSession {
    pub user: AuthUser,
    pub id_token: AuthToken,
    pub refresh_token: AuthToken,
    /// Unix seconds at which the ID token is considered expired.
    pub expires_at: i64,
}

impl AuthSession {
    pub fn new(
        user: AuthUser,
        id_token: impl Into<AuthToken>,
        refresh_token: impl Into<AuthToken>,
        expires_at: i64,
    ) -> Self {
        Self {
            user,
            id_token: id_token.into(),
            refresh_token: refresh_token.into(),
            expires_at,
        }
    }

    pub fn is_expired_at(&self, now: i64) -> bool {
        now >= self.expires_at
    }

    pub fn user_value(&self) -> Value {
        self.user.to_value()
    }
}

impl From<String> for AuthToken {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<&str> for AuthToken {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

/// Synchronous authentication backend used by [`crate::runtime::Runtime`].
///
/// A backend owns the current local session.  `sign_out` only clears that
/// local session; MVP does not revoke emulator-issued tokens on a server.
pub trait Auth {
    fn is_available(&self) -> bool {
        true
    }

    fn set_timeout(&mut self, _timeout: Option<std::time::Duration>) {}

    fn supports_backend_timeout(&self) -> bool {
        false
    }

    fn sign_up(&mut self, email: &str, password: &str) -> Result<AuthSession, AuthError>;
    fn sign_in(&mut self, email: &str, password: &str) -> Result<AuthSession, AuthError>;
    fn current_user(&self) -> Result<Option<AuthUser>, AuthError>;
    fn sign_out(&mut self) -> Result<(), AuthError>;
}

/// Forwarding adapter so boxed backends (CLI selection, service builders)
/// satisfy the same bound as concrete backends.
impl<T: Auth + ?Sized> Auth for Box<T> {
    fn is_available(&self) -> bool {
        (**self).is_available()
    }

    fn set_timeout(&mut self, timeout: Option<std::time::Duration>) {
        (**self).set_timeout(timeout)
    }

    fn supports_backend_timeout(&self) -> bool {
        (**self).supports_backend_timeout()
    }

    fn sign_up(&mut self, email: &str, password: &str) -> Result<AuthSession, AuthError> {
        (**self).sign_up(email, password)
    }
    fn sign_in(&mut self, email: &str, password: &str) -> Result<AuthSession, AuthError> {
        (**self).sign_in(email, password)
    }
    fn current_user(&self) -> Result<Option<AuthUser>, AuthError> {
        (**self).current_user()
    }
    fn sign_out(&mut self) -> Result<(), AuthError> {
        (**self).sign_out()
    }
}

impl<T: Auth + ?Sized> Auth for &mut T {
    fn is_available(&self) -> bool {
        (**self).is_available()
    }
    fn set_timeout(&mut self, timeout: Option<std::time::Duration>) {
        (**self).set_timeout(timeout)
    }
    fn supports_backend_timeout(&self) -> bool {
        (**self).supports_backend_timeout()
    }
    fn sign_up(&mut self, email: &str, password: &str) -> Result<AuthSession, AuthError> {
        (**self).sign_up(email, password)
    }
    fn sign_in(&mut self, email: &str, password: &str) -> Result<AuthSession, AuthError> {
        (**self).sign_in(email, password)
    }
    fn current_user(&self) -> Result<Option<AuthUser>, AuthError> {
        (**self).current_user()
    }
    fn sign_out(&mut self) -> Result<(), AuthError> {
        (**self).sign_out()
    }
}

pub(crate) fn validate_email(email: &str) -> Result<String, AuthError> {
    let email = email.trim();
    if email.is_empty() || email.chars().any(char::is_whitespace) {
        return Err(AuthError::InvalidEmail);
    }
    let mut parts = email.split('@');
    let local = parts.next().unwrap_or_default();
    let domain = parts.next().unwrap_or_default();
    if local.is_empty() || domain.is_empty() || parts.next().is_some() || !domain.contains('.') {
        return Err(AuthError::InvalidEmail);
    }
    Ok(email.to_ascii_lowercase())
}

pub(crate) fn validate_password(password: &str) -> Result<(), AuthError> {
    if password.chars().count() < 6 {
        return Err(AuthError::WeakPassword);
    }
    Ok(())
}

pub(crate) fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

pub(crate) fn expires_at(ttl_seconds: i64) -> Result<i64, AuthError> {
    now_unix()
        .checked_add(ttl_seconds)
        .ok_or_else(|| AuthError::InvalidResponse("token expiration overflows Unix time".into()))
}
