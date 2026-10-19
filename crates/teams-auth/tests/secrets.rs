//! The `SecretStore` contract: put/get/delete per account and audience, and nothing ever prints a secret.
//!
//! The in-memory store runs everywhere. The Secret Service test is `#[ignore]`d: it writes into the user's real
//! keyring (under a test-only `application` attribute that it removes again), so it runs only on request:
//! `cargo test -p teams-auth --test secrets -- --ignored`.

use teams_auth::secrets::{KeyringSecretStore, MemorySecretStore, Secret, SecretError, SecretKey, SecretStore};

const SPACES: &str = "https://api.spaces.skype.com";
const SKYPETOKEN: &str = "skypetoken";
const PLANTED: &str = "planted-secret-value-0123456789";

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
/// it, and the run removes everything it stored.
async fn exercise(store: &impl SecretStore, account: &str) -> Result<(), SecretError> {
    let other = format!("{account}-other");
    let result = put_get_replace(store, account, &other).await;
    let cleanup = delete_all(store, account, &other).await;
    result.and(cleanup)
}

async fn put_get_replace(store: &impl SecretStore, account: &str, other: &str) -> Result<(), SecretError> {
    assert_eq!(value(store, account, SPACES).await?, None, "starts empty");

    put(store, account, SPACES, "spaces-1").await?;
    put(store, account, SKYPETOKEN, "skype-1").await?;
    put(store, other, SPACES, "other-1").await?;
    assert_eq!(value(store, account, SPACES).await?.as_deref(), Some("spaces-1"));
    assert_eq!(
        value(store, account, SKYPETOKEN).await?.as_deref(),
        Some("skype-1"),
        "audiences are separate"
    );
    assert_eq!(
        value(store, other, SPACES).await?.as_deref(),
        Some("other-1"),
        "accounts are separate"
    );

    put(store, account, SPACES, "spaces-2").await?;
    assert_eq!(
        value(store, account, SPACES).await?.as_deref(),
        Some("spaces-2"),
        "put replaces"
    );
    Ok(())
}

async fn delete_all(store: &impl SecretStore, account: &str, other: &str) -> Result<(), SecretError> {
    store.delete(&key(account, SPACES)).await?;
    assert_eq!(value(store, account, SPACES).await?, None, "delete removes");
    assert_eq!(
        value(store, account, SKYPETOKEN).await?.as_deref(),
        Some("skype-1"),
        "delete touches only its key"
    );
    store.delete(&key(account, SPACES)).await?;

    store.delete(&key(account, SKYPETOKEN)).await?;
    store.delete(&key(other, SPACES)).await?;
    assert_eq!(value(store, other, SPACES).await?, None);
    Ok(())
}

#[tokio::test]
async fn memory_store_keeps_secrets_per_account_and_audience() {
    exercise(&MemorySecretStore::new(), "a@lab.example.com").await.unwrap();
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
