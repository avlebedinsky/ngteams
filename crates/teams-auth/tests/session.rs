//! The session's tests (`session/cases.rs`), run twice: on a `MemorySecretStore` (`in_memory::cases::…`), and through
//! the credential broker (`through_the_broker::cases::…`: a `BrokerSecretStore` on a socket pair, the broker serving a
//! `MemorySecretStore` for the session's account), the way a sandboxed app's session reads and stores its tokens.

use std::fmt;
#[path = "support/broker_transport.rs"]
mod transport;
use std::sync::Arc;

use teams_auth::broker::{BrokerScope, BrokerSecretStore, serve};
use teams_auth::secrets::{MemorySecretStore, Secret, SecretError, SecretKey, SecretStore};

#[path = "session/in_memory.rs"]
mod in_memory;
#[path = "session/through_the_broker.rs"]
mod through_the_broker;

/// Where a run of the cases keeps the session's tokens.
#[derive(Clone, Copy, Debug)]
enum Kind {
    Memory,
    Broker,
}

/// The session's store: in memory, or the broker's client with the broker's own store kept for a look behind it.
enum TestStore {
    Memory(MemorySecretStore),
    Broker {
        client: BrokerSecretStore,
        behind: Arc<MemorySecretStore>,
    },
}

impl TestStore {
    /// A store of `kind`; a broker serves the lab's application for `account` on a task of the test's runtime until the
    /// client closes its end.
    fn new(kind: Kind, account: &str) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(match kind {
            Kind::Memory => Self::Memory(MemorySecretStore::new()),
            Kind::Broker => {
                let (server, client) = transport::pair()?;
                let behind = Arc::new(MemorySecretStore::new());
                let store = Arc::clone(&behind);
                let scope = BrokerScope::new("teams-lab", account);
                tokio::spawn(async move { serve(server, &*store, &scope, |_| {}).await });
                Self::Broker {
                    client: BrokerSecretStore::new(client, "teams-lab")?,
                    behind,
                }
            }
        })
    }

    /// The store the tokens end up in: the broker's own for a broker.
    fn behind(&self) -> &MemorySecretStore {
        match self {
            Self::Memory(store) => store,
            Self::Broker { behind, .. } => behind,
        }
    }
}

impl fmt::Debug for TestStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Memory(store) => store.fmt(f),
            Self::Broker { client, .. } => client.fmt(f),
        }
    }
}

impl SecretStore for TestStore {
    async fn put(&self, key: &SecretKey, secret: &Secret) -> Result<(), SecretError> {
        match self {
            Self::Memory(store) => store.put(key, secret).await,
            Self::Broker { client, .. } => client.put(key, secret).await,
        }
    }

    async fn get(&self, key: &SecretKey) -> Result<Option<Secret>, SecretError> {
        match self {
            Self::Memory(store) => store.get(key).await,
            Self::Broker { client, .. } => client.get(key).await,
        }
    }

    async fn delete(&self, key: &SecretKey) -> Result<(), SecretError> {
        match self {
            Self::Memory(store) => store.delete(key).await,
            Self::Broker { client, .. } => client.delete(key).await,
        }
    }
}
