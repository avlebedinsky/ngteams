//! Secret storage for refresh tokens and skypetokens, one secret per account and audience.
//!
//! [`KeyringSecretStore`] keeps them in the Secret Service (gnome-keyring) through `oo7`; [`MemorySecretStore`] is the
//! in-memory backend for tests; a sandboxed process reaches one account's items through the credential broker
//! ([`crate::broker::BrokerSecretStore`]). A [`Secret`] never shows its value in `Debug` and has no `Display`: the only way to the
//! value is [`Secret::expose`], so a token can't reach a log by accident.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Mutex, PoisonError};

use tokio::sync::OnceCell;
use zeroize::Zeroizing;

/// A secret value: a refresh token, an access token or a skypetoken. The memory is zeroed on drop.
#[derive(Clone)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }

    /// The secret value. Callers pass it straight to the request that needs it, never to a log or stdout.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// Where a secret lives: the account (its UPN) and the audience the secret is for (a resource such as
/// `https://api.spaces.skype.com`, or a name such as `skypetoken`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SecretKey {
    account: String,
    audience: String,
}

impl SecretKey {
    #[must_use]
    pub fn new(account: impl Into<String>, audience: impl Into<String>) -> Self {
        Self {
            account: account.into(),
            audience: audience.into(),
        }
    }

    #[must_use]
    pub fn account(&self) -> &str {
        &self.account
    }

    #[must_use]
    pub fn audience(&self) -> &str {
        &self.audience
    }
}

/// Why a store operation failed. Never carries a secret value.
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("Secret Service: {0}")]
    Keyring(Box<oo7::Error>),
    #[error("the stored secret is not UTF-8")]
    NotUtf8,
    /// The credential broker's store ([`crate::broker::BrokerSecretStore`]) didn't do it.
    #[error(transparent)]
    Broker(#[from] crate::broker::BrokerError),
}

impl From<oo7::Error> for SecretError {
    fn from(error: oo7::Error) -> Self {
        Self::Keyring(Box::new(error))
    }
}

/// A store of secrets keyed by account and audience. `put` replaces, `delete` of a missing key succeeds.
///
/// The methods are async because the Secret Service is reached over D-Bus; callers run them on their tokio runtime.
pub trait SecretStore: Send + Sync {
    fn put(&self, key: &SecretKey, secret: &Secret) -> impl Future<Output = Result<(), SecretError>> + Send;
    fn get(&self, key: &SecretKey) -> impl Future<Output = Result<Option<Secret>, SecretError>> + Send;
    fn delete(&self, key: &SecretKey) -> impl Future<Output = Result<(), SecretError>> + Send;
}

/// An in-memory store, for tests.
#[derive(Debug, Default)]
pub struct MemorySecretStore {
    secrets: Mutex<BTreeMap<SecretKey, Secret>>,
}

impl MemorySecretStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn with_secrets<T>(&self, f: impl FnOnce(&mut BTreeMap<SecretKey, Secret>) -> T) -> T {
        // A panic while holding the lock can't leave the map half-updated: every operation is a single map call.
        f(&mut self.secrets.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl SecretStore for MemorySecretStore {
    fn put(&self, key: &SecretKey, secret: &Secret) -> impl Future<Output = Result<(), SecretError>> + Send {
        self.with_secrets(|secrets| secrets.insert(key.clone(), secret.clone()));
        std::future::ready(Ok(()))
    }

    fn get(&self, key: &SecretKey) -> impl Future<Output = Result<Option<Secret>, SecretError>> + Send {
        std::future::ready(Ok(self.with_secrets(|secrets| secrets.get(key).cloned())))
    }

    fn delete(&self, key: &SecretKey) -> impl Future<Output = Result<(), SecretError>> + Send {
        self.with_secrets(|secrets| secrets.remove(key));
        std::future::ready(Ok(()))
    }
}

/// A store that reads through to `inner` and keeps every change in memory: a `put` and a `delete` never reach `inner`
/// (the personal build's read-only mode: a token renewed while it runs, a rotated refresh token included, lives in
/// this process only, and the user's stored items stay as they were). A key put or deleted here is answered from
/// memory from then on; any other is read from `inner`.
#[derive(Debug)]
pub struct OverlaySecretStore<S> {
    inner: S,
    changed: Mutex<BTreeMap<SecretKey, Change>>,
}

/// What became of a key in an [`OverlaySecretStore`].
#[derive(Clone, Debug)]
enum Change {
    Put(Secret),
    Deleted,
}

impl<S> OverlaySecretStore<S> {
    #[must_use]
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            changed: Mutex::new(BTreeMap::new()),
        }
    }

