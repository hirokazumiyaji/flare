use std::collections::BTreeMap;

use super::{
    expires_at, validate_email, validate_password, Auth, AuthError, AuthSession, AuthToken,
    AuthUser, DEFAULT_ID_TOKEN_TTL_SECONDS,
};

#[derive(Debug, Clone)]
struct StoredUser {
    user: AuthUser,
    password: String,
}

/// Deterministic, in-process Auth backend used by `run_script` and tests.
///
/// It intentionally implements the same observable contract as the emulator
/// adapter, including a local session, token expiry, and typed errors.  The
/// generated token strings are fake and are never printed by the language.
#[derive(Debug, Default)]
pub struct InMemoryAuth {
    users: BTreeMap<String, StoredUser>,
    session: Option<AuthSession>,
    next_id: u64,
    next_token: u64,
}

impl InMemoryAuth {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed a user for tests without going through password validation.
    /// This is useful for testing `current_user` and session expiry while
    /// keeping the normal script API focused on email/password auth.
    pub fn seed_user(&mut self, user: AuthUser, password: impl Into<String>) {
        let email = user.email.to_ascii_lowercase();
        self.users.insert(
            email,
            StoredUser {
                user,
                password: password.into(),
            },
        );
    }

    pub fn session(&self) -> Option<&AuthSession> {
        self.session.as_ref()
    }

    fn make_session(&mut self, user: &AuthUser) -> Result<AuthSession, AuthError> {
        self.next_token = self.next_token.saturating_add(1);
        let sequence = self.next_token;
        let expires_at = expires_at(DEFAULT_ID_TOKEN_TTL_SECONDS)?;
        Ok(AuthSession {
            user: user.clone(),
            id_token: AuthToken::new(format!("fake-id-token-{}-{sequence}", user.uid)),
            refresh_token: AuthToken::new(format!("fake-refresh-token-{}-{sequence}", user.uid)),
            expires_at,
        })
    }
}

impl Auth for InMemoryAuth {
    fn sign_up(&mut self, email: &str, password: &str) -> Result<AuthSession, AuthError> {
        let email = validate_email(email)?;
        validate_password(password)?;
        if self.users.contains_key(&email) {
            return Err(AuthError::UserAlreadyExists);
        }

        self.next_id = self.next_id.saturating_add(1);
        let user = AuthUser {
            uid: format!("in-memory-user-{}", self.next_id),
            email: email.clone(),
            email_verified: false,
            display_name: None,
            photo_url: None,
            disabled: false,
            created_at: Some(super::now_unix().to_string()),
            last_login_at: None,
            provider_id: Some("password".to_string()),
        };
        self.users.insert(
            email,
            StoredUser {
                user: user.clone(),
                password: password.to_string(),
            },
        );
        let session = self.make_session(&user)?;
        self.session = Some(session.clone());
        Ok(session)
    }

    fn sign_in(&mut self, email: &str, password: &str) -> Result<AuthSession, AuthError> {
        let email = validate_email(email)?;
        validate_password(password)?;
        let Some(stored) = self.users.get(&email) else {
            return Err(AuthError::UserNotFound);
        };
        if stored.password != password {
            return Err(AuthError::InvalidCredentials);
        }
        let user = stored.user.clone();
        let session = self.make_session(&user)?;
        self.session = Some(session.clone());
        Ok(session)
    }

    fn current_user(&self) -> Result<Option<AuthUser>, AuthError> {
        let Some(session) = &self.session else {
            return Ok(None);
        };
        if session.is_expired_at(super::now_unix()) {
            return Ok(None);
        }
        Ok(Some(session.user.clone()))
    }

    fn sign_out(&mut self) -> Result<(), AuthError> {
        // Sign-out is intentionally idempotent, matching the local SDK model.
        self.session = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_up_sign_in_and_sign_out_share_a_session() {
        let mut auth = InMemoryAuth::default();
        let session = auth.sign_up("Alice@Example.com", "password").unwrap();
        assert_eq!(session.user.email, "alice@example.com");
        assert_eq!(auth.current_user().unwrap().unwrap().uid, session.user.uid);
        auth.sign_out().unwrap();
        assert!(auth.current_user().unwrap().is_none());
        auth.sign_in("alice@example.com", "password").unwrap();
        assert!(auth.current_user().unwrap().is_some());
    }

    #[test]
    fn typed_errors_are_distinct() {
        let mut auth = InMemoryAuth::default();
        assert_eq!(
            auth.sign_in("nobody@example.com", "password").unwrap_err(),
            AuthError::UserNotFound
        );
        auth.sign_up("person@example.com", "password").unwrap();
        assert_eq!(
            auth.sign_in("person@example.com", "wrongpass").unwrap_err(),
            AuthError::InvalidCredentials
        );
        assert_eq!(
            auth.sign_up("person@example.com", "password").unwrap_err(),
            AuthError::UserAlreadyExists
        );
    }

    #[test]
    fn weak_password_is_rejected_without_creating_a_user() {
        let mut auth = InMemoryAuth::default();
        assert_eq!(
            auth.sign_up("person@example.com", "short").unwrap_err(),
            AuthError::WeakPassword
        );
        assert!(auth.current_user().unwrap().is_none());
    }
}
