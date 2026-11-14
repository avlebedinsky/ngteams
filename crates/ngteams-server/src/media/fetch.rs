//! The one way the server reads the outside world for an account: a link preview's page and picture. An account
//! chooses the address, so every address is checked before anything is sent to it (server-side request forgery):
//!
//! - only `http` and `https`, on their own ports, with no user info;
//! - only public addresses: the host's every address is looked up and checked first, and the connection is then
//!   made through a resolver that hands out public addresses alone, so a name can't answer differently in between;
//! - redirects are followed by hand, a few at most, each one checked the same way;
//! - no proxy, a deadline for the whole exchange, a cap on the body, and a cap on fetches running at once.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::LOCATION;
use tokio::sync::Semaphore;
use url::{Host, Url};

use crate::config::MediaConfig;

/// Redirects followed for one fetch.
const MAX_REDIRECTS: usize = 3;
/// Fetches a node runs at once; one more is told to come back.
const MAX_AT_ONCE: usize = 16;
/// The longest address fetched.
const MAX_URL: usize = 2048;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum FetchError {
    /// Not an address a preview may be read from.
    Url,
    /// An address that isn't on the public internet.
    NotPublic,
    Busy,
    TooLarge,
    Failed,
}

pub(super) struct Fetched {
    /// Where the body came from, after redirects.
    pub url: Url,
    pub bytes: Vec<u8>,
}

pub(crate) struct Fetcher {
    client: reqwest::Client,
    allow_private: bool,
    slots: Semaphore,
}

impl std::fmt::Debug for Fetcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fetcher")
            .field("allow_private", &self.allow_private)
            .finish_non_exhaustive()
    }
}

struct PublicOnly {
    allow_private: bool,
}

impl Resolve for PublicOnly {
    fn resolve(&self, name: Name) -> Resolving {
        let allow_private = self.allow_private;
        Box::pin(async move {
            let found = tokio::net::lookup_host((name.as_str(), 0)).await?;
            let addresses: Vec<SocketAddr> = found
                .filter(|address| allow_private || is_public(address.ip()))
                .collect();
            if addresses.is_empty() {
                return Err("the name has no public address".into());
            }
            Ok(Box::new(addresses.into_iter()) as Addrs)
        })
    }
}

impl Fetcher {
    /// # Errors
    /// The HTTP client can't be built.
    pub(crate) fn new(config: &MediaConfig) -> Result<Self, reqwest::Error> {
        let allow_private = config.preview_allow_private_addresses;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .dns_resolver(Arc::new(PublicOnly { allow_private }))
            .timeout(Duration::from_secs(config.preview_timeout))
            .user_agent("ngteams-server link preview")
            .build()?;
        Ok(Self {
            client,
            allow_private,
            slots: Semaphore::new(MAX_AT_ONCE),
        })
    }

    /// The body at `url`, at most `limit` bytes of it: a longer one is cut there (`cut`) or refused.
    pub(super) async fn get(&self, url: &str, limit: u64, cut: bool) -> Result<Fetched, FetchError> {
        if url.len() > MAX_URL {
            return Err(FetchError::Url);
        }
        let mut url = Url::parse(url).map_err(|_| FetchError::Url)?;
        let _slot = self.slots.try_acquire().map_err(|_| FetchError::Busy)?;
        for _ in 0..=MAX_REDIRECTS {
            self.check(&url).await?;
            let mut answer = self
                .client
                .get(url.clone())
                .send()
                .await
                .map_err(|_| FetchError::Failed)?;
            if answer.status().is_redirection() {
                let location = answer
                    .headers()
                    .get(LOCATION)
                    .and_then(|location| location.to_str().ok())
                    .ok_or(FetchError::Failed)?;
                url = url.join(location).map_err(|_| FetchError::Failed)?;
                continue;
            }
            if !answer.status().is_success() {
                return Err(FetchError::Failed);
            }
            let bytes = body(&mut answer, limit, cut).await?;
            return Ok(Fetched { url, bytes });
        }
        Err(FetchError::Failed)
    }

