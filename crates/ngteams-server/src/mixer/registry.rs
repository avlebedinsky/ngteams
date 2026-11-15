//! The live media nodes, in Redis: each node's heartbeat (`media:nodes`, a sorted set of node ids by their last beat's
//! time) and its entry (`media:node:<id>`, what reaches it and what it carries, expiring when the beats stop). A call's
//! path never asks it: the call's row names its node. Only a new room's pick and the sweeper's look at a call's node
//! do.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::fanout::keyspace::Shared;

/// The sorted set of node ids by their last heartbeat (Unix milliseconds).
const NODES: &str = "media:nodes";

/// The key of the node `id`'s entry.
fn node_key(id: &str) -> String {
    format!("media:node:{id}")
}

/// What a node's heartbeat says of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Entry {
    pub id: String,
    pub url: String,
    pub epoch: u64,
    pub rooms: usize,
    pub legs: usize,
    /// It takes no new room (it is stopping).
    pub draining: bool,
}

/// The registry, on the deployment's Redis.
#[derive(Clone, Debug)]
pub(crate) struct Registry {
    shared: Shared,
    /// How long an entry lives without a new beat.
    dead_after: Duration,
}

fn millis_now() -> i64 {
    crate::db::now() * 1000
}

impl Registry {
    pub(crate) fn new(shared: Shared, dead_after: Duration) -> Self {
        Self { shared, dead_after }
    }

    /// The node's heartbeat.
    pub(crate) async fn beat(&self, entry: &Entry) -> redis::RedisResult<()> {
        let body = serde_json::to_string(entry).unwrap_or_default();
        let mut commands = self.shared.commands();
        redis::pipe()
            .cmd("SET")
            .arg(self.shared.key(&node_key(&entry.id)))
            .arg(body)
            .arg("EX")
            .arg(self.dead_after.as_secs().max(1))
            .ignore()
            .zadd(self.shared.key(NODES), &entry.id, millis_now())
            .ignore()
            .query_async(&mut commands)
            .await
    }

    /// The node `id` stopped: it is no longer live.
    pub(crate) async fn remove(&self, id: &str) -> redis::RedisResult<()> {
        let mut commands = self.shared.commands();
        redis::pipe()
            .del(self.shared.key(&node_key(id)))
            .ignore()
            .zrem(self.shared.key(NODES), id)
            .ignore()
            .query_async(&mut commands)
            .await
    }

    /// The node `id`'s entry, while it is live.
    pub(crate) async fn entry(&self, id: &str) -> redis::RedisResult<Option<Entry>> {
        let mut commands = self.shared.commands();
        let body: Option<String> = redis::cmd("GET")
            .arg(self.shared.key(&node_key(id)))
            .query_async(&mut commands)
            .await?;
        Ok(body.and_then(|body| serde_json::from_str(&body).ok()))
    }

    /// Every live node's entry; the ids whose beats stopped leave the set.
    pub(crate) async fn live(&self) -> redis::RedisResult<Vec<Entry>> {
        let mut commands = self.shared.commands();
        let oldest = millis_now() - i64::try_from(self.dead_after.as_millis()).unwrap_or(i64::MAX);
        let (): () = redis::cmd("ZREMRANGEBYSCORE")
            .arg(self.shared.key(NODES))
            .arg("-inf")
            .arg(format!("({oldest}"))
            .query_async(&mut commands)
            .await?;
        let ids: Vec<String> = redis::cmd("ZRANGE")
            .arg(self.shared.key(NODES))
            .arg(0)
            .arg(-1)
            .query_async(&mut commands)
            .await?;
        let mut live = Vec::new();
        for id in ids {
            if let Some(entry) = self.entry(&id).await? {
                live.push(entry);
            }
        }
        Ok(live)
    }
}

/// The node a new room goes to: the live one with the fewest legs that isn't stopping.
pub(crate) fn least_loaded(live: &[Entry]) -> Option<&Entry> {
    live.iter()
        .filter(|entry| !entry.draining)
        .min_by_key(|entry| (entry.legs, entry.rooms))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, legs: usize, draining: bool) -> Entry {
        Entry {
            id: id.into(),
            url: format!("http://{id}"),
            epoch: 1,
            rooms: 1,
            legs,
            draining,
        }
    }

    #[test]
    fn a_new_room_goes_to_the_least_loaded_node_that_isnt_stopping() {
        let live = [entry("a", 9, false), entry("b", 2, true), entry("c", 4, false)];
        assert_eq!(least_loaded(&live).map(|entry| entry.id.as_str()), Some("c"));
        assert!(least_loaded(&[entry("b", 2, true)]).is_none());
        assert!(least_loaded(&[]).is_none());
    }
}
