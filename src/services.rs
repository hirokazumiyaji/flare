//! Runtime service composition and project configuration.
//!
//! `Services` is the single extension point for capabilities.  It owns
//! provider objects rather than exposing them as `Runtime` type parameters,
//! so adding a service does not change the type of the evaluator or every
//! builtin function.  `RuntimeContext` pairs that service set with immutable
//! project configuration for one script execution context.

use std::error::Error;
use std::time::Duration;

use crate::auth::Auth;
use crate::error::{CapabilityError, FirelangError};
use crate::firestore::{Firestore, InMemoryFirestore};
use crate::storage::Storage;

/// Default project used by local in-memory execution.
pub const DEFAULT_PROJECT_ID: &str = "firelang-test";
/// Default upper bound for a synchronous provider call.
///
/// Providers may choose a shorter deadline for a particular request.  The
/// runtime itself never stores credentials and never runs background work.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Non-secret configuration shared by all services in one runtime context.
///
/// This type intentionally has no credential, private key, access token, or
/// refresh-token fields.  Provider adapters own those values in their own
/// client objects; the script language only sees the project identity and
/// public emulator endpoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectConfig {
    pub project_id: String,
    pub auth_emulator_host: Option<String>,
    pub storage_emulator_host: Option<String>,
    pub request_timeout: Duration,
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            project_id: DEFAULT_PROJECT_ID.to_string(),
            auth_emulator_host: None,
            storage_emulator_host: None,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }
}

impl ProjectConfig {
    /// Create a config for an explicit project.  Use [`Self::try_new`] when
    /// validation of an empty project id is required.
    pub fn new(project_id: impl Into<String>) -> Self {
        Self {
            project_id: project_id.into(),
            ..Self::default()
        }
    }

    pub fn try_new(project_id: impl Into<String>) -> Result<Self, Box<dyn Error>> {
        let config = Self::new(project_id);
        if config.project_id.trim().is_empty() {
            return Err("project id must not be empty".into());
        }
        Ok(config)
    }

    /// Read only non-secret emulator configuration from the environment.
    ///
    /// Missing emulator variables are represented as `None`; this does not
    /// attempt to discover credentials or contact a provider.
    pub fn from_env() -> Option<Self> {
        let project_id = std::env::var("FIREBASE_PROJECT_ID")
            .or_else(|_| std::env::var("GCLOUD_PROJECT"))
            .or_else(|_| std::env::var("FIRESTORE_PROJECT_ID"))
            .unwrap_or_else(|_| DEFAULT_PROJECT_ID.to_string());
        Some(Self {
            project_id,
            auth_emulator_host: std::env::var("FIREBASE_AUTH_EMULATOR_HOST").ok(),
            storage_emulator_host: std::env::var("FIREBASE_STORAGE_EMULATOR_HOST").ok(),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        })
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    pub fn with_auth_emulator_host(mut self, host: impl Into<String>) -> Self {
        self.auth_emulator_host = Some(host.into());
        self
    }

    pub fn with_storage_emulator_host(mut self, host: impl Into<String>) -> Self {
        self.storage_emulator_host = Some(host.into());
        self
    }

    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    pub fn request_timeout(&self) -> Duration {
        self.request_timeout
    }
}

/// The set of optional capabilities available to one script context.
///
/// `None` is meaningful: a script that calls a missing capability receives a
/// typed capability error at the call site.  `Services::in_memory()` is the
/// convenient all-local configuration; `Services::empty()` is useful for
/// testing capability errors and for hosts that only need Firestore.
pub struct Services {
    pub firestore: Option<Box<dyn Firestore>>,
    pub auth: Option<Box<dyn Auth>>,
    pub storage: Option<Box<dyn Storage>>,
}

impl Default for Services {
    fn default() -> Self {
        Self::in_memory()
    }
}

impl Services {
    /// No capabilities are configured.  This is the strict configuration for
    /// capability-error tests and for a host that only evaluates pure scripts.
    pub fn empty() -> Self {
        Self {
            firestore: None,
            auth: None,
            storage: None,
        }
    }

    /// Configure all three services with deterministic in-memory fakes.
    pub fn in_memory() -> Self {
        Self {
            firestore: Some(Box::new(InMemoryFirestore::default())),
            auth: Some(Box::new(crate::auth::InMemoryAuth::default())),
            storage: Some(Box::new(crate::storage::InMemoryStorage::default())),
        }
    }

    /// Start an empty builder.  Unlike `Default`, the builder never silently
    /// installs a capability.
    pub fn builder() -> ServicesBuilder {
        ServicesBuilder::new()
    }

