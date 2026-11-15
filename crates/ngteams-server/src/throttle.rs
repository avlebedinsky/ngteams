//! Password attempts per account: after `max_failures` within the window, the account's password sign-ins are
//! held off until the window ends. Counted in Redis when the server has one (every node of the deployment sees the
//! same count, in the deployment's keyspace), else in the node.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use redis::AsyncCommands as _;
use sha2::{Digest as _, Sha256};

use crate::config::LoginConfig;
use crate::fanout::keyspace::Shared;

/// The attempts counted: per account, the count and when its window started.
type Windows = HashMap<String, (u32, Instant)>;

/// Where the counts live.
pub enum Throttle {
    Local {
        limits: LoginConfig,
        windows: Mutex<Windows>,
    },
    Redis {
        limits: LoginConfig,
        redis: Shared,
        /// What is counted (`login`, `setup`): the counts of one kind don't meet another's.
        kind: &'static str,
    },
}

impl std::fmt::Debug for Throttle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Local { .. } => "Throttle::Local",
            Self::Redis { .. } => "Throttle::Redis",
        })
    }
}

impl Throttle {
    #[must_use]
    pub fn local(limits: LoginConfig) -> Self {
        Self::Local {
            limits,
            windows: Mutex::default(),
        }
    }

    #[must_use]
    pub fn redis(limits: LoginConfig, redis: Shared, kind: &'static str) -> Self {
        Self::Redis { limits, redis, kind }
    }

    /// Reserves one password attempt before verification. Returns the remaining hold when the limit is exceeded.
    /// A Redis failure holds nothing off (a sign-in still needs the password).
    pub async fn reserve(&self, account: &str) -> Option<Duration> {
        match self {
            Self::Local { limits, windows } => {
                let mut windows = windows.lock().unwrap_or_else(PoisonError::into_inner);
                let window = Duration::from_secs(limits.window);
                windows.retain(|_, (_, started)| started.elapsed() < window);
                let (count, started) = windows.entry(account.to_owned()).or_insert((0, Instant::now()));
                let held = *count >= limits.max_failures;
                *count = count.saturating_add(1);
                held.then(|| window.saturating_sub(started.elapsed()))
            }
            Self::Redis { limits, redis, kind } => {
                let key = key(redis, kind, account);
                let mut redis = redis.commands();
                let result: redis::RedisResult<(u64, i64)> = redis::pipe()
                    .atomic()
                    .cmd("SET")
                    .arg(&key)
                    .arg(0)
                    .arg("NX")
                    .arg("EX")
                    .arg(limits.window)
                    .ignore()
                    .incr(&key, 1)
                    .ttl(&key)
                    .query_async(&mut redis)
                    .await;
                match result {
                    Ok((count, ttl)) => (count > u64::from(limits.max_failures))
                        .then(|| Duration::from_secs(u64::try_from(ttl).unwrap_or(limits.window).max(1))),
                    Err(error) => {
                        tracing::warn!(%error, "can't reserve a password sign-in");
                        None
                    }
                }
            }
        }
    }

    /// Forgets `account`'s failures after it signed in.
    pub async fn succeeded(&self, account: &str) {
        match self {
            Self::Local { windows, .. } => {
                windows.lock().unwrap_or_else(PoisonError::into_inner).remove(account);
            }
            Self::Redis { redis, kind, .. } => {
                let key = key(redis, kind, account);
                if let Err(error) = redis.commands().del::<_, ()>(key).await {
                    tracing::warn!(%error, "can't clear the login failures");
                }
            }
        }
    }
}

/// The Redis key of an account's count: its UPN hashed, so Redis holds no account names.
fn key(redis: &Shared, kind: &str, account: &str) -> String {
    let account = URL_SAFE_NO_PAD.encode(Sha256::digest(account.as_bytes()));
    redis.key(&format!("{kind}:{account}"))
}
