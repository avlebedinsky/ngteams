//! The `SecretStore` contract: put/get/delete per account and audience, and nothing ever prints a secret.
//!
//! The in-memory store runs everywhere. The Secret Service test is `#[ignore]`d: it writes into the user's real
//! keyring (under a test-only `application` attribute that it removes again, also when a check fails), so it runs only
//! on request: `cargo test -p teams-auth --test secrets -- --ignored`.

use std::fmt::Debug;

use teams_auth::secrets::{KeyringSecretStore, MemorySecretStore, Secret, SecretError, SecretKey, SecretStore};

const SPACES: &str = "https://api.spaces.skype.com";
const SKYPETOKEN: &str = "skypetoken";
const PLANTED: &str = "planted-secret-value-0123456789";

/// Why a contract run failed. Checks return this instead of panicking, so a failed check never skips the cleanup.
#[derive(Debug)]
struct Failure(String);

impl From<SecretError> for Failure {
    fn from(error: SecretError) -> Self {
        Self(format!("store: {error}"))
    }
}

fn ensure_eq<T: PartialEq + Debug>(actual: &T, expected: &T, what: &str) -> Result<(), Failure> {
    if actual == expected {
        Ok(())
    } else {
        Err(Failure(format!("{what}: got {actual:?}, expected {expected:?}")))
    }
}

fn key(account: &str, audience: &str) -> SecretKey {
    SecretKey::new(account, audience)
}

/// The stored value under `account` and `audience`, if any.
async fn value(store: &impl SecretStore, account: &str, audience: &str) -> Result<Option<String>, SecretError> {
    let secret = store.get(&key(account, audience)).await?;
    Ok(secret.map(|secret| secret.expose().to_owned()))
}

async fn put(store: &impl SecretStore, account: &str, audience: &str, value: &str) -> Result<(), SecretError> {
    store.put(&key(account, audience), &Secret::new(value)).await
}

/// The whole contract, run against any store. `account` is unique per run so a real keyring holds nothing else under
/// it. Every delete runs whether or not a check failed, so the run removes everything it stored unless the store
/// itself refuses a delete.
async fn exercise(store: &impl SecretStore, account: &str) -> Result<(), Failure> {
    let other = format!("{account}-other");
    let result = put_get_replace(store, account, &other).await;
    let cleanup = delete_all(store, account, &other).await;
    result.and(cleanup)
}

async fn put_get_replace(store: &impl SecretStore, account: &str, other: &str) -> Result<(), Failure> {
    ensure_eq(&value(store, account, SPACES).await?, &None, "starts empty")?;

    put(store, account, SPACES, "spaces-1").await?;
    put(store, account, SKYPETOKEN, "skype-1").await?;
    put(store, other, SPACES, "other-1").await?;
    ensure_eq(
        &value(store, account, SPACES).await?.as_deref(),
        &Some("spaces-1"),
        "get returns what put stored",
    )?;
    ensure_eq(
        &value(store, account, SKYPETOKEN).await?.as_deref(),
        &Some("skype-1"),
        "audiences are separate",
    )?;
    ensure_eq(
        &value(store, other, SPACES).await?.as_deref(),
        &Some("other-1"),
        "accounts are separate",
    )?;

    put(store, account, SPACES, "spaces-2").await?;
    ensure_eq(
        &value(store, account, SPACES).await?.as_deref(),
        &Some("spaces-2"),
        "put replaces",
    )
}

/// Runs every delete before looking at any result, so neither a failed check nor a failed read leaves an item behind.
async fn delete_all(store: &impl SecretStore, account: &str, other: &str) -> Result<(), Failure> {
    let deleted = store.delete(&key(account, SPACES)).await;
    let after_delete = value(store, account, SPACES).await;
    let untouched = value(store, account, SKYPETOKEN).await;
    let deleted_again = store.delete(&key(account, SPACES)).await;
    let deleted_skypetoken = store.delete(&key(account, SKYPETOKEN)).await;
    let deleted_other = store.delete(&key(other, SPACES)).await;
    let other_after_delete = value(store, other, SPACES).await;

    deleted?;
    ensure_eq(&after_delete?, &None, "delete removes")?;
    ensure_eq(&untouched?.as_deref(), &Some("skype-1"), "delete touches only its key")?;
    deleted_again?;
    deleted_skypetoken?;
    deleted_other?;
    ensure_eq(&other_after_delete?, &None, "delete removes the other account's item")
}

#[tokio::test]
async fn memory_store_keeps_secrets_per_account_and_audience() {
    exercise(&MemorySecretStore::new(), "a@lab.example.com").await.unwrap();
}

/// A store whose `put` adds instead of replacing: the contract run must report it and still leave the store empty.
#[derive(Debug, Default)]
struct KeepsFirstStore(MemorySecretStore);

impl SecretStore for KeepsFirstStore {
    async fn put(&self, key: &SecretKey, secret: &Secret) -> Result<(), SecretError> {
        if self.0.get(key).await?.is_none() {
            self.0.put(key, secret).await?;
        }
        Ok(())
    }

    async fn get(&self, key: &SecretKey) -> Result<Option<Secret>, SecretError> {
        self.0.get(key).await
    }

    async fn delete(&self, key: &SecretKey) -> Result<(), SecretError> {
        self.0.delete(key).await
    }
}

#[tokio::test]
async fn a_failed_check_still_removes_everything_stored() {
    let store = KeepsFirstStore::default();
    let account = "a@lab.example.com";
    let failure = exercise(&store, account).await.unwrap_err();
    assert!(failure.0.starts_with("put replaces"), "{failure:?}");
    for (account, audience) in [
        (account, SPACES),
        (account, SKYPETOKEN),
        ("a@lab.example.com-other", SPACES),
    ] {
        assert!(
            store.get(&key(account, audience)).await.unwrap().is_none(),
            "{account} {audience}"
        );
    }
}

#[test]
fn debug_never_prints_a_secret() {
    let secret = Secret::new(PLANTED);
    let shown = format!("{secret:?}");
    assert!(!shown.contains(PLANTED), "{shown}");
    assert!(shown.contains("redacted"), "{shown}");
    assert_eq!(secret.expose(), PLANTED);
}

#[tokio::test]
async fn debug_of_a_filled_memory_store_never_prints_a_secret() {
    let store = MemorySecretStore::new();
    store
        .put(&key("a@lab.example.com", SPACES), &Secret::new(PLANTED))
        .await
        .unwrap();
    let shown = format!("{store:?}");
    assert!(!shown.contains(PLANTED), "{shown}");
}

#[test]
fn a_secret_built_from_a_string_keeps_it() {
    let secret = Secret::from(String::from(PLANTED));
    assert_eq!(secret.expose(), PLANTED);
    assert_eq!(secret.clone().expose(), PLANTED);
}

#[test]
fn a_key_shows_its_account_and_audience() {
    let key = key("a@lab.example.com", SPACES);
    assert_eq!(key.account(), "a@lab.example.com");
    assert_eq!(key.audience(), SPACES);
}

#[tokio::test]
#[ignore = "writes into the user's real Secret Service keyring under a test-only attribute; run with --ignored"]
async fn keyring_store_keeps_secrets_per_account_and_audience() {
    let store = KeyringSecretStore::new("teams-lab-test").await.unwrap();
    let account = format!("keyring-test-{}@lab.example.com", std::process::id());
    exercise(&store, &account).await.unwrap();
    assert!(!format!("{store:?}").contains("spaces-"));
}
