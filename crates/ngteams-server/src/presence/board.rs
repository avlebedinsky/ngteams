//! Where presence state lives: hashes and sorted sets, in Redis when the server has one (every node reads and writes
//! the same state) or in the node. [`key`] names every key the presence service uses; on Redis, [`Shared`] puts each
//! name in the deployment's keyspace.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Mutex, PoisonError};

use async_trait::async_trait;
use redis::AsyncCommands as _;

use crate::fanout::keyspace;

/// The presence state can't be reached.
#[derive(Debug, thiserror::Error)]
#[error("the presence state is unavailable: {0}")]
pub struct BoardError(String);

impl From<redis::RedisError> for BoardError {
    fn from(error: redis::RedisError) -> Self {
        Self(error.to_string())
    }
}

/// The name of the presence state `kind` of `id`. Every key of the service is named here.
#[must_use]
pub(super) fn key(kind: &str, id: &str) -> String {
    format!("presence:{kind}:{id}")
}

/// The few hash and sorted-set commands the presence service needs.
#[async_trait]
pub(super) trait Board: Send + Sync + std::fmt::Debug {
    /// Sets `field` of the hash `key`, which then lives `ttl` seconds more.
    async fn hset(&self, key: &str, field: &str, value: &str, ttl: u64) -> Result<(), BoardError>;
    /// Removes each `(field, value)` of the hash `key` whose value is still that one; how many it removed.
    async fn hdel_unchanged(&self, key: &str, fields: &[(String, String)]) -> Result<usize, BoardError>;
    async fn hgetall(&self, key: &str) -> Result<Vec<(String, String)>, BoardError>;
    /// Adds `member` to the sorted set `key`, which then lives `ttl` seconds more when one is given.
    async fn zadd(&self, key: &str, member: &str, score: i64, ttl: Option<u64>) -> Result<(), BoardError>;
    /// Whether `member` was there.
    async fn zrem(&self, key: &str, member: &str) -> Result<bool, BoardError>;
    /// At most `limit` members scored `min..=max`, lowest first.
    async fn zrange(&self, key: &str, min: i64, max: i64, limit: usize) -> Result<Vec<String>, BoardError>;
    async fn zscore(&self, key: &str, member: &str) -> Result<Option<i64>, BoardError>;
    /// Removes the members scored up to `max`.
    async fn zprune(&self, key: &str, max: i64) -> Result<(), BoardError>;
    async fn del(&self, key: &str) -> Result<(), BoardError>;
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// What one node keeps, and when each key that has a lifetime lapses (Unix milliseconds), as Redis keeps it.
#[derive(Debug, Default)]
struct Kept {
    hashes: HashMap<String, BTreeMap<String, String>>,
    sets: HashMap<String, BTreeMap<String, i64>>,
    lapses: HashMap<String, i64>,
    due: BTreeSet<(i64, String)>,
}

impl Kept {
    /// Forgets the keys that lapsed by `now`.
    fn forget_lapsed(&mut self, now: i64) {
        while let Some((lapse, key)) = self.due.pop_first() {
            if lapse > now {
                self.due.insert((lapse, key));
                return;
            }
            self.lapses.remove(&key);
            self.hashes.remove(&key);
            self.sets.remove(&key);
        }
    }

    fn lives(&mut self, key: &str, ttl: u64, now: i64) {
        let lapse = now.saturating_add(i64::try_from(ttl.saturating_mul(1000)).unwrap_or(i64::MAX));
        if let Some(earlier) = self.lapses.insert(key.to_owned(), lapse) {
            self.due.remove(&(earlier, key.to_owned()));
        }
        self.due.insert((lapse, key.to_owned()));
    }

    /// Forgets `key` and its lifetime.
    fn remove(&mut self, key: &str) {
        self.hashes.remove(key);
        self.sets.remove(key);
        if let Some(lapse) = self.lapses.remove(key) {
            self.due.remove(&(lapse, key.to_owned()));
        }
    }

    /// Forgets `key` when nothing is left in it.
    fn drop_if_empty(&mut self, key: &str) {
        let empty =
            self.hashes.get(key).is_none_or(BTreeMap::is_empty) && self.sets.get(key).is_none_or(BTreeMap::is_empty);
        if empty {
            self.remove(key);
        }
    }
}

/// One node's state.
#[derive(Debug, Default)]
pub(super) struct Memory(Mutex<Kept>);

impl Memory {
    /// The state, without the keys that lapsed.
    fn kept(&self) -> std::sync::MutexGuard<'_, Kept> {
        let mut kept = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        kept.forget_lapsed(now_ms());
        kept
    }
}

