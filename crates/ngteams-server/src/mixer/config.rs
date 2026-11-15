//! The media node's configuration (`[mixer]`): where it listens, the addresses clients' relays reach it at, and its
//! limits and timeouts.

use std::net::{IpAddr, SocketAddr};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct MixerConfig {
    /// The node's id in the registry; a new one each start when unset.
    pub node_id: Option<String>,
    /// The internal API's listener (signaling nodes → this node): a private network's address, never public.
    pub internal_listen: SocketAddr,
    /// The internal API's URL as the other nodes reach it; the listener's own address when unset.
    pub internal_url: Option<url::Url>,
    /// The one UDP socket every call's media goes through.
    pub udp_listen: SocketAddr,
    /// The addresses a client's TURN relay reaches the UDP socket at (its host candidates, on the socket's port);
    /// the socket's own address, or else the host's outward one, when unset.
    pub public_addrs: Vec<IpAddr>,
    /// ICE-lite: the node only answers connectivity checks (it has a reachable address).
    pub ice_lite: bool,
    pub max_rooms: usize,
    pub max_legs_per_room: usize,
    /// Seconds between the node's heartbeats in the registry.
    pub heartbeat: u64,
    /// Seconds without a heartbeat after which a node is dead and its calls end.
    pub dead_after: u64,
    /// Seconds a stopping node keeps serving its rooms before it ends them.
    pub drain_timeout: u64,
    /// Seconds between the node's looks at its rooms' calls (a room whose call is over goes).
    pub reconcile: u64,
    /// Seconds a leg's connection may stay down before its party is taken out of the call.
    pub media_timeout: u64,
    /// Seconds a leg has to connect after its answer before its party is taken out of the call.
    pub connect_timeout: u64,
}

impl Default for MixerConfig {
    fn default() -> Self {
        Self {
            node_id: None,
            internal_listen: SocketAddr::from(([127, 0, 0, 1], 0)),
            internal_url: None,
            udp_listen: SocketAddr::from(([0, 0, 0, 0], 3480)),
            public_addrs: Vec::new(),
            ice_lite: true,
            max_rooms: 1000,
            max_legs_per_room: 50,
            heartbeat: 5,
            dead_after: 15,
            drain_timeout: 300,
            reconcile: 30,
            media_timeout: 20,
            connect_timeout: 30,
        }
    }
}

impl MixerConfig {
    /// Whether every limit and time is above zero, and a node is dead only after it missed a heartbeat.
    #[must_use]
    pub fn usable(&self) -> bool {
        let times = [
            self.heartbeat,
            self.dead_after,
            self.drain_timeout,
            self.reconcile,
            self.media_timeout,
            self.connect_timeout,
        ];
        self.max_rooms > 0
            && self.max_legs_per_room > 0
            && times.iter().all(|seconds| *seconds > 0)
            && self.dead_after > self.heartbeat
            && self
                .internal_url
                .as_ref()
                .is_none_or(|url| matches!(url.scheme(), "http" | "https"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_node_is_dead_only_after_a_missed_heartbeat() {
        assert!(MixerConfig::default().usable());
        let config = MixerConfig {
            dead_after: 5,
            ..MixerConfig::default()
        };
        assert!(!config.usable());
        let config = MixerConfig {
            max_legs_per_room: 0,
            ..MixerConfig::default()
        };
        assert!(!config.usable());
        let config = MixerConfig {
            internal_url: Some(url::Url::parse("ftp://node").unwrap()),
            ..MixerConfig::default()
        };
        assert!(!config.usable());
    }
}
