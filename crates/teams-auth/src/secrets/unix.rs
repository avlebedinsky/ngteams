//! Secret Service backend on Unix.

use super::{Secret, SecretError, SecretKey, SecretStore};

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
