//! Provider-neutral authentication capability used by the runtime.
//!
//! The runtime is deliberately synchronous.  A provider adapter can bridge an
//! asynchronous SDK here, but it must do so before returning from the method
//! (and must not block inside the script language itself).  No credential or
//! token is stored in this module: [`InMemoryAuth`] is a test double and
//! [`AuthEmulator`] is a local emulator adapter.

use std::collections::BTreeMap;
use std::error::Error;

/// The identity visible to a script after an authentication operation.
///
/// This is intentionally a small, provider-neutral representation.  Provider
/// adapters can map their native claims to `custom_claims`, but scripts never
/// receive a credential or a refresh token through the runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthUser {
    pub uid: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub email_verified: bool,
    pub custom_claims: BTreeMap<String, String>,
}

impl AuthUser {
    pub fn new(uid: impl Into<String>) -> Self {
        Self {
            uid: uid.into(),
            email: None,
            display_name: None,
            email_verified: false,
            custom_claims: BTreeMap::new(),
        }
    }

    pub fn with_email(mut self, email: impl Into<String>) -> Self {
        self.email = Some(email.into());
        self
    }

    pub fn with_display_name(mut self, display_name: impl Into<String>) -> Self {
        self.display_name = Some(display_name.into());
        self
    }

    pub fn verified(mut self, verified: bool) -> Self {
        self.email_verified = verified;
        self
    }

    pub fn with_claim(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.custom_claims.insert(key.into(), value.into());
        self
    }
}

/// Synchronous authentication capability.
///
/// `Auth` is object safe so a `Services` value can contain independently
/// selected providers without adding a type parameter to `Runtime`.
pub trait Auth {
    fn sign_in(&mut self, email: &str, password: &str) -> Result<AuthUser, Box<dyn Error>>;
    fn sign_out(&mut self) -> Result<(), Box<dyn Error>>;
    fn current_user(&self) -> Result<Option<AuthUser>, Box<dyn Error>>;

    /// Anonymous sign-in is optional for providers.  Providers that do not
    /// support it return a stable unsupported-operation error by default.
    fn sign_in_anonymously(&mut self) -> Result<AuthUser, Box<dyn Error>> {
        Err("authentication provider does not support anonymous sign-in".into())
    }

    /// Token verification is optional.  It is deliberately a capability
    /// rather than a required method so small provider fakes remain easy to
    /// implement.
    fn verify_token(&self, _token: &str) -> Result<Option<AuthUser>, Box<dyn Error>> {
        Err("authentication provider does not support token verification".into())
    }
}

#[derive(Debug, Clone)]
struct StoredUser {
    user: AuthUser,
    password: String,
}

/// Deterministic in-memory authentication fake.
///
/// It is suitable for local development and contract tests.  Passwords never
/// leave this value and are not exposed through [`AuthUser`].
#[derive(Default)]
pub struct InMemoryAuth {
    users: BTreeMap<String, StoredUser>,
    current_uid: Option<String>,
    next_uid: u64,
}

impl InMemoryAuth {
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct a fake with one known user.
    pub fn with_user(user: AuthUser, password: impl Into<String>) -> Self {
        let mut auth = Self::default();
        auth.add_user(user, password)
            .expect("a valid in-memory user can always be inserted");
        auth
    }

    pub fn add_user(
        &mut self,
        user: AuthUser,
        password: impl Into<String>,
    ) -> Result<(), Box<dyn Error>> {
        if user.uid.is_empty() {
            return Err("auth user uid must not be empty".into());
        }
        if self.users.contains_key(&user.uid) {
            return Err(format!("auth user already exists: {}", user.uid).into());
        }
        self.users.insert(
            user.uid.clone(),
            StoredUser {
                user,
                password: password.into(),
            },
        );
        Ok(())
    }

    pub fn user_count(&self) -> usize {
        self.users.len()
    }

    pub fn is_signed_in(&self) -> bool {
        self.current_uid.is_some()
    }

    fn create_uid(&mut self) -> String {
        self.next_uid = self.next_uid.saturating_add(1);
        format!("in-memory-user-{}", self.next_uid)
    }
}

impl Auth for InMemoryAuth {
    fn sign_in(&mut self, email: &str, password: &str) -> Result<AuthUser, Box<dyn Error>> {
        let user = self
            .users
            .values()
            .find(|stored| stored.user.email.as_deref() == Some(email))
            .ok_or_else(|| format!("auth sign-in failed for {email}"))?;
        if user.password != password {
            return Err(format!("auth sign-in failed for {email}").into());
        }
        let user = user.user.clone();
        self.current_uid = Some(user.uid.clone());
        Ok(user)
    }

