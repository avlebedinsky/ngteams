//! Where presence state lives: hashes and sorted sets, in Redis when the server has one (every node reads and writes
//! the same state) or in the node. [`key`] names every Redis key the presence service uses.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, PoisonError};

use async_trait::async_trait;
use redis::AsyncCommands as _;
use redis::aio::ConnectionManager;

/// The presence state can't be reached.
#[derive(Debug, thiserror::Error)]
#[error("the presence state is unavailable: {0}")]
pub struct BoardError(String);

impl From<redis::RedisError> for BoardError {
    fn from(error: redis::RedisError) -> Self {
        Self(error.to_string())
    }
}

/// The Redis key of the presence state `kind` of `id`. Every key of the service is made here.
#[must_use]
pub(super) fn key(kind: &str, id: &str) -> String {
    format!("ngts:presence:{kind}:{id}")
}

/// The few hash and sorted-set commands the presence service needs.
#[async_trait]
pub(super) trait Board: Send + Sync + std::fmt::Debug {
    /// Sets `field` of the hash `key`, which then lives `ttl` seconds more.
    async fn hset(&self, key: &str, field: &str, value: &str, ttl: u64) -> Result<(), BoardError>;
    async fn hdel(&self, key: &str, fields: &[String]) -> Result<(), BoardError>;
    async fn hgetall(&self, key: &str) -> Result<Vec<(String, String)>, BoardError>;
    async fn zadd(&self, key: &str, member: &str, score: i64) -> Result<(), BoardError>;
    /// Whether `member` was there.
    async fn zrem(&self, key: &str, member: &str) -> Result<bool, BoardError>;
    /// At most `limit` members scored `min..=max`, lowest first.
    async fn zrange(&self, key: &str, min: i64, max: i64, limit: usize) -> Result<Vec<String>, BoardError>;
    async fn zscore(&self, key: &str, member: &str) -> Result<Option<i64>, BoardError>;
    /// Removes the members scored up to `max`.
    async fn zprune(&self, key: &str, max: i64) -> Result<(), BoardError>;
    async fn del(&self, key: &str) -> Result<(), BoardError>;
}

/// One node's state.
#[derive(Debug, Default)]
pub(super) struct Memory {
    hashes: Mutex<HashMap<String, BTreeMap<String, String>>>,
    sets: Mutex<HashMap<String, BTreeMap<String, i64>>>,
}

impl Memory {
    fn hashes(&self) -> std::sync::MutexGuard<'_, HashMap<String, BTreeMap<String, String>>> {
        self.hashes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn sets(&self) -> std::sync::MutexGuard<'_, HashMap<String, BTreeMap<String, i64>>> {
        self.sets.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[async_trait]
impl Board for Memory {
    async fn hset(&self, key: &str, field: &str, value: &str, _ttl: u64) -> Result<(), BoardError> {
        self.hashes()
            .entry(key.to_owned())
            .or_default()
            .insert(field.to_owned(), value.to_owned());
        Ok(())
    }

    async fn hdel(&self, key: &str, fields: &[String]) -> Result<(), BoardError> {
        let mut hashes = self.hashes();
        if let Some(hash) = hashes.get_mut(key) {
            hash.retain(|field, _| !fields.contains(field));
            if hash.is_empty() {
                hashes.remove(key);
            }
        }
        Ok(())
    }

