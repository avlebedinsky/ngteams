//! A deployment's part of a Redis. Deployments may share one Redis, so every key and channel the server writes,
//! reads or subscribes to is named through [`Keyspace::key`], under the deployment's namespace: nothing else builds a
//! Redis name. A node's command connection is handed out only together with its keyspace ([`Shared`]).

use redis::aio::ConnectionManager;

/// What every name of the server starts with in a Redis.
const PREFIX: &str = "ngts";
/// The longest namespace.
const NAMESPACE_MAX: usize = 64;

/// The names of one deployment: `ngts:<namespace>:<name>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keyspace {
    prefix: String,
}

impl Keyspace {
    /// The keyspace of `namespace`: the configured one, or else the deployment's tenant id, which every node of a
    /// deployment reads from its database and no other deployment has.
    #[must_use]
    pub fn new(namespace: &str) -> Self {
        Self {
            prefix: format!("{PREFIX}:{namespace}:"),
        }
    }

    /// The Redis key or channel of `name` (`login:<account>`, `push:<mri>`, `journal:<topic>`).
    #[must_use]
    pub fn key(&self, name: &str) -> String {
        format!("{}{name}", self.prefix)
    }

    /// Whether `namespace` can be configured: a short name of letters, digits, `.`, `_` and `-`.
    #[must_use]
    pub fn is_namespace(namespace: &str) -> bool {
        (1..=NAMESPACE_MAX).contains(&namespace.len())
            && namespace
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    }
}

/// A node's Redis command connection with the keyspace its commands name their keys in.
#[derive(Clone)]
pub struct Shared {
    commands: ConnectionManager,
    keys: Keyspace,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared")
            .field("keys", &self.keys)
            .finish_non_exhaustive()
    }
}

impl Shared {
    #[must_use]
    pub fn new(commands: ConnectionManager, keys: Keyspace) -> Self {
        Self { commands, keys }
    }

    /// The Redis key of `name` in the deployment's keyspace.
    #[must_use]
    pub fn key(&self, name: &str) -> String {
        self.keys.key(name)
    }

    /// The connection, for commands on keys [`Self::key`] named.
    #[must_use]
    pub fn commands(&self) -> ConnectionManager {
        self.commands.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_under_its_namespace() {
        let tenant = Keyspace::new("0f0e");
        assert_eq!(tenant.key("push:8:orgid:a"), "ngts:0f0e:push:8:orgid:a");
        let configured = Keyspace::new("staging");
        assert_eq!(configured.key("login:x"), "ngts:staging:login:x");
        assert_ne!(tenant, configured);
    }

    #[test]
    fn a_namespace_is_a_short_plain_name() {
        for good in ["a", "staging", "eu-1.prod_2", &"n".repeat(64)] {
            assert!(Keyspace::is_namespace(good), "{good}");
        }
        for bad in ["", "a:b", "a b", "a*", "{a}", "é", &"n".repeat(65)] {
            assert!(!Keyspace::is_namespace(bad), "{bad}");
        }
    }
}
