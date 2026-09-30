use std::env;
use std::time::Duration;

use serde_json::{json, Value as JsonValue};
use tokio::runtime::{Builder as RuntimeBuilder, Runtime as TokioRuntime};

use super::{
    expires_at, validate_email, validate_password, Auth, AuthError, AuthSession, AuthToken,
    AuthUser, DEFAULT_PROJECT_ID,
};

const DEFAULT_API_KEY: &str = "fake-api-key";

/// Firebase Authentication Emulator adapter.
///
/// The Auth Emulator exposes the Identity Toolkit REST API over plain HTTP.
/// This adapter intentionally uses that small public surface instead of a
/// production Firebase SDK, so it cannot accidentally acquire production
/// credentials or silently switch to the hosted service.
pub struct AuthEmulator {
    base_url: String,
    project_id: String,
    api_key: String,
    client: reqwest::Client,
    runtime: TokioRuntime,
    session: Option<AuthSession>,
}

impl AuthEmulator {
    /// Build a client from the emulator environment.
    ///
    /// An absent `FIREBASE_AUTH_EMULATOR_HOST` means "not configured" and is
    /// represented by `Ok(None)`.  Once the variable is present, malformed
    /// configuration is an error rather than being silently converted to a
    /// skip condition.
    pub fn from_env() -> Result<Option<Self>, AuthError> {
        let host = match env::var_os("FIREBASE_AUTH_EMULATOR_HOST") {
            None => None,
            Some(value) => Some(value.into_string().map_err(|_| {
                AuthError::Configuration("FIREBASE_AUTH_EMULATOR_HOST is not valid UTF-8".into())
            })?),
        };
        let project_id = project_id_from_env()?;

        let Some(host) = host else {
            return Ok(None);
        };
        let project_id = project_id.unwrap_or_else(|| DEFAULT_PROJECT_ID.to_string());
        Self::new(host, project_id).map(Some)
    }

    /// Construct an Auth Emulator client for an explicit host and project.
    pub fn new(host: impl Into<String>, project_id: impl Into<String>) -> Result<Self, AuthError> {
        let base_url = normalize_base_url(&host.into())?;
        let project_id = validate_project_id(&project_id.into())?;
        let api_key = env::var("FIREBASE_API_KEY").unwrap_or_else(|_| DEFAULT_API_KEY.to_string());
        if api_key.trim().is_empty() {
            return Err(AuthError::Configuration(
                "FIREBASE_API_KEY must not be empty when set".into(),
            ));
        }

        let runtime = RuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| {
                AuthError::Configuration(format!("could not create Auth client runtime: {error}"))
            })?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|error| {
                AuthError::Configuration(format!("could not create Auth HTTP client: {error}"))
            })?;

        Ok(Self {
            base_url,
            project_id,
            api_key,
            client,
            runtime,
            session: None,
        })
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn session(&self) -> Option<&AuthSession> {
        self.session.as_ref()
    }

    fn post(&self, operation: &str, body: JsonValue) -> Result<JsonValue, AuthError> {
        // The public Identity Toolkit endpoints authenticate with the API key
        // query parameter.  No OAuth2 `Authorization` header is sent on purpose:
        // the emulator treats such a request as an Admin SDK call, and the
        // sign-up operation then returns a user record *without* issuing ID or
        // refresh tokens.
        //
        // The project header is still sent so multi-project setups have a
        // useful diagnostic hook for which project this client was built for.
        let url = format!(
            "{}/identitytoolkit.googleapis.com/v1/{operation}?key={}",
            self.base_url, self.api_key
        );
        let (status, text) = self.runtime.block_on(async {
            let response = self
                .client
                .post(url)
                .header("X-Firebase-Project-ID", &self.project_id)
                .header("X-Goog-Api-Key", &self.api_key)
                .json(&body)
                .send()
                .await
                .map_err(network_error)?;
            let status = response.status();
            let text = response.text().await.map_err(network_error)?;
            Ok::<_, AuthError>((status, text))
        })?;
        if !status.is_success() {
            return Err(map_http_error(status.as_u16(), &text));
        }
        serde_json::from_str(&text).map_err(|error| {
            AuthError::InvalidResponse(format!("could not decode JSON response: {error}"))
        })
    }

    fn authenticate(
        &mut self,
        operation: &str,
        email: &str,
        password: &str,
    ) -> Result<AuthSession, AuthError> {
        let email = validate_email(email)?;
        validate_password(password)?;
        let response = self.post(
            operation,
            json!({
                "email": email,
                "password": password,
                "returnSecureToken": true,
            }),
        )?;
        let session = session_from_response(response, &email)?;
        self.session = Some(session.clone());
        Ok(session)
    }
}