#[async_trait]
impl Board for Memory {
    async fn hset(&self, key: &str, field: &str, value: &str, ttl: u64) -> Result<(), BoardError> {
        let mut kept = self.kept();
        kept.hashes
            .entry(key.to_owned())
            .or_default()
            .insert(field.to_owned(), value.to_owned());
        kept.lives(key, ttl, now_ms());
        Ok(())
    }

    async fn hdel_unchanged(&self, key: &str, fields: &[(String, String)]) -> Result<usize, BoardError> {
        let mut kept = self.kept();
        let Some(hash) = kept.hashes.get_mut(key) else {
            return Ok(0);
        };
        let mut removed = 0;
        for (field, value) in fields {
            if hash.get(field) == Some(value) {
                hash.remove(field);
                removed += 1;
            }
        }
        kept.drop_if_empty(key);
        Ok(removed)
    }

    async fn hgetall(&self, key: &str) -> Result<Vec<(String, String)>, BoardError> {
        let kept = self.kept();
        Ok(kept
            .hashes
            .get(key)
            .map(|hash| {
                hash.iter()
                    .map(|(field, value)| (field.clone(), value.clone()))
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn zadd(&self, key: &str, member: &str, score: i64, ttl: Option<u64>) -> Result<(), BoardError> {
        let mut kept = self.kept();
        kept.sets
            .entry(key.to_owned())
            .or_default()
            .insert(member.to_owned(), score);
        if let Some(ttl) = ttl {
            kept.lives(key, ttl, now_ms());
        }
        Ok(())
    }

    async fn zrem(&self, key: &str, member: &str) -> Result<bool, BoardError> {
        let mut kept = self.kept();
        let Some(set) = kept.sets.get_mut(key) else {
            return Ok(false);
        };
        let removed = set.remove(member).is_some();
        kept.drop_if_empty(key);
        Ok(removed)
    }

    async fn zrange(&self, key: &str, min: i64, max: i64, limit: usize) -> Result<Vec<String>, BoardError> {
        let kept = self.kept();
        let mut members: Vec<(&String, &i64)> = kept
            .sets
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
        Ok(self.kept().sets.get(key).and_then(|set| set.get(member).copied()))
    }

    async fn zprune(&self, key: &str, max: i64) -> Result<(), BoardError> {
        let mut kept = self.kept();
        if let Some(set) = kept.sets.get_mut(key) {
            set.retain(|_, score| *score > max);
            kept.drop_if_empty(key);
        }
        Ok(())
    }

    async fn del(&self, key: &str) -> Result<(), BoardError> {
        self.kept().remove(key);
        Ok(())
    }
}

/// Removes each field of `KEYS[1]` named in `ARGV` whose value, the argument after it, is still that one.
const HDEL_UNCHANGED: &str = "local removed = 0
for i = 1, #ARGV, 2 do
  if redis.call('HGET', KEYS[1], ARGV[i]) == ARGV[i + 1] then
    removed = removed + redis.call('HDEL', KEYS[1], ARGV[i])
  end
end
return removed";

/// The state every node of the deployment shares, under its keyspace in Redis.
#[derive(Debug)]
pub(super) struct Shared(pub keyspace::Shared);

impl Shared {
    /// The connection and the Redis key of the name `key`.
    fn on(&self, key: &str) -> (redis::aio::ConnectionManager, String) {
        (self.0.commands(), self.0.key(key))
    }
}

#[async_trait]
impl Board for Shared {
    async fn hset(&self, key: &str, field: &str, value: &str, ttl: u64) -> Result<(), BoardError> {
        let (mut redis, key) = self.on(key);
        let ttl = i64::try_from(ttl).unwrap_or(i64::MAX);
        let () = redis::pipe()
            .atomic()
            .hset(&key, field, value)
            .ignore()
            .expire(&key, ttl)
            .ignore()
            .query_async(&mut redis)
            .await?;
        Ok(())
    }

    async fn hdel_unchanged(&self, key: &str, fields: &[(String, String)]) -> Result<usize, BoardError> {
        if fields.is_empty() {
            return Ok(0);
        }
        let (mut redis, key) = self.on(key);
        let mut script = redis::cmd("EVAL");
        script.arg(HDEL_UNCHANGED).arg(1).arg(key);
        for (field, value) in fields {
            script.arg(field).arg(value);
        }
        let removed: i64 = script.query_async(&mut redis).await?;
        Ok(usize::try_from(removed).unwrap_or(0))
    }

    async fn hgetall(&self, key: &str) -> Result<Vec<(String, String)>, BoardError> {
        let (mut redis, key) = self.on(key);
        Ok(redis.hgetall(key).await?)
    }

    async fn zadd(&self, key: &str, member: &str, score: i64, ttl: Option<u64>) -> Result<(), BoardError> {
        let (mut redis, key) = self.on(key);
        let Some(ttl) = ttl else {
            return Ok(redis.zadd(key, member, score).await?);
        };
        let ttl = i64::try_from(ttl).unwrap_or(i64::MAX);
        let () = redis::pipe()
            .atomic()
            .zadd(&key, member, score)
            .ignore()
            .expire(&key, ttl)
            .ignore()
            .query_async(&mut redis)
            .await?;
        Ok(())
    }

    async fn zrem(&self, key: &str, member: &str) -> Result<bool, BoardError> {
        let (mut redis, key) = self.on(key);
        let removed: i64 = redis.zrem(key, member).await?;
        Ok(removed > 0)
    }

    async fn zrange(&self, key: &str, min: i64, max: i64, limit: usize) -> Result<Vec<String>, BoardError> {
        let (mut redis, key) = self.on(key);
        let limit = isize::try_from(limit).unwrap_or(isize::MAX);
        Ok(redis.zrangebyscore_limit(key, min, max, 0, limit).await?)
    }

    async fn zscore(&self, key: &str, member: &str) -> Result<Option<i64>, BoardError> {
        let (mut redis, key) = self.on(key);
        Ok(redis.zscore(key, member).await?)
    }

    async fn zprune(&self, key: &str, max: i64) -> Result<(), BoardError> {
        let (mut redis, key) = self.on(key);
        Ok(redis.zrembyscore(key, i64::MIN, max).await?)
    }

    async fn del(&self, key: &str) -> Result<(), BoardError> {
        let (mut redis, key) = self.on(key);
        Ok(redis.del(key).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The boards to try: the node's own, and Redis when the tests have one.
    async fn boards() -> Vec<Box<dyn Board>> {
        let mut boards: Vec<Box<dyn Board>> = vec![Box::new(Memory::default())];
        if let Ok(url) = std::env::var("NGTEAMS_SERVER_REDIS_URL") {
            let redis = redis::Client::open(url).unwrap();
            let keys = keyspace::Keyspace::new(&uuid::Uuid::new_v4().simple().to_string());
            let shared = keyspace::Shared::new(redis.get_connection_manager().await.unwrap(), keys);
            boards.push(Box::new(Shared(shared)));
        }
        boards
    }

    /// Keys nobody else uses.
    fn fresh() -> impl Fn(&str) -> String {
        let run = uuid::Uuid::new_v4().simple().to_string();
        move |name| format!("test:{run}:{name}")
    }

    #[tokio::test]
    async fn a_board_keeps_hashes_and_sorted_sets() {
        for board in boards().await {
            let k = fresh();
            let (h, z) = (k("h"), k("z"));
            board.hset(&h, "a", "1", 60).await.unwrap();
            board.hset(&h, "b", "2", 60).await.unwrap();
            board
                .hdel_unchanged(&h, &[("a".to_owned(), "1".to_owned())])
                .await
                .unwrap();
            assert_eq!(board.hgetall(&h).await.unwrap(), [("b".to_owned(), "2".to_owned())]);
            board
                .hdel_unchanged(&h, &[("b".to_owned(), "2".to_owned())])
                .await
                .unwrap();
            assert!(board.hgetall(&h).await.unwrap().is_empty());

            for (member, score) in [("late", 30), ("early", 10), ("mid", 20)] {
                board.zadd(&z, member, score, None).await.unwrap();
            }
            assert_eq!(board.zrange(&z, 0, 20, 10).await.unwrap(), ["early", "mid"]);
            assert_eq!(board.zrange(&z, 0, 99, 1).await.unwrap(), ["early"]);
            assert_eq!(board.zscore(&z, "mid").await.unwrap(), Some(20));
            assert_eq!(board.zscore(&z, "none").await.unwrap(), None);
            assert!(board.zrem(&z, "mid").await.unwrap());
            assert!(!board.zrem(&z, "mid").await.unwrap());
            assert!(!board.zrem(&k("nothing"), "mid").await.unwrap());
            board.zprune(&z, 10).await.unwrap();
            board.zprune(&k("nothing"), 10).await.unwrap();
            assert_eq!(board.zrange(&z, 0, 99, 10).await.unwrap(), ["late"]);
            board.zprune(&z, 99).await.unwrap();
            board.zadd(&z, "again", 1, None).await.unwrap();
            board.del(&z).await.unwrap();
            assert!(board.zrange(&z, 0, 99, 10).await.unwrap().is_empty());
        }
        assert!(format!("{:?}", Memory::default()).contains("Memory"));
        assert_eq!(key("account", "x"), "presence:account:x");
    }

    #[tokio::test]
    async fn a_key_with_a_lifetime_is_forgotten_when_it_lapses() {
        for board in boards().await {
            let k = fresh();
            board.hset(&k("gone"), "a", "1", 0).await.unwrap();
            board.hset(&k("kept"), "a", "1", 60).await.unwrap();
            board.zadd(&k("gone set"), "m", 1, Some(0)).await.unwrap();
            board.zadd(&k("kept set"), "m", 1, Some(60)).await.unwrap();
            board.zadd(&k("set for good"), "m", 1, None).await.unwrap();
            // a later lifetime replaces the one before
            board.zadd(&k("renewed"), "m", 1, Some(0)).await.unwrap();
            board.zadd(&k("renewed"), "n", 2, Some(60)).await.unwrap();
            assert!(board.hgetall(&k("gone")).await.unwrap().is_empty());
            assert_eq!(board.hgetall(&k("kept")).await.unwrap().len(), 1);
            assert!(board.zrange(&k("gone set"), 0, 9, 9).await.unwrap().is_empty());
            for kept in ["kept set", "set for good"] {
                assert_eq!(board.zrange(&k(kept), 0, 9, 9).await.unwrap(), ["m"], "{kept}");
            }
            assert_eq!(board.zrange(&k("renewed"), 0, 9, 9).await.unwrap(), ["n"]);
            for key in ["kept", "kept set", "set for good", "renewed"] {
                board.del(&k(key)).await.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn the_node_forgets_a_lapsed_key_whole() {
        let board = Memory::default();
        board.hset("h", "a", "1", 0).await.unwrap();
        board.zadd("z", "m", 1, Some(0)).await.unwrap();
        board.zadd("renewed", "m", 1, Some(60)).await.unwrap();
        board.zadd("renewed", "n", 1, Some(0)).await.unwrap();
        board.hset("kept", "a", "1", 60).await.unwrap();
        board
            .hdel_unchanged("kept", &[("a".to_owned(), "1".to_owned())])
            .await
            .unwrap();
        let kept = board.kept();
        assert!(kept.hashes.is_empty() && kept.sets.is_empty());
        assert!(kept.lapses.is_empty() && kept.due.is_empty());
    }

    #[tokio::test]
    async fn a_field_is_removed_only_while_it_holds_the_value_read() {
        for board in boards().await {
            let h = fresh()("h");
            board.hset(&h, "a", "old", 60).await.unwrap();
            board.hset(&h, "b", "old", 60).await.unwrap();
            board.hset(&h, "b", "new", 60).await.unwrap();
            let read = |field: &str| (field.to_owned(), "old".to_owned());
            assert_eq!(
                board
                    .hdel_unchanged(&h, &[read("a"), read("b"), read("c")])
                    .await
                    .unwrap(),
                1
            );
            assert_eq!(board.hgetall(&h).await.unwrap(), [("b".to_owned(), "new".to_owned())]);
            assert_eq!(board.hdel_unchanged(&h, &[]).await.unwrap(), 0);
            assert_eq!(board.hdel_unchanged(&fresh()("none"), &[read("a")]).await.unwrap(), 0);
            board
                .hdel_unchanged(&h, &[("b".to_owned(), "new".to_owned())])
                .await
                .unwrap();
            assert!(board.hgetall(&h).await.unwrap().is_empty());
        }
    }
}