    /// Refuses `url` unless it is a plain web address whose host has public addresses alone.
    async fn check(&self, url: &Url) -> Result<(), FetchError> {
        let plain = matches!(url.scheme(), "http" | "https") && url.username().is_empty() && url.password().is_none();
        if !plain {
            return Err(FetchError::Url);
        }
        if self.allow_private {
            return Ok(());
        }
        if url.port().is_some() {
            return Err(FetchError::NotPublic);
        }
        let public = match url.host().ok_or(FetchError::Url)? {
            Host::Ipv4(address) => is_public(IpAddr::V4(address)),
            Host::Ipv6(address) => is_public(IpAddr::V6(address)),
            Host::Domain(name) => {
                let mut found = tokio::net::lookup_host((name, 0))
                    .await
                    .map_err(|_| FetchError::Failed)?
                    .peekable();
                found.peek().is_some() && found.all(|address| is_public(address.ip()))
            }
        };
        if public { Ok(()) } else { Err(FetchError::NotPublic) }
    }
}

/// At most `limit` bytes of an answer's body: a longer one is cut there (`cut`) or refused.
async fn body(answer: &mut reqwest::Response, limit: u64, cut: bool) -> Result<Vec<u8>, FetchError> {
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let mut bytes = Vec::new();
    while let Some(chunk) = answer.chunk().await.map_err(|_| FetchError::Failed)? {
        let room = limit - bytes.len();
        if chunk.len() <= room {
            bytes.extend_from_slice(&chunk);
        } else if cut {
            bytes.extend_from_slice(&chunk[..room]);
            break;
        } else {
            return Err(FetchError::TooLarge);
        }
    }
    Ok(bytes)
}

/// Whether `address` is on the public internet: not loopback, private, link-local (the cloud metadata address
/// among them), shared, reserved, documentation, multicast or unspecified, in either family, nor an IPv6 form that
/// carries such an IPv4 address.
pub(super) fn is_public(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_public_v4(address),
        IpAddr::V6(address) => is_public_v6(address),
    }
}

fn is_public_v4(address: Ipv4Addr) -> bool {
    let [a, b, c, _] = address.octets();
    !(address.is_unspecified()
        || address.is_loopback()
        || address.is_private()
        || address.is_link_local()
        || address.is_broadcast()
        || address.is_multicast()
        || address.is_documentation()
        || a == 0
        || a >= 240
        || (a == 100 && (64..128).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && (18..20).contains(&b)))
}

fn is_public_v6(address: Ipv6Addr) -> bool {
    let segments = address.segments();
    let embedded = |high: u16, low: u16| {
        let [a, b] = high.to_be_bytes();
        let [c, d] = low.to_be_bytes();
        is_public_v4(Ipv4Addr::new(a, b, c, d))
    };
    match segments {
        // IPv4-mapped and IPv4-compatible
        [0, 0, 0, 0, 0, 0xffff | 0, high, low] => {
            !address.is_unspecified() && !address.is_loopback() && embedded(high, low)
        }
        // NAT64 and 6to4
        [0x64, 0xff9b, 0, 0, 0, 0, high, low] | [0x2002, high, low, ..] => embedded(high, low),
        [first, second, ..] => {
            !(address.is_multicast()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
                || (first == 0x2001 && second == 0x0db8)
                || first == 0x0100)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_public_addresses_pass() {
        for refused in [
            "127.0.0.1",
            "127.8.9.1",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "0.1.2.3",
            "255.255.255.255",
            "224.0.0.1",
            "240.0.0.1",
            "192.0.0.1",
            "192.0.2.1",
            "198.18.0.1",
            "198.51.100.7",
            "203.0.113.9",
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::127.0.0.1",
            "64:ff9b::a00:1",
            "2002:7f00:1::1",
            "2002:c0a8:101::1",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "febf::1",
            "ff02::1",
            "2001:db8::1",
            "100::1",
        ] {
            assert!(!is_public(refused.parse().unwrap()), "{refused}");
        }
        for public in [
            "1.1.1.1",
            "8.8.8.8",
            "172.32.0.1",
            "100.128.0.1",
            "198.20.0.1",
            "2606:4700:4700::1111",
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
            "2002:808:808::1",
        ] {
            assert!(is_public(public.parse().unwrap()), "{public}");
        }
    }
}
