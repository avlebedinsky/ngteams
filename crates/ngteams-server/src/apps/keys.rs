//! The keys the server signs activities to bots with: RSA-2048, RS256, as every bot SDK takes a channel's token (the
//! server's own `EdDSA` keys can't be used: no SDK accepts them). Kept in `bot_signing_keys`, so every node signs
//! with and publishes the same keys; rotated as the account keys are (`crate::auth::keys`): the newest unretired
//! key signs, a retired one is still published until nothing it signed can be used, then deleted.
//!
//! The published set is read from the database on each request, so a key another node just made is published by
//! every node at once; a node signs with its own view, reloaded at housekeeping.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use aws_lc_rs::encoding::{AsDer as _, Pkcs8V1Der};
use aws_lc_rs::rsa::{KeyPair, KeySize, PublicKeyComponents};
use aws_lc_rs::signature::KeyPair as _;
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use diesel::prelude::*;
use diesel_async::{AsyncConnection as _, RunQueryDsl};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::Serialize;
use serde_json::{Value, json};

use super::schema::bot_signing_keys;
use crate::auth::keys::KeyError;
use crate::config::Config;
use crate::db::schema::server_meta;
use crate::db::{Db, DbError, now, seconds, with_conn};

/// The channel the server speaks as: an SDK takes a key only for a channel it endorses.
pub const CHANNEL: &str = "msteams";
/// The clock skew the SDKs allow when they check a token.
const SDK_SKEW: Duration = Duration::from_secs(300);
/// The `server_meta` row a rotation locks, so two nodes don't both leave a signer.
const LOCK: &str = "bot_signing_key_lock";

#[derive(Clone, Queryable, Selectable, Insertable)]
#[diesel(table_name = bot_signing_keys)]
struct Stored {
    kid: String,
    /// PKCS#8.
    private_der: Vec<u8>,
    created_at: i64,
    retired_at: Option<i64>,
}

struct Signer {
    kid: String,
    key: EncodingKey,
}

/// What an activity's token says: exactly what a channel's token says, so no SDK takes it for an emulator's or a
/// skill's.
#[derive(Serialize)]
struct ActivityClaims<'a> {
    iss: &'a str,
    aud: &'a str,
    serviceurl: &'a str,
    iat: i64,
    nbf: i64,
    exp: i64,
}

/// The node's view of the bot signing keys.
pub struct BotKeys {
    db: Db,
    signer: RwLock<Option<Arc<Signer>>>,
    /// Public keys by `kid`, as published: worked out once per key.
    published: Mutex<HashMap<String, Value>>,
    issuer: String,
    rotation: Duration,
    lifetime: Duration,
    keep: Duration,
}

impl std::fmt::Debug for BotKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let signer = self.signer.read().unwrap_or_else(PoisonError::into_inner);
        f.debug_struct("BotKeys")
            .field("signing", &signer.as_ref().map(|signer| signer.kid.clone()))
            .finish_non_exhaustive()
    }
}

/// How long a retired key stays published: as long as a token it signed may still be taken, plus the time other
/// nodes may still sign with it before they reload (three housekeeping rounds).
#[must_use]
pub fn retention(config: &Config) -> Duration {
    config.bots.activity_token_lifetime() + SDK_SKEW + 3 * crate::app::MAINTENANCE
}

impl BotKeys {
    /// Loads the keys, making the first one or rotating as due.
    ///
    /// # Errors
    /// A database failure, or a key that can't be made.
    pub async fn load(db: Db, config: &Config) -> Result<Self, KeyError> {
        let keys = Self {
            db,
            signer: RwLock::default(),
            published: Mutex::default(),
            issuer: super::issuer(config),
            rotation: config.bots.key_rotation(),
            lifetime: config.bots.activity_token_lifetime(),
            keep: retention(config),
        };
        keys.rotate_if_due().await?;
        Ok(keys)
    }

    /// Makes a new key when the newest is older than the rotation period (or there is none), deletes the keys
    /// retired longer ago than [`retention`], and reloads.
    ///
    /// # Errors
    /// A database failure, or a key that can't be made.
    pub async fn rotate_if_due(&self) -> Result<(), KeyError> {
        let at = now();
        let fresh = all(&self.db)
            .await?
            .iter()
            .find(|key| key.retired_at.is_none())
            .is_some_and(|key| at - key.created_at < seconds(self.rotation));
        if !fresh {
            self.rotate().await?;
        }
        delete_retired(&self.db, at - seconds(self.keep)).await?;
        self.reload().await
    }

    /// Makes a new signing key now and retires the others (they stay published); its `kid`.
    ///
    /// # Errors
    /// A database failure, or a key that can't be made.
    pub async fn rotate(&self) -> Result<String, KeyError> {
        let key = tokio::task::spawn_blocking(generate)
            .await
            .map_err(|_| KeyError::Generate)??;
        let kid = key.kid.clone();
        store(&self.db, &key).await?;
        self.reload().await?;
        Ok(kid)
    }

    /// Reads the signer again.
    ///
    /// # Errors
    /// A database failure.
    pub async fn reload(&self) -> Result<(), KeyError> {
        let signer = all(&self.db)
            .await?
            .into_iter()
            .find(|key| key.retired_at.is_none())
            .map(|key| -> Result<Arc<Signer>, KeyError> {
                let key_pem = pem(&key.private_der);
                Ok(Arc::new(Signer {
                    key: EncodingKey::from_rsa_pem(key_pem.as_bytes()).map_err(|_| KeyError::Sign)?,
                    kid: key.kid,
                }))
            })
            .transpose()?;
        *self.signer.write().unwrap_or_else(PoisonError::into_inner) = signer;
        Ok(())
    }