impl Auth for AuthEmulator {
    fn sign_up(&mut self, email: &str, password: &str) -> Result<AuthSession, AuthError> {
        self.authenticate("accounts:signUp", email, password)
    }

    fn sign_in(&mut self, email: &str, password: &str) -> Result<AuthSession, AuthError> {
        self.authenticate("accounts:signInWithPassword", email, password)
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
        self.session = None;
        Ok(())
    }
}

fn project_id_from_env() -> Result<Option<String>, AuthError> {
    for key in [
        "FIREBASE_AUTH_PROJECT_ID",
        "FIREBASE_PROJECT_ID",
        "FIRESTORE_PROJECT_ID",
    ] {
        let Some(value) = env::var_os(key) else {
            continue;
        };
        let value = value
            .into_string()
            .map_err(|_| AuthError::Configuration(format!("{key} is not valid UTF-8")))?;
        return validate_project_id(&value).map(Some);
    }
    Ok(None)
}

fn validate_project_id(project_id: &str) -> Result<String, AuthError> {
    let project_id = project_id.trim();
    if project_id.is_empty() {
        return Err(AuthError::Configuration(
            "Firebase project ID must not be empty".into(),
        ));
    }
    if project_id.chars().any(|ch| ch.is_whitespace() || ch == '/') {
        return Err(AuthError::Configuration(
            "Firebase project ID contains an invalid character".into(),
        ));
    }
    Ok(project_id.to_string())
}

fn normalize_base_url(host: &str) -> Result<String, AuthError> {
    let host = host.trim();
    if host.is_empty() {
        return Err(AuthError::Configuration(
            "FIREBASE_AUTH_EMULATOR_HOST must not be empty".into(),
        ));
    }
    if host.chars().any(char::is_whitespace) {
        return Err(AuthError::Configuration(
            "FIREBASE_AUTH_EMULATOR_HOST contains whitespace".into(),
        ));
    }
    let candidate = if host.contains("://") {
        host.to_string()
    } else {
        format!("http://{host}")
    };
    let parsed = reqwest::Url::parse(&candidate).map_err(|_| {
        AuthError::Configuration("FIREBASE_AUTH_EMULATOR_HOST is not a valid URL".into())
    })?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(AuthError::Configuration(
            "FIREBASE_AUTH_EMULATOR_HOST must use http or https".into(),
        ));
    }
    if parsed.host_str().is_none() {
        return Err(AuthError::Configuration(
            "FIREBASE_AUTH_EMULATOR_HOST must include a host".into(),
        ));
    }
    Ok(candidate.trim_end_matches('/').to_string())
}

fn network_error(error: reqwest::Error) -> AuthError {
    // Do not format the request URL into the public error: a user may have
    // configured a custom API key, and URLs are not useful script diagnostics.
    if error.is_timeout() {
        AuthError::Network("request to the Auth emulator timed out".into())
    } else if error.is_connect() {
        AuthError::Network("could not connect to the Auth emulator".into())
    } else {
        AuthError::Network("Auth emulator request failed".into())
    }
}

fn map_http_error(status: u16, body: &str) -> AuthError {
    let parsed = serde_json::from_str::<JsonValue>(body).ok();
    let message = parsed
        .as_ref()
        .and_then(|value| value.get("error"))
        .and_then(|error| error.get("message"))
        .and_then(JsonValue::as_str)
        .unwrap_or("the Auth emulator returned an error");

    // Identity Toolkit codes may carry a human-readable suffix, for example
    // `WEAK_PASSWORD : Password should be at least 6 characters`.  Match on the
    // leading code token so those details do not push a known code into the
    // untyped fallback.
    let code = message
        .split(':')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_uppercase();

    match code.as_str() {
        "EMAIL_NOT_FOUND" | "USER_NOT_FOUND" => AuthError::UserNotFound,
        "INVALID_PASSWORD" | "INVALID_LOGIN_CREDENTIALS" => AuthError::InvalidCredentials,
        "EMAIL_EXISTS" | "DUPLICATE_EMAIL" => AuthError::UserAlreadyExists,
        "USER_DISABLED" => AuthError::InvalidCredentials,
        "INVALID_EMAIL" | "MISSING_EMAIL" => AuthError::InvalidEmail,
        "WEAK_PASSWORD" | "MISSING_PASSWORD" | "PASSWORD_LOGIN_DISABLED" => AuthError::WeakPassword,
        _ => AuthError::InvalidResponse(format!("Auth emulator HTTP {status}: {message}")),
    }
}

