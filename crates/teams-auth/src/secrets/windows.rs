//! Windows Credential Manager through the maintained native keyring store. We select a store explicitly, use only
//! exact entry operations, and never enumerate the user's credentials. Targets hash a length-delimited namespace,
//! account and audience so Credential Manager's case-insensitive names cannot merge distinct keys.
//!
//! Operations run off the async executor and serialize within this store. Persistence is local to this machine,
//! matching the Linux desktop keyring. Third-party errors are reduced to fixed categories: keyring-core errors can
//! contain raw secret bytes and must never reach our Debug, broker error frames or diagnostic log.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, PoisonError};

use keyring_core::api::CredentialStoreApi as _;
use keyring_core::{Entry, Error};
use sha2::{Digest as _, Sha256};
use windows_native_keyring_store::Store;
use zeroize::{Zeroize as _, Zeroizing};

use super::{Secret, SecretError, SecretKey, SecretStore};

#[derive(Debug)]
pub struct KeyringSecretStore {
    native: Arc<Store>,
    application: String,
    serial: Arc<Mutex<()>>,
}

impl KeyringSecretStore {
    /// Selects Windows Credential Manager without reading any entries.
    ///
    /// # Errors
    /// The native credential store cannot be initialized.
    pub fn new(application: impl Into<String>) -> impl Future<Output = Result<Self, SecretError>> {
        std::future::ready(
            Store::new()
                .map(|native| Self {
                    native,
                    application: application.into(),
                    serial: Arc::new(Mutex::new(())),
                })
                .map_err(failed),
        )
    }

    fn entry(&self, key: &SecretKey) -> Result<Entry, SecretError> {
        let target = target(&self.application, key);
        let modifiers = HashMap::from([("target", target.as_str()), ("persistence", "Local")]);
        self.native
            .build("ngteams", key.account(), Some(&modifiers))
            .map_err(failed)
    }

    async fn execute<T, F>(&self, operation: F) -> Result<T, SecretError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, SecretError> + Send + 'static,
    {
        let serial = Arc::clone(&self.serial);
        tokio::task::spawn_blocking(move || {
            let _guard = serial.lock().unwrap_or_else(PoisonError::into_inner);
            operation()
        })
        .await
        .map_err(|_| SecretError::WindowsKeyring("credential worker failed"))?
    }
}

impl SecretStore for KeyringSecretStore {
    async fn put(&self, key: &SecretKey, secret: &Secret) -> Result<(), SecretError> {
        let entry = self.entry(key)?;
        let secret = secret.clone();
        self.execute(move || entry.set_secret(secret.expose().as_bytes()).map_err(failed))
            .await
    }

    async fn get(&self, key: &SecretKey) -> Result<Option<Secret>, SecretError> {
        let entry = self.entry(key)?;
        self.execute(move || match entry.get_secret() {
            Ok(bytes) => {
                let bytes = Zeroizing::new(bytes);
                let text = std::str::from_utf8(&bytes).map_err(|_| SecretError::NotUtf8)?;
                Ok(Some(Secret::new(text)))
            }
            Err(Error::NoEntry) => Ok(None),
            Err(error) => Err(failed(error)),
        })
        .await
    }

    async fn delete(&self, key: &SecretKey) -> Result<(), SecretError> {
        let entry = self.entry(key)?;
        self.execute(move || match entry.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(error) => Err(failed(error)),
        })
        .await
    }
}

fn target(application: &str, key: &SecretKey) -> String {
    let mut digest = Sha256::new();
    for part in [application, key.account(), key.audience()] {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part.as_bytes());
    }
    let mut name = String::from("ngteams:v1:");
    for byte in digest.finalize() {
        let _ = write!(&mut name, "{byte:02x}");
    }
    name
}

fn failed(error: Error) -> SecretError {
    let reason = match error {
        Error::BadEncoding(mut bytes) | Error::BadDataFormat(mut bytes, _) => {
            bytes.zeroize();
            "invalid credential data"
        }
        Error::TooLong(_, _) => "value exceeds the native credential limit",
        Error::Invalid(_, _) => "invalid credential attributes",
        Error::NoStorageAccess(_) => "credential store unavailable in this logon session",
        _ => "native credential operation failed",
    };
    SecretError::WindowsKeyring(reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_keep_namespaces_accounts_audiences_and_component_boundaries_distinct() {
        let mut targets = std::collections::BTreeSet::new();
        for (app, account, audience) in [
            ("a", "bc", "d"),
            ("ab", "c", "d"),
            ("a", "b", "cd"),
            ("a", "BC", "d"),
            ("A", "bc", "d"),
            ("a", "bc", "D"),
        ] {
            assert!(targets.insert(target(app, &SecretKey::new(account, audience))));
        }
        let name = target(
            "ngteams-lab",
            &SecretKey::new("synthetic@example.test", "https://example.test"),
        );
        assert_eq!(name.len(), "ngteams:v1:".len() + 64);
    }

    #[test]
    fn raw_keyring_errors_never_expose_credentials() {
        for error in [
            Error::BadEncoding(b"synthetic-private-value".to_vec()),
            Error::TooLong("synthetic-private-value".into(), 1),
            Error::Invalid("synthetic-private-value".into(), "synthetic-private-value".into()),
            Error::NoStorageAccess(Box::new(std::io::Error::other("synthetic-private-value"))),
            Error::NoEntry,
        ] {
            let error = failed(error);
            assert!(!format!("{error:?} {error}").contains("synthetic-private-value"));
        }
    }

    #[tokio::test]
    async fn invalid_native_bytes_are_reported_without_leaking_them() {
        let mut random = [0; 16];
        getrandom::fill(&mut random).unwrap();
        let store = KeyringSecretStore::new(format!("ngteams-synthetic-invalid-{random:x?}"))
            .await
            .unwrap();
        let key = SecretKey::new("synthetic@example.test", "invalid-encoding");
        store.entry(&key).unwrap().set_secret(&[0xff, 0xfe]).unwrap();
        let result = store.get(&key).await;
        store.delete(&key).await.unwrap();
        assert!(matches!(result, Err(SecretError::NotUtf8)));
    }
}
