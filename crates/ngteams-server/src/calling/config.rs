//! The calls' configuration: how long a call rings, and the TURN relay the clients' media goes through.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CallingConfig {
    /// Seconds a call rings before it ends unanswered.
    pub ring_timeout: u64,
    pub turn: TurnConfig,
}

/// The TURN relay (coturn) and how its credentials are made: from `secret` (coturn's `use-auth-secret`), else the
/// one long-term user `username` and `password` (coturn's `user=`). With neither, no credentials are handed out.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TurnConfig {
    /// The relay's URLs as a client's ICE agent takes them (`turn:host:3478?transport=udp`, `turns:host:5349`).
    pub urls: Vec<String>,
    /// coturn's `static-auth-secret`.
    pub secret: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    /// coturn's `realm`.
    pub realm: String,
    /// Seconds a credential made from `secret` holds.
    pub ttl: u64,
}

impl std::fmt::Debug for TurnConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnConfig")
            .field("urls", &self.urls)
            .field("realm", &self.realm)
            .field("ttl", &self.ttl)
            .finish_non_exhaustive()
    }
}

impl Default for TurnConfig {
    fn default() -> Self {
        Self {
            urls: Vec::new(),
            secret: None,
            username: None,
            password: None,
            realm: "rtcmedia".to_owned(),
            ttl: 86_400,
        }
    }
}

impl Default for CallingConfig {
    fn default() -> Self {
        Self {
            ring_timeout: 30,
            turn: TurnConfig::default(),
        }
    }
}

impl CallingConfig {
    /// Whether the values can be used: a ring and a credential last some time.
    #[must_use]
    pub fn usable(&self) -> bool {
        self.ring_timeout > 0 && self.turn.ttl > 0 && self.turn.username.is_some() == self.turn.password.is_some()
    }
}