fn session_from_response(
    response: JsonValue,
    fallback_email: &str,
) -> Result<AuthSession, AuthError> {
    let user = user_from_response(&response, fallback_email)?;
    let id_token = required_string(&response, "idToken", "ID token")?;
    let refresh_token = required_string(&response, "refreshToken", "refresh token")?;
    let ttl = response
        .get("expiresIn")
        .or_else(|| response.get("expires_in"))
        .and_then(parse_i64)
        .ok_or_else(|| AuthError::InvalidResponse("expiresIn is missing or invalid".into()))?;
    if ttl <= 0 {
        return Err(AuthError::InvalidResponse(
            "expiresIn must be positive".into(),
        ));
    }
    let expires_at = expires_at(ttl)?;
    Ok(AuthSession {
        user,
        id_token: AuthToken::new(id_token),
        refresh_token: AuthToken::new(refresh_token),
        expires_at,
    })
}

fn user_from_response(response: &JsonValue, fallback_email: &str) -> Result<AuthUser, AuthError> {
    let uid = response
        .get("localId")
        .or_else(|| response.get("uid"))
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AuthError::InvalidResponse("localId is missing".into()))?
        .to_string();
    let email = response
        .get("email")
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback_email)
        .to_string();
    Ok(AuthUser {
        uid,
        email,
        email_verified: response
            .get("emailVerified")
            .or_else(|| response.get("email_verified"))
            .and_then(json_bool)
            .unwrap_or(false),
        display_name: optional_string(response, "displayName"),
        photo_url: optional_string(response, "photoUrl"),
        disabled: response
            .get("disabled")
            .and_then(json_bool)
            .unwrap_or(false),
        created_at: optional_string(response, "createdAt"),
        last_login_at: optional_string(response, "lastLoginAt"),
        provider_id: optional_string(response, "providerId"),
    })
}

fn required_string(value: &JsonValue, key: &str, label: &str) -> Result<String, AuthError> {
    value
        .get(key)
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| AuthError::InvalidResponse(format!("{label} is missing")))
}

fn optional_string(value: &JsonValue, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn json_bool(value: &JsonValue) -> Option<bool> {
    value
        .as_bool()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

fn parse_i64(value: &JsonValue) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_host_and_default_project() {
        let auth = AuthEmulator::new("127.0.0.1:9099", DEFAULT_PROJECT_ID).unwrap();
        assert_eq!(auth.base_url(), "http://127.0.0.1:9099");
        assert_eq!(auth.project_id(), DEFAULT_PROJECT_ID);
    }

    #[test]
    fn rejects_empty_configuration() {
        assert!(matches!(
            AuthEmulator::new("", DEFAULT_PROJECT_ID),
            Err(AuthError::Configuration(_))
        ));
        assert!(matches!(
            AuthEmulator::new("127.0.0.1:9099", " "),
            Err(AuthError::Configuration(_))
        ));
    }

    #[test]
    fn maps_identity_toolkit_errors() {
        assert!(matches!(
            map_http_error(400, r#"{"error":{"message":"EMAIL_NOT_FOUND"}}"#),
            AuthError::UserNotFound
        ));
        assert!(matches!(
            map_http_error(400, r#"{"error":{"message":"INVALID_PASSWORD"}}"#),
            AuthError::InvalidCredentials
        ));
        assert!(matches!(
            map_http_error(400, r#"{"error":{"message":"EMAIL_EXISTS"}}"#),
            AuthError::UserAlreadyExists
        ));
        assert!(matches!(
            map_http_error(400, r#"{"error":{"message":"INVALID_EMAIL"}}"#),
            AuthError::InvalidEmail
        ));
        assert!(matches!(
            map_http_error(503, r#"{"error":{"message":"unavailable"}}"#),
            AuthError::InvalidResponse(_)
        ));
    }

    #[test]
    fn maps_codes_that_carry_a_detail_suffix() {
        // The live emulator formats this as
        // `WEAK_PASSWORD : Password should be at least 6 characters`.
        assert!(matches!(
            map_http_error(
                400,
                r#"{"error":{"message":"WEAK_PASSWORD : Password should be at least 6 characters"}}"#
            ),
            AuthError::WeakPassword
        ));
    }

    #[test]
    fn error_details_never_echo_a_secret_or_a_url() {
        // `map_http_error` is the only place that forwards a server message
        // into an AuthError; it must stay free of request URLs (which can
        // carry the API key).
        let err = map_http_error(400, r#"{"error":{"message":"SOMETHING_UNEXPECTED"}}"#);
        let text = err.to_string();
        assert!(!text.contains("://"), "{text}");
        assert!(!text.contains("key="), "{text}");
    }
}