    /// The store underneath, which this one never writes.
    #[must_use]
    pub fn inner(&self) -> &S {
        &self.inner
    }

    fn change(&self, key: &SecretKey, change: Change) {
        self.changed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.clone(), change);
    }

    fn changed(&self, key: &SecretKey) -> Option<Change> {
        self.changed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .cloned()
    }
}

impl<S: SecretStore> SecretStore for OverlaySecretStore<S> {
    fn put(&self, key: &SecretKey, secret: &Secret) -> impl Future<Output = Result<(), SecretError>> + Send {
        self.change(key, Change::Put(secret.clone()));
        std::future::ready(Ok(()))
    }

    async fn get(&self, key: &SecretKey) -> Result<Option<Secret>, SecretError> {
        match self.changed(key) {
            Some(Change::Put(secret)) => Ok(Some(secret)),
            Some(Change::Deleted) => Ok(None),
            None => self.inner.get(key).await,
        }
    }

    fn delete(&self, key: &SecretKey) -> impl Future<Output = Result<(), SecretError>> + Send {
        self.change(key, Change::Deleted);
        std::future::ready(Ok(()))
    }
}

/// A store in the Secret Service's default collection (gnome-keyring), through `oo7`.
///
/// Every item carries the attributes `application`, `account` and `audience`; `application` namespaces the store, so
/// tests use their own and never touch the lab's items.
#[derive(Debug)]
pub struct KeyringSecretStore {
    keyring: oo7::Keyring,
    application: String,
}

impl KeyringSecretStore {
    /// Connects to the Secret Service. `application` is the namespace for this store's items (the lab uses
    /// `teams-lab`).
    ///
    /// # Errors
    /// When the Secret Service can't be reached.
    pub async fn new(application: impl Into<String>) -> Result<Self, SecretError> {
        Ok(Self {
            keyring: oo7::Keyring::new().await?,
            application: application.into(),
        })
    }

    fn attributes<'a>(&'a self, key: &'a SecretKey) -> [(&'static str, &'a str); 3] {
        [
            ("application", self.application.as_str()),
            ("account", key.account()),
            ("audience", key.audience()),
        ]
    }

    async fn unlock(&self) -> Result<(), SecretError> {
        if self.keyring.is_locked().await? {
            self.keyring.unlock().await?;
        }
        Ok(())
    }
}

impl SecretStore for KeyringSecretStore {
    async fn put(&self, key: &SecretKey, secret: &Secret) -> Result<(), SecretError> {
        self.unlock().await?;
        let label = format!("{}: {} for {}", self.application, key.audience(), key.account());
        let value = oo7::Secret::text(secret.expose());
        self.keyring
            .create_item(&label, &self.attributes(key), value, true)
            .await?;
        Ok(())
    }

    async fn get(&self, key: &SecretKey) -> Result<Option<Secret>, SecretError> {
        self.unlock().await?;
        let items = self.keyring.search_items(&self.attributes(key)).await?;
        let Some(item) = items.first() else {
            return Ok(None);
        };
        if item.is_locked().await? {
            item.unlock().await?;
        }
        let value = item.secret().await?;
        let text = std::str::from_utf8(&value).map_err(|_| SecretError::NotUtf8)?;
        Ok(Some(Secret::new(text)))
    }

    async fn delete(&self, key: &SecretKey) -> Result<(), SecretError> {
        self.unlock().await?;
        self.keyring.delete(&self.attributes(key)).await?;
        Ok(())
    }
}

/// The Secret Service store of `application`, connected on its first use and kept from then on; a failed connection
/// is tried again on the next request.
#[derive(Debug)]
pub struct LazyKeyring {
    application: String,
    keyring: OnceCell<KeyringSecretStore>,
}

impl LazyKeyring {
    #[must_use]
    pub fn new(application: impl Into<String>) -> Self {
        Self {
            application: application.into(),
            keyring: OnceCell::new(),
        }
    }

    /// Whether the Secret Service has been connected.
    #[must_use]
    pub fn connected(&self) -> bool {
        self.keyring.initialized()
    }

    async fn keyring(&self) -> Result<&KeyringSecretStore, SecretError> {
        self.keyring
            .get_or_try_init(|| KeyringSecretStore::new(self.application.clone()))
            .await
    }
}

impl SecretStore for LazyKeyring {
    async fn put(&self, key: &SecretKey, secret: &Secret) -> Result<(), SecretError> {
        self.keyring().await?.put(key, secret).await
    }

    async fn get(&self, key: &SecretKey) -> Result<Option<Secret>, SecretError> {
        self.keyring().await?.get(key).await
    }

    async fn delete(&self, key: &SecretKey) -> Result<(), SecretError> {
        self.keyring().await?.delete(key).await
    }
}
