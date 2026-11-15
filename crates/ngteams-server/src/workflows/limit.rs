//! Posts a workflow's webhook takes in a minute: counted in Redis when the server has one (every node of the
//! deployment counts the same posts, in its keyspace), else in the node with governor's GCRA limiter.

use std::num::NonZeroU32;

use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};

use crate::fanout::keyspace::Shared;

/// How many workflows the in-process limiter tracks before it forgets the ones that are idle again.
const TRACKED: usize = 10_000;
/// The window Redis counts posts in.
const WINDOW: u64 = 60;

pub enum Limiter {
    Local(DefaultKeyedRateLimiter<String>),
    Redis { redis: Shared, per_minute: u32 },
}

impl std::fmt::Debug for Limiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Local(_) => "Limiter::Local",
            Self::Redis { .. } => "Limiter::Redis",
        })
    }
}

impl Limiter {
    #[must_use]
    pub fn local(per_minute: u32) -> Self {
        let rate = NonZeroU32::new(per_minute).unwrap_or(NonZeroU32::MIN);
        Self::Local(RateLimiter::keyed(Quota::per_minute(rate)))
    }

    #[must_use]
    pub fn redis(redis: Shared, per_minute: u32) -> Self {
        Self::Redis {
            redis,
            per_minute: per_minute.max(1),
        }
    }

    /// Takes one post of `workflow`'s webhook; `false` when it is over its rate. A Redis failure lets it through
    /// (the webhook's secret still guards it).
    pub async fn take(&self, workflow: &str) -> bool {
        match self {
            Self::Local(limiter) => {
                if limiter.len() > TRACKED {
                    limiter.retain_recent();
                }
                limiter.check_key(&workflow.to_owned()).is_ok()
            }
            Self::Redis { redis, per_minute } => {
                let window = crate::db::now().cast_unsigned() / WINDOW;
                let key = redis.key(&format!("workflow-rate:{workflow}:{window}"));
                let result: redis::RedisResult<(u64,)> = redis::pipe()
                    .atomic()
                    .incr(&key, 1)
                    .expire(&key, (2 * WINDOW).cast_signed())
                    .ignore()
                    .query_async(&mut redis.commands())
                    .await;
                match result {
                    Ok((count,)) => count <= u64::from(*per_minute),
                    Err(error) => {
                        tracing::warn!(%error, "can't count a workflow's webhook posts");
                        true
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_node_counts_each_workflow_on_its_own() {
        let limiter = Limiter::local(2);
        assert!(limiter.take("a").await);
        assert!(limiter.take("a").await);
        assert!(!limiter.take("a").await);
        assert!(limiter.take("b").await);
        assert!(format!("{limiter:?}").contains("Local"));
        assert!(Limiter::local(0).take("c").await);
    }
}
