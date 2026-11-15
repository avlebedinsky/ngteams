//! What a run in the QA network saw besides its loopback services: the hosts the processes in it tried to reach, the
//! names they asked DNS for, and the UDP that went straight between two clients instead of through a relay. The QA
//! network (`qa/289` round 1, `pod-net.sh`) is a namespace with loopback only, a dummy default route, nftables sets
//! `inet qa` `outside4`/`outside6`/`direct` that record (and drop) each such attempt, and a DNS listener that logs
//! every name asked (`NGTS_QA_NET_DNS_LOG`) and answers none. Its records are the whole network's: a test reads them
//! within its rig's life, while no other test that uses coturn runs (`crate::support::take_turn`). Only the calls
//! tests run in it: it drops every other loopback UDP, an in-process relay's too.

use std::collections::BTreeMap;
use std::net::UdpSocket;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use serde_json::Value;

pub use crate::support::DNS_LOG;

/// The QA network the test runs in.
pub struct QaNet {
    dns: PathBuf,
}

/// What the network recorded up to a moment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    /// Each attempt to reach a host other than loopback (`address . protocol . port`) and its packets.
    pub outside: BTreeMap<String, u64>,
    /// Each direct UDP flow between two loopback ports, neither a relay's (`port . port`), and its packets.
    pub direct: BTreeMap<String, u64>,
    /// The names asked of DNS, in order.
    pub names: Vec<String>,
}

impl Seen {
    /// What `self` has that `before` didn't: new entries and entries with more packets; names asked since.
    #[must_use]
    pub fn since(&self, before: &Self) -> Self {
        let grew = |now: &BTreeMap<String, u64>, then: &BTreeMap<String, u64>| {
            now.iter()
                .filter(|(key, packets)| then.get(*key).is_none_or(|was| was < *packets))
                .map(|(key, packets)| (key.clone(), *packets))
                .collect()
        };
        Self {
            outside: grew(&self.outside, &before.outside),
            direct: grew(&self.direct, &before.direct),
            names: self.names[before.names.len().min(self.names.len())..].to_vec(),
        }
    }
}

impl QaNet {
    /// The QA network, when the test runs inside it: only loopback and the dummy route's interface, and the DNS log
    /// named. `None` otherwise (and why, printed).
    pub fn here() -> Option<Self> {
        let Some(dns) = std::env::var_os(DNS_LOG).map(PathBuf::from) else {
            eprintln!("network checks skipped: {DNS_LOG} names no QA network");
            return None;
        };
        // the process's own network namespace's interfaces (sysfs may show the host's)
        let interfaces: Vec<String> = std::fs::read_to_string("/proc/self/net/dev")
            .unwrap()
            .lines()
            .skip(2)
            .filter_map(|line| Some(line.split(':').next()?.trim().to_owned()))
            .collect();
        assert!(
            interfaces.iter().all(|name| name == "lo" || name == "dummy0"),
            "{DNS_LOG} is set but the network has {interfaces:?}"
        );
        Some(Self { dns })
    }

    fn set(name: &str) -> BTreeMap<String, u64> {
        let output = Command::new("nft")
            .args(["-j", "list", "set", "inet", "qa", name])
            .output()
            .unwrap();
        assert!(output.status.success(), "nft list set {name}: {output:?}");
        let listing: Value = serde_json::from_slice(&output.stdout).unwrap();
        let mut seen = BTreeMap::new();
        for item in listing["nftables"].as_array().unwrap() {
            for element in item["set"]["elem"].as_array().into_iter().flatten() {
                let value = &element["elem"]["val"];
                let key = value["concat"].as_array().map_or_else(
                    || value.to_string(),
                    |parts| parts.iter().map(Value::to_string).collect::<Vec<_>>().join(" . "),
                );
                let packets = element["elem"]["counter"]["packets"].as_u64().unwrap_or(0);
                seen.insert(key, packets);
            }
        }
        seen
    }

    /// Everything recorded so far.
    pub fn seen(&self) -> Seen {
        let mut outside = Self::set("outside4");
        outside.extend(Self::set("outside6"));
        let names = std::fs::read_to_string(&self.dns)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let asked: Value = serde_json::from_str(line).unwrap();
                asked["name"].as_str().map(str::to_owned)
            })
            .collect();
        Seen {
            outside,
            direct: Self::set("direct"),
            names,
        }
    }

    /// The direct path between two loopback ports that aren't a relay's is blocked: a datagram sent along it never
    /// arrives, and the network records it.
    pub fn direct_path_is_blocked(&self) {
        let before = self.seen();
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        // the drop refuses the send itself (EPERM), or lets it go nowhere
        let _ = sender.send_to(b"direct", receiver.local_addr().unwrap());
        let mut buffer = [0_u8; 16];
        assert!(receiver.recv_from(&mut buffer).is_err(), "a direct datagram arrived");
        let flow = format!(
            "{} . {}",
            sender.local_addr().unwrap().port(),
            receiver.local_addr().unwrap().port()
        );
        let recorded = self.seen().since(&before);
        assert!(recorded.direct.contains_key(&flow), "{flow} not recorded: {recorded:?}");
    }
}
