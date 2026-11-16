//! A node's Redis command connection, which never holds a request long while Redis is down or doesn't answer. Every
//! caller goes on without Redis (the rate limits let a request through, the fan-out leaves the rest to sync), so a
//! command is given up after the configured timeout, and once one fails for want of Redis the commands after it fail
//! at once for as long: a request with many commands (a message's fan-out to each member) waits on Redis once, not
//! once per command while the connection manager reconnects.

use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use redis::aio::{ConnectionLike, ConnectionManager};
use redis::{Cmd, ErrorKind, Pipeline, RedisError, RedisFuture, RedisResult, Value};

const RESTING: &str = "Redis failed just now";

/// The connection manager behind the timeout, and the moment until which commands fail without trying Redis.
#[derive(Clone, Debug)]
pub struct Commands {
    manager: ConnectionManager,
    timeout: Duration,
    resting_until: Arc<Mutex<Option<Instant>>>,
}

impl Commands {
    #[must_use]
    pub fn new(manager: ConnectionManager, timeout: Duration) -> Self {
        Self {
            manager,
            timeout,
            resting_until: Arc::new(Mutex::new(None)),
        }
    }

    fn resting_until(&self) -> std::sync::MutexGuard<'_, Option<Instant>> {
        self.resting_until.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn bounded<T>(&self, command: impl Future<Output = RedisResult<T>>) -> RedisResult<T> {
        if self.resting_until().is_some_and(|until| Instant::now() < until) {
            return Err(RedisError::from((ErrorKind::Io, RESTING)));
        }
        let result = within(self.timeout, command).await;
        if let Err(error) = &result
            && (error.is_io_error() || error.is_unrecoverable_error())
        {
            *self.resting_until() = Some(Instant::now() + self.timeout);
        }
        result
    }
}

/// `command`'s result, or a timeout error once it has taken `timeout`.
pub(crate) async fn within<T>(timeout: Duration, command: impl Future<Output = RedisResult<T>>) -> RedisResult<T> {
    tokio::time::timeout(timeout, command)
        .await
        .unwrap_or_else(|_| Err(std::io::Error::from(std::io::ErrorKind::TimedOut).into()))
}

impl ConnectionLike for Commands {
    fn req_packed_command<'a>(&'a mut self, cmd: &'a Cmd) -> RedisFuture<'a, Value> {
        Box::pin(async move {
            let mut manager = self.manager.clone();
            self.bounded(async move { manager.send_packed_command(cmd).await })
                .await
        })
    }

    fn req_packed_commands<'a>(
        &'a mut self,
        pipeline: &'a Pipeline,
        offset: usize,
        count: usize,
    ) -> RedisFuture<'a, Vec<Value>> {
        Box::pin(async move {
            let mut manager = self.manager.clone();
            self.bounded(async move { manager.send_packed_commands(pipeline, offset, count).await })
                .await
        })
    }

    fn get_db(&self) -> i64 {
        self.manager.get_db()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn after_a_failure_the_next_command_fails_without_trying_redis() {
        let closed = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let client = redis::Client::open(format!("redis://127.0.0.1:{closed}")).unwrap();
        let config = redis::aio::ConnectionManagerConfig::new().set_number_of_retries(0);
        let manager = ConnectionManager::new_lazy_with_config(client, config).unwrap();
        let mut commands = Commands::new(manager, Duration::from_secs(5));
        assert_eq!(commands.get_db(), 0);
        let ping = || redis::cmd("PING");
        let refused = ping().query_async::<()>(&mut commands).await.unwrap_err();
        assert!(
            refused.is_io_error() && !refused.to_string().contains(RESTING),
            "{refused}"
        );
        let resting = ping().query_async::<()>(&mut commands).await.unwrap_err();
        assert!(resting.to_string().contains(RESTING), "{resting}");
        assert!(format!("{commands:?}").contains("Commands"));
    }
}