    fn sign_out(&mut self) -> Result<(), Box<dyn Error>> {
        self.current_uid = None;
        Ok(())
    }

    fn current_user(&self) -> Result<Option<AuthUser>, Box<dyn Error>> {
        Ok(self
            .current_uid
            .as_ref()
            .and_then(|uid| self.users.get(uid))
            .map(|stored| stored.user.clone()))
    }

    fn sign_in_anonymously(&mut self) -> Result<AuthUser, Box<dyn Error>> {
        let user = AuthUser::new(self.create_uid());
        self.add_user(user.clone(), "")?;
        self.current_uid = Some(user.uid.clone());
        Ok(user)
    }
}

/// In-process Auth emulator-mode adapter.
///
/// This adapter keeps state in memory and performs **no** HTTP request to a
/// Firebase Auth emulator.  It exists to give hosts an emulator-named local
/// implementation of the same [`Auth`] contract; a remote adapter can replace
/// it without changing `Runtime` or the language API.
pub struct AuthEmulator {
    project_id: String,
    inner: InMemoryAuth,
}

impl AuthEmulator {
    pub fn new(project_id: impl Into<String>) -> Self {
        Self {
            project_id: project_id.into(),
            inner: InMemoryAuth::default(),
        }
    }

    pub fn from_config(config: &crate::services::ProjectConfig) -> Self {
        Self::new(config.project_id.clone())
    }

    /// Read only non-secret emulator configuration.  No credential is read or
    /// retained by this constructor, and no connection is opened to the
    /// detected host.
    pub fn from_env() -> Option<Self> {
        let project_id = std::env::var("FIREBASE_PROJECT_ID")
            .or_else(|_| std::env::var("FIRESTORE_PROJECT_ID"))
            .unwrap_or_else(|_| "firelang-test".to_string());
        std::env::var("FIREBASE_AUTH_EMULATOR_HOST")
            .ok()
            .map(|_| Self::new(project_id))
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    pub fn with_user(mut self, user: AuthUser, password: impl Into<String>) -> Self {
        self.inner
            .add_user(user, password)
            .expect("a valid in-memory user can always be inserted");
        self
    }

    pub fn user_count(&self) -> usize {
        self.inner.user_count()
    }
}

impl Auth for AuthEmulator {
    fn sign_in(&mut self, email: &str, password: &str) -> Result<AuthUser, Box<dyn Error>> {
        self.inner.sign_in(email, password)
    }

    fn sign_out(&mut self) -> Result<(), Box<dyn Error>> {
        self.inner.sign_out()
    }

    fn current_user(&self) -> Result<Option<AuthUser>, Box<dyn Error>> {
        self.inner.current_user()
    }

    fn sign_in_anonymously(&mut self) -> Result<AuthUser, Box<dyn Error>> {
        self.inner.sign_in_anonymously()
    }

    fn verify_token(&self, token: &str) -> Result<Option<AuthUser>, Box<dyn Error>> {
        self.inner.verify_token(token)
    }
}

/// Alias for hosts that name their in-process emulator-mode adapter with the
/// Firebase provider.
pub type FirebaseAuthEmulator = AuthEmulator;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_auth_tracks_the_local_session() {
        let user = AuthUser::new("u1")
            .with_email("alice@example.com")
            .verified(true);
        let mut auth = InMemoryAuth::with_user(user, "secret");

        assert_eq!(auth.current_user().unwrap(), None);
        assert!(format!(
            "{}",
            auth.sign_in("alice@example.com", "wrong").unwrap_err()
        )
        .contains("auth sign-in failed"));

        let signed_in = auth.sign_in("alice@example.com", "secret").unwrap();
        assert_eq!(signed_in.uid, "u1");
        assert!(auth.is_signed_in());
        assert_eq!(auth.current_user().unwrap(), Some(signed_in));

        auth.sign_out().unwrap();
        assert!(!auth.is_signed_in());
        assert_eq!(auth.current_user().unwrap(), None);
    }

    #[test]
    fn anonymous_sign_in_creates_a_unique_local_identity() {
        let mut auth = InMemoryAuth::new();
        let first = auth.sign_in_anonymously().unwrap();
        let second = auth.sign_in_anonymously().unwrap();
        assert_ne!(first.uid, second.uid);
        assert_eq!(auth.user_count(), 2);
        assert_eq!(
            auth.current_user().unwrap().map(|user| user.uid),
            Some(second.uid)
        );
    }
}
