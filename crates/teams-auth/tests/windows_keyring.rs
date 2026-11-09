//! Synthetic credentials in a unique namespace, accessed only by exact key. Never enumerate or read user entries.
#![cfg(windows)]
#![cfg(test)]

use teams_auth::secrets::{KeyringSecretStore, LazyKeyring, Secret, SecretKey, SecretStore};

#[tokio::test]
async fn native_entries_are_persistent_scoped_replaced_and_deleted() {
    let mut random = [0; 16];
    getrandom::fill(&mut random).unwrap();
    let namespace = format!("ngteams-synthetic-test-{random:x?}");
    let store = KeyringSecretStore::new(&namespace).await.unwrap();
    let other = KeyringSecretStore::new(format!("{namespace}-other")).await.unwrap();
    let keys = [
        SecretKey::new("synthetic-a@example.test", "spaces"),
        SecretKey::new("synthetic-a@example.test", "skype"),
        SecretKey::new("synthetic-b@example.test", "spaces"),
    ];
    // Return errors instead of asserting until cleanup has run, so even a failed contract removes its own items.
    let result = exercise(&namespace, &store, &other, &keys).await;
    for key in &keys {
        store.delete(key).await.unwrap();
        store.delete(key).await.unwrap();
        other.delete(key).await.unwrap();
    }
    result.unwrap();
}

async fn exercise(
    namespace: &str,
    store: &KeyringSecretStore,
    other: &KeyringSecretStore,
    keys: &[SecretKey; 3],
) -> Result<(), Box<dyn std::error::Error>> {
    for (index, key) in keys.iter().enumerate() {
        if store.get(key).await?.is_some() {
            return Err("synthetic key was not empty".into());
        }
        store.put(key, &Secret::new(format!("synthetic-value-{index}"))).await?;
        if other.get(key).await?.is_some() {
            return Err("credential crossed namespaces".into());
        }
    }
    let reopened = KeyringSecretStore::new(namespace).await?;
    for (index, key) in keys.iter().enumerate() {
        let expected = format!("synthetic-value-{index}");
        if reopened.get(key).await?.as_ref().map(Secret::expose) != Some(expected.as_str()) {
            return Err("credential did not persist or crossed keys".into());
        }
    }
    store.put(&keys[0], &Secret::new("synthetic-replacement")).await?;
    if store.get(&keys[0]).await?.as_ref().map(Secret::expose) != Some("synthetic-replacement") {
        return Err("put did not replace".into());
    }
    let lazy = LazyKeyring::new(namespace);
    if lazy.connected() {
        return Err("lazy store initialized early".into());
    }
    if lazy.get(&keys[0]).await?.as_ref().map(Secret::expose) != Some("synthetic-replacement") || !lazy.connected() {
        return Err("lazy store did not read the native entry".into());
    }
    lazy.put(&keys[0], &Secret::new("synthetic-lazy-replacement")).await?;
    lazy.delete(&keys[0]).await?;
    if lazy.get(&keys[0]).await?.is_some() {
        return Err("delete retained a credential".into());
    }
    Ok(())
}
