use super::{Auth, AuthError, AuthSession, AuthUser};

#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableAuth;

impl Auth for UnavailableAuth {
    fn is_available(&self) -> bool {
        false
    }

    fn sign_up(&mut self, _email: &str, _password: &str) -> Result<AuthSession, AuthError> {
        Err(AuthError::Configuration(
            "Auth service is unavailable".into(),
        ))
    }

    fn sign_in(&mut self, _email: &str, _password: &str) -> Result<AuthSession, AuthError> {
        Err(AuthError::Configuration(
            "Auth service is unavailable".into(),
        ))
    }

    fn current_user(&self) -> Result<Option<AuthUser>, AuthError> {
        Err(AuthError::Configuration(
            "Auth service is unavailable".into(),
        ))
    }

    fn sign_out(&mut self) -> Result<(), AuthError> {
        Err(AuthError::Configuration(
            "Auth service is unavailable".into(),
        ))
    }
}
