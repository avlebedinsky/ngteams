//! The app's selected credential store: the desktop Secret Service, or a lab account's broker.
//!
//! A lab process never opens the desktop keyring. Its socket is shared by account startup, token refresh,
//! sign-in and forget so each operation reaches the broker's one account in the lab namespace.

use std::io;
use std::sync::Arc;
use teams_auth::broker::BrokerStream;

use teams_auth::broker::{BrokerScope, BrokerSecretStore};
use teams_auth::secrets::{LazyKeyring, MemorySecretStore, Secret, SecretError, SecretKey, SecretStore};

use crate::KEYRING_APPLICATION;
use crate::lab::LAB_KEYRING_APPLICATION;

/// A cloneable handle to the credential backend selected at startup.
#[derive(Debug)]
pub struct AppSecrets {
    backend: Backend,
}

#[derive(Debug)]
enum Backend {
    Desktop(LazyKeyring),
    Lab(Arc<BrokerSecretStore>),
    Memory(Arc<MemorySecretStore>),
}

impl Clone for AppSecrets {
    fn clone(&self) -> Self {
        match &self.backend {
            // The operation that first connects owns its runtime. Do not carry that runtime's D-Bus connection
            // into another operation's temporary runtime or the account engine's long-lived one.
            Backend::Desktop(_) => Self::personal(),
            Backend::Lab(store) => Self {
                backend: Backend::Lab(Arc::clone(store)),
            },
            Backend::Memory(store) => Self {
                backend: Backend::Memory(Arc::clone(store)),
            },
        }
    }
}

impl AppSecrets {
    /// The desktop's own namespace, connected lazily on the first operation.
    #[must_use]
    pub fn personal() -> Self {
        Self {
            backend: Backend::Desktop(LazyKeyring::new(KEYRING_APPLICATION)),
        }
    }

    /// A lab account's broker socket, always in the lab namespace.
    ///
    /// # Errors
    /// The socket cannot be put in blocking mode.
    pub fn lab(socket: BrokerStream) -> io::Result<Self> {
        Ok(Self {
            backend: Backend::Lab(Arc::new(BrokerSecretStore::new(socket, LAB_KEYRING_APPLICATION)?)),
        })
    }

    /// A store kept in this process alone and gone when it exits, standing in for the desktop's: the UI sandbox has
    /// no Secret Service, and a test's accounts must never reach the user's. Nothing in the app selects it.
    #[must_use]
    pub fn in_memory() -> Self {
        Self {
            backend: Backend::Memory(Arc::new(MemorySecretStore::new())),
        }
    }

    /// The selected namespace; never configurable by a caller.
    #[must_use]
    pub fn application(&self) -> &'static str {
        match self.backend {
            Backend::Desktop(_) | Backend::Memory(_) => KEYRING_APPLICATION,
            Backend::Lab(_) => LAB_KEYRING_APPLICATION,
        }
    }
}

impl SecretStore for AppSecrets {
    async fn put(&self, key: &SecretKey, secret: &Secret) -> Result<(), SecretError> {
        match &self.backend {
            Backend::Desktop(store) => store.put(key, secret).await,
            Backend::Lab(store) => store.put(key, secret).await,
            Backend::Memory(store) => store.put(key, secret).await,
        }
    }

    async fn get(&self, key: &SecretKey) -> Result<Option<Secret>, SecretError> {
        match &self.backend {
            Backend::Desktop(store) => store.get(key).await,
            Backend::Lab(store) => store.get(key).await,
            Backend::Memory(store) => store.get(key).await,
        }
    }

    async fn delete(&self, key: &SecretKey) -> Result<(), SecretError> {
        match &self.backend {
            Backend::Desktop(store) => store.delete(key).await,
            Backend::Lab(store) => store.delete(key).await,
            Backend::Memory(store) => store.delete(key).await,
        }
    }
}

/// One lab account's broker scope: reads and changes, always under the lab namespace.
#[must_use]
pub fn lab_broker_scope(upn: &str) -> BrokerScope {
    BrokerScope::new(LAB_KEYRING_APPLICATION, upn)
}
