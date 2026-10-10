use crate::auth::{Auth, InMemoryAuth, UnavailableAuth};
use crate::firestore::{Firestore, InMemoryFirestore, UnavailableFirestore};
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
    pub(crate) project: ProjectConfig,
}

pub(crate) struct ServiceAccess<'a> {
    pub(crate) firestore: &'a mut dyn Firestore,
    pub(crate) storage: &'a mut dyn Storage,
    pub(crate) auth: &'a mut dyn Auth,
    #[allow(dead_code)]
    pub(crate) project: &'a ProjectConfig,
}

impl Services<InMemoryFirestore, InMemoryStorage> {
    pub fn in_memory() -> Self {
        Self {
            firestore: InMemoryFirestore::default(),
            storage: InMemoryStorage::default(),
            auth: Box::new(InMemoryAuth::default()),
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
            project: self.project,
        }
    }

    pub fn with_storage<S2: Storage>(self, storage: S2) -> Services<F, S2> {
        Services {
            firestore: self.firestore,
            storage,
            auth: self.auth,
            project: self.project,
        }
    }

    pub fn with_auth<A: Auth + 'static>(mut self, auth: A) -> Self {
        self.auth = Box::new(auth);
        self
    }

    pub fn with_project(mut self, project: ProjectConfig) -> Self {
        self.project = project;
        self
    }
}