    async fn hgetall(&self, key: &str) -> Result<Vec<(String, String)>, BoardError> {
        let hashes = self.hashes();
        Ok(hashes
            .get(key)
            .map(|hash| {
                hash.iter()
                    .map(|(field, value)| (field.clone(), value.clone()))
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn zadd(&self, key: &str, member: &str, score: i64) -> Result<(), BoardError> {
        self.sets()
            .entry(key.to_owned())
            .or_default()
            .insert(member.to_owned(), score);
        Ok(())
    }

    async fn zrem(&self, key: &str, member: &str) -> Result<bool, BoardError> {
        let mut sets = self.sets();
        let Some(set) = sets.get_mut(key) else {
            return Ok(false);
        };
        let removed = set.remove(member).is_some();
        if set.is_empty() {
            sets.remove(key);
        }
        Ok(removed)
    }

    async fn zrange(&self, key: &str, min: i64, max: i64, limit: usize) -> Result<Vec<String>, BoardError> {
        let sets = self.sets();
        let mut members: Vec<(&String, &i64)> = sets
            .get(key)
            .into_iter()
            .flatten()
            .filter(|(_, score)| (min..=max).contains(*score))
            .collect();
        members.sort_by_key(|(member, score)| (**score, *member));
        Ok(members
            .into_iter()
            .take(limit)
            .map(|(member, _)| member.clone())
            .collect())
    }

    async fn zscore(&self, key: &str, member: &str) -> Result<Option<i64>, BoardError> {
        Ok(self.sets().get(key).and_then(|set| set.get(member).copied()))
    }

    async fn zprune(&self, key: &str, max: i64) -> Result<(), BoardError> {
        let mut sets = self.sets();
        if let Some(set) = sets.get_mut(key) {
            set.retain(|_, score| *score > max);
            if set.is_empty() {
                sets.remove(key);
            }
        }
        Ok(())
    }

    async fn del(&self, key: &str) -> Result<(), BoardError> {
        self.hashes().remove(key);
        self.sets().remove(key);
        Ok(())
    }
}

/// The state every node shares.
pub(super) struct Shared(pub ConnectionManager);

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Shared")
    }
}

#[async_trait]
impl Board for Shared {
    async fn hset(&self, key: &str, field: &str, value: &str, ttl: u64) -> Result<(), BoardError> {
        let mut redis = self.0.clone();
        let ttl = i64::try_from(ttl).unwrap_or(i64::MAX);
        let () = redis::pipe()
            .atomic()
            .hset(key, field, value)
            .ignore()
            .expire(key, ttl)
            .ignore()
            .query_async(&mut redis)
            .await?;
        Ok(())
    }

    async fn hdel(&self, key: &str, fields: &[String]) -> Result<(), BoardError> {
        if fields.is_empty() {
            return Ok(());
        }
        Ok(self.0.clone().hdel(key, fields).await?)
    }

    async fn hgetall(&self, key: &str) -> Result<Vec<(String, String)>, BoardError> {
        Ok(self.0.clone().hgetall(key).await?)
    }

    async fn zadd(&self, key: &str, member: &str, score: i64) -> Result<(), BoardError> {
        Ok(self.0.clone().zadd(key, member, score).await?)
    }

    async fn zrem(&self, key: &str, member: &str) -> Result<bool, BoardError> {
        let removed: i64 = self.0.clone().zrem(key, member).await?;
        Ok(removed > 0)
    }

    async fn zrange(&self, key: &str, min: i64, max: i64, limit: usize) -> Result<Vec<String>, BoardError> {
        let limit = isize::try_from(limit).unwrap_or(isize::MAX);
        Ok(self.0.clone().zrangebyscore_limit(key, min, max, 0, limit).await?)
    }

    async fn zscore(&self, key: &str, member: &str) -> Result<Option<i64>, BoardError> {
        Ok(self.0.clone().zscore(key, member).await?)
    }

    async fn zprune(&self, key: &str, max: i64) -> Result<(), BoardError> {
        Ok(self.0.clone().zrembyscore(key, i64::MIN, max).await?)
    }

    async fn del(&self, key: &str) -> Result<(), BoardError> {
        Ok(self.0.clone().del(key).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_nodes_own_board_keeps_hashes_and_sorted_sets() {
        let board = Memory::default();
        board.hset("h", "a", "1", 60).await.unwrap();
        board.hset("h", "b", "2", 60).await.unwrap();
        board.hdel("h", &["a".to_owned()]).await.unwrap();
        board.hdel("nothing", &["a".to_owned()]).await.unwrap();
        assert_eq!(board.hgetall("h").await.unwrap(), [("b".to_owned(), "2".to_owned())]);
        board.hdel("h", &["b".to_owned()]).await.unwrap();
        assert!(board.hgetall("h").await.unwrap().is_empty());

        for (member, score) in [("late", 30), ("early", 10), ("mid", 20)] {
            board.zadd("z", member, score).await.unwrap();
        }
        assert_eq!(board.zrange("z", 0, 20, 10).await.unwrap(), ["early", "mid"]);
        assert_eq!(board.zrange("z", 0, 99, 1).await.unwrap(), ["early"]);
        assert_eq!(board.zscore("z", "mid").await.unwrap(), Some(20));
        assert_eq!(board.zscore("z", "none").await.unwrap(), None);
        assert!(board.zrem("z", "mid").await.unwrap());
        assert!(!board.zrem("z", "mid").await.unwrap());
        assert!(!board.zrem("nothing", "mid").await.unwrap());
        board.zprune("z", 10).await.unwrap();
        board.zprune("nothing", 10).await.unwrap();
        assert_eq!(board.zrange("z", 0, 99, 10).await.unwrap(), ["late"]);
        board.zprune("z", 99).await.unwrap();
        board.zadd("z", "again", 1).await.unwrap();
        board.del("z").await.unwrap();
        assert!(board.zrange("z", 0, 99, 10).await.unwrap().is_empty());
        assert!(format!("{board:?}").contains("Memory"));
        assert_eq!(key("account", "x"), "ngts:presence:account:x");
    }
}