    /// The token an activity to the bot `bot` carries, for an activity whose `serviceUrl` is `service_url`.
    ///
    /// # Errors
    /// No key, or signing fails.
    pub fn activity_token(&self, bot: &str, service_url: &str) -> Result<String, KeyError> {
        let signer = self
            .signer
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or(KeyError::NoKey)?;
        let iat = now();
        let claims = ActivityClaims {
            iss: &self.issuer,
            aud: bot,
            serviceurl: service_url,
            iat,
            nbf: iat,
            exp: iat + seconds(self.lifetime),
        };
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(signer.kid.clone());
        jsonwebtoken::encode(&header, &claims, &signer.key).map_err(|_| KeyError::Sign)
    }

    /// The JWKS: every key in the database, newest first, endorsed for the server's channel.
    ///
    /// # Errors
    /// A database failure.
    pub async fn published(&self) -> Result<Value, DbError> {
        let stored = all(&self.db).await?;
        let mut cache = self.published.lock().unwrap_or_else(PoisonError::into_inner);
        cache.retain(|kid, _| stored.iter().any(|key| key.kid == *kid));
        let keys: Vec<Value> = stored
            .iter()
            .filter_map(|key| {
                if let Some(jwk) = cache.get(&key.kid) {
                    return Some(jwk.clone());
                }
                let jwk = jwk(key)?;
                cache.insert(key.kid.clone(), jwk.clone());
                Some(jwk)
            })
            .collect();
        Ok(json!({ "keys": keys }))
    }
}

/// Housekeeping: a new key when due, and the old ones dropped.
pub(crate) async fn rotate(app: &crate::app::App) {
    if let Err(error) = app.bot_keys.rotate_if_due().await {
        tracing::warn!(%error, "bot key rotation failed");
    }
}

/// Every key, newest first.
async fn all(db: &Db) -> Result<Vec<Stored>, DbError> {
    with_conn!(db, |conn| Ok(bot_signing_keys::table
        .order((bot_signing_keys::created_at.desc(), bot_signing_keys::kid.desc()))
        .select(Stored::as_select())
        .load(conn)
        .await?))
}

/// Deletes the keys retired before `before`.
async fn delete_retired(db: &Db, before: i64) -> Result<usize, DbError> {
    with_conn!(db, |conn| Ok(diesel::delete(
        bot_signing_keys::table.filter(bot_signing_keys::retired_at.lt(before))
    )
    .execute(conn)
    .await?))
}

/// Stores the new signer and retires the others, under the lock every node takes.
async fn store(db: &Db, key: &Stored) -> Result<(), DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            diesel::insert_into(server_meta::table)
                .values((server_meta::name.eq(LOCK), server_meta::value.eq("")))
                .on_conflict_do_nothing()
                .execute(conn)
                .await?;
            diesel::update(server_meta::table.find(LOCK))
                .set(server_meta::value.eq(server_meta::value))
                .execute(conn)
                .await?;
            diesel::insert_into(bot_signing_keys::table)
                .values(key)
                .execute(conn)
                .await?;
            diesel::update(
                bot_signing_keys::table
                    .filter(bot_signing_keys::kid.ne(&key.kid))
                    .filter(bot_signing_keys::retired_at.is_null()),
            )
            .set(bot_signing_keys::retired_at.eq(Some(key.created_at)))
            .execute(conn)
            .await?;
            Ok(())
        })
        .await)
}

/// A fresh RSA-2048 key with a random `kid`.
fn generate() -> Result<Stored, KeyError> {
    let pair = KeyPair::generate(KeySize::Rsa2048).map_err(|_| KeyError::Generate)?;
    let der: Pkcs8V1Der<'static> = pair.as_der().map_err(|_| KeyError::Generate)?;
    Ok(Stored {
        kid: uuid::Uuid::new_v4().simple().to_string(),
        private_der: der.as_ref().to_vec(),
        created_at: now(),
        retired_at: None,
    })
}

/// A key's public half as a JWK; none for a row that isn't an RSA key.
fn jwk(key: &Stored) -> Option<Value> {
    let pair = KeyPair::from_pkcs8(&key.private_der).ok()?;
    let public = PublicKeyComponents::<Vec<u8>>::from(pair.public_key());
    Some(json!({
        "kty": "RSA",
        "use": "sig",
        "alg": "RS256",
        "kid": key.kid,
        "n": URL_SAFE_NO_PAD.encode(&public.n),
        "e": URL_SAFE_NO_PAD.encode(&public.e),
        "endorsements": [CHANNEL],
    }))
}

/// PKCS#8 as PEM, the form `jsonwebtoken` reads an RSA key from.
fn pem(der: &[u8]) -> String {
    let body = STANDARD.encode(der);
    let mut pem = String::from("-----BEGIN PRIVATE KEY-----\n");
    for line in body.as_bytes().chunks(64) {
        pem.push_str(&String::from_utf8_lossy(line));
        pem.push('\n');
    }
    pem.push_str("-----END PRIVATE KEY-----\n");
    pem
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_key_is_published_as_a_2048_bit_rsa_jwk_and_garbage_is_not() {
        let key = generate().unwrap();
        let public = jwk(&key).unwrap();
        assert_eq!(public["e"], "AQAB");
        assert_eq!(public["n"].as_str().unwrap().len(), 342);
        assert_eq!(public["endorsements"], json!(["msteams"]));
        let garbage = Stored {
            private_der: vec![1, 2, 3],
            ..key
        };
        assert!(jwk(&garbage).is_none());
        assert!(EncodingKey::from_rsa_pem(pem(&garbage.private_der).as_bytes()).is_err());
    }
}