    /// Convenience adapter for the original Firestore-only construction.
    pub fn from_firestore<F>(firestore: F) -> Self
    where
        F: Firestore + 'static,
    {
        Self {
            firestore: Some(Box::new(firestore)),
            auth: None,
            storage: None,
        }
    }

    pub fn with_firestore<F>(mut self, firestore: F) -> Self
    where
        F: Firestore + 'static,
    {
        self.firestore = Some(Box::new(firestore));
        self
    }

    pub fn with_auth<A>(mut self, auth: A) -> Self
    where
        A: Auth + 'static,
    {
        self.auth = Some(Box::new(auth));
        self
    }

    pub fn with_storage<S>(mut self, storage: S) -> Self
    where
        S: Storage + 'static,
    {
        self.storage = Some(Box::new(storage));
        self
    }

    pub fn has_firestore(&self) -> bool {
        self.firestore.is_some()
    }

    pub fn has_auth(&self) -> bool {
        self.auth.is_some()
    }

    pub fn has_storage(&self) -> bool {
        self.storage.is_some()
    }

    pub fn firestore_ref(&self) -> Option<&dyn Firestore> {
        self.firestore.as_deref()
    }

    pub fn firestore_mut(&mut self) -> Option<&mut (dyn Firestore + 'static)> {
        self.firestore.as_deref_mut()
    }

    pub fn auth_ref(&self) -> Option<&dyn Auth> {
        self.auth.as_deref()
    }

    pub fn auth_mut(&mut self) -> Option<&mut (dyn Auth + 'static)> {
        self.auth.as_deref_mut()
    }

    pub fn storage_ref(&self) -> Option<&dyn Storage> {
        self.storage.as_deref()
    }

    pub fn storage_mut(&mut self) -> Option<&mut (dyn Storage + 'static)> {
        self.storage.as_deref_mut()
    }

    pub fn into_runtime_context(self) -> RuntimeContext {
        RuntimeContext::new(self)
    }
}

/// Fluent builder for [`Services`].
#[derive(Default)]
pub struct ServicesBuilder {
    pub firestore: Option<Box<dyn Firestore>>,
    pub auth: Option<Box<dyn Auth>>,
    pub storage: Option<Box<dyn Storage>>,
}

impl ServicesBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn firestore<F>(mut self, firestore: F) -> Self
    where
        F: Firestore + 'static,
    {
        self.firestore = Some(Box::new(firestore));
        self
    }

    pub fn auth<A>(mut self, auth: A) -> Self
    where
        A: Auth + 'static,
    {
        self.auth = Some(Box::new(auth));
        self
    }

    pub fn storage<S>(mut self, storage: S) -> Self
    where
        S: Storage + 'static,
    {
        self.storage = Some(Box::new(storage));
        self
    }

    pub fn build(self) -> Services {
        Services {
            firestore: self.firestore,
            auth: self.auth,
            storage: self.storage,
        }
    }
}

impl From<ServicesBuilder> for Services {
    fn from(builder: ServicesBuilder) -> Self {
        builder.build()
    }
}

/// Immutable project configuration plus the selected service objects.
///
/// A `RuntimeContext` is consumed by `Runtime` and owns all mutable service
/// state.  It is not global: a host can create independent contexts for
/// independent projects, credentials, or tests.
pub struct RuntimeContext {
    pub config: ProjectConfig,
    pub services: Services,
}

impl Default for RuntimeContext {
    fn default() -> Self {
        Self::new(Services::default())
    }
}

impl RuntimeContext {
    pub fn new(services: impl Into<Services>) -> Self {
        Self {
            config: ProjectConfig::default(),
            services: services.into(),
        }
    }

    pub fn from_services(services: impl Into<Services>) -> Self {
        Self::new(services)
    }

    pub fn with_config(config: ProjectConfig, services: impl Into<Services>) -> Self {
        Self {
            config,
            services: services.into(),
        }
    }

    pub fn from_config(config: ProjectConfig) -> Self {
        Self::with_config(config, Services::default())
    }

    pub fn in_memory() -> Self {
        Self::new(Services::in_memory())
    }

    pub fn project_config(&self) -> &ProjectConfig {
        &self.config
    }

    pub fn project_id(&self) -> &str {
        self.config.project_id()
    }

    pub fn services(&self) -> &Services {
        &self.services
    }

    pub fn services_mut(&mut self) -> &mut Services {
        &mut self.services
    }
}

impl From<Services> for RuntimeContext {
    fn from(services: Services) -> Self {
        Self::new(services)
    }
}

impl From<RuntimeContext> for Services {
    fn from(context: RuntimeContext) -> Self {
        context.services
    }
}

/// Create the canonical typed error for an unavailable capability.
pub fn capability_error(service: &str, operation: &str) -> Box<dyn Error> {
    FirelangError::from(CapabilityError::new(service, operation)).into()
}
