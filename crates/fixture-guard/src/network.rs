//! A call's network data, which a capture of a call holds and a fixture never may: a public IP address (an ICE
//! candidate's, a relay's, an SDP's `c=` line), an ICE username fragment (an SDP's, a candidate string's `ufrag …`)
//! and password, a DTLS certificate fingerprint, and a TURN credential (a `trap/tokens` answer's `username` and
//! `password`, an ICE server's `credential`).
//!
//! A fixture of a call holds made-up network data instead, which passes:
//! - addresses in the documentation ranges (`192.0.2.0/24`, `198.51.100.0/24`, `203.0.113.0/24`, `2001:db8::/32`,
//!   `3fff::/20`) or in ranges no public host has (private, loopback, link-local, shared, multicast, reserved; for
//!   IPv6 anything outside the global unicast `2000::/3`);
//! - placeholders (`<token:1>`, `<password>`) and values that say they are made up (`SyntheticUfrag0001`);
//! - a fingerprint of one byte repeated (`AA:AA:…:AA`), which no certificate's digest is.
//!
//! What only looks like an address passes too: a Rust path (`teams_proto::chat`, `a::b`), a socket.io frame's
//! `5:::`, a dotted version or section number whose four parts are single digits (`1.2.3.4`, the `OpenID` Connect Core
//! `3.1.3.7`: a real address of that form is a public resolver like `8.8.8.8`, nobody's own), a product version after
//! its name (`Chrome/130.0.0.0`), and a longer dotted run (`1.3.6.1.4.1`).

use std::net::{Ipv4Addr, Ipv6Addr};

use regex::Regex;

/// What a made-up value says about itself, in any case.
const MADE_UP: &str = "synthetic";

/// The shortest TURN username taken as one without a password beside it: the relay's are long opaque strings.
const TURN_USERNAME_MIN: usize = 20;

/// The compiled patterns of [`crate::Kind::IpAddress`], [`crate::Kind::IceCredential`],
/// [`crate::Kind::DtlsFingerprint`] and [`crate::Kind::TurnCredential`].
#[derive(Debug)]
pub struct Network {
    ipv4: Regex,
    /// A run of word characters, colons and dots: an IPv6 address is one whole run.
    token: Regex,
    ice: Regex,
    fingerprint: Regex,
    secret: Regex,
    username: Regex,
    turn_context: Regex,
}

impl Network {
    /// Compiles the patterns.
    ///
    /// # Errors
    /// A pattern fails to compile; that is a bug in this crate, caught by its tests.
    pub fn new() -> Result<Self, regex::Error> {
        Ok(Self {
            ipv4: Regex::new(r"\b(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})\b")?,
            token: Regex::new(r"[0-9A-Za-z_.:]+")?,
            ice: Regex::new(
                r#"(?i)(?:\bice-(?:ufrag|pwd)\s*:|\bufrag\s+|"(?:usernameFragment|ufrag|iceUfrag|icePwd)"\s*:\s*")([A-Za-z0-9+/]{4,})"#,
            )?,
            fingerprint: Regex::new(r"(?:[0-9A-Fa-f]{2}:){19,}[0-9A-Fa-f]{2}")?,
            secret: Regex::new(
                r#"(?i)(?:"(?:password|credential)"\s*:\s*"|\b(?:password|credential)=)([^"<&\s][^"&\s]*)"#,
            )?,
            username: Regex::new(r#"(?i)(?:"username"\s*:\s*"|\busername=)([A-Za-z0-9+/=:._\-]+)"#)?,
            turn_context: Regex::new(r#"(?i)"(?:realm|password|credential)"\s*:|\b(?:password|credential)="#)?,
        })
    }

    /// Whether `line` holds a public IPv4 or IPv6 address (see the module docs for what passes).
    #[must_use]
    pub fn has_public_address(&self, line: &str) -> bool {
        self.has_public_ipv4(line) || self.has_public_ipv6(line)
    }

    fn has_public_ipv4(&self, line: &str) -> bool {
        self.ipv4.captures_iter(line).any(|captures| {
            let Some(whole) = captures.get(0) else {
                return false;
            };
            let octets: Option<Vec<u8>> = (1..=4)
                .map(|group| captures.get(group).and_then(|octet| octet.as_str().parse().ok()))
                .collect();
            let Some(&[a, b, c, d]) = octets.as_deref() else {
                return false;
            };
            let single_digits = whole.as_str().len() == 7;
            !single_digits
                && is_whole_quad(line, whole.start(), whole.end())
                && is_public_ipv4(Ipv4Addr::new(a, b, c, d))
        })
    }

    fn has_public_ipv6(&self, line: &str) -> bool {
        self.token.find_iter(line).any(|token| {
            let token = token.as_str();
            if token.matches(':').count() < 2 {
                return false;
            }
            let trimmed = token.trim_end_matches('.');
            // the run may have taken in a word before the address (`port:2a01:…`, `turn:2a01:…`): what follows the
            // word's `:` is a candidate too, unless the word is a hex group, which makes it the address's first
            let after_word = trimmed
                .split_once(':')
                .filter(|(word, _)| !is_hex_group(word))
                .map(|(_, rest)| rest);
            [Some(trimmed), after_word]
                .into_iter()
                .flatten()
                .flat_map(|run| [Some(run), run.strip_suffix(':').filter(|rest| !rest.ends_with(':'))])
                .flatten()
                .filter_map(|candidate| candidate.parse::<Ipv6Addr>().ok())
                .any(is_public_ipv6)
        })
    }

    /// Whether `line` holds an ICE username fragment or password (`a=ice-ufrag:`, `a=ice-pwd:`, a candidate string's
    /// `ufrag …`, or under a JSON key) that is no placeholder and doesn't say it is made up.
    #[must_use]
    pub fn has_ice_credential(&self, line: &str) -> bool {
        values(&self.ice, line).any(|value| !is_made_up(value))
    }

    /// Whether `line` holds a certificate fingerprint (20 bytes or more, colon-separated hex, as `a=fingerprint:`
    /// writes a SHA-1 or SHA-256 digest) other than one byte repeated.
    #[must_use]
    pub fn has_fingerprint(&self, line: &str) -> bool {
        self.fingerprint.find_iter(line).any(|run| {
            let mut bytes = run.as_str().split(':').map(str::to_ascii_lowercase);
            let first = bytes.next();
            bytes.any(|byte| Some(byte) != first)
        })
    }

    /// Whether `line` holds a TURN credential: a `password` or `credential` value under its JSON key or as a query
    /// parameter, or a `username` value as long as a relay's or on a line with a realm, password or credential key
    /// (a `trap/tokens` answer), none of them a placeholder or a made-up value.
    #[must_use]
    pub fn has_turn_credential(&self, line: &str) -> bool {
        if values(&self.secret, line).any(|value| !is_made_up(value)) {
            return true;
        }
        let beside_secret = self.turn_context.is_match(line);
        values(&self.username, line)
            .any(|value| !is_made_up(value) && (beside_secret || value.len() >= TURN_USERNAME_MIN))
    }
}

/// Whether `word` could be one group of an IPv6 address: empty (the start of a `::`) or up to four hex digits.
fn is_hex_group(word: &str) -> bool {
    word.len() <= 4 && word.chars().all(|char| char.is_ascii_hexdigit())
}

/// Each match's first group in `line`.
fn values<'a>(regex: &'a Regex, line: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    regex
        .captures_iter(line)
        .filter_map(|captures| captures.get(1).map(|value| value.as_str()))
}

/// Whether a value says it is made up.
fn is_made_up(value: &str) -> bool {
    value.to_ascii_lowercase().contains(MADE_UP)
}

/// Whether the dotted quad at `start..end` of `line` stands alone: not part of a longer dotted run (`1.3.6.1.4.1`)
/// and not a product's version after its name (`Chrome/130.0.0.0`; a URL's `//` host is an address).
fn is_whole_quad(line: &str, start: usize, end: usize) -> bool {
    let mut before = line[..start].chars().rev();
    let (previous, earlier) = (before.next(), before.next());
    let longer_before = previous == Some('.');
    let version = previous == Some('/') && earlier.is_some_and(|char| char.is_ascii_alphanumeric());
    let after = &line.as_bytes()[end..];
    let longer_after = after.first() == Some(&b'.') && after.get(1).is_some_and(u8::is_ascii_digit);
    !(longer_before || longer_after || version)
}

/// Whether an IPv4 address is one a public host may have: outside the documentation ranges and every range set aside
/// for private, local, shared, multicast or reserved use.
#[must_use]
pub fn is_public_ipv4(address: Ipv4Addr) -> bool {
    let [a, b, c, _] = address.octets();
    let reserved = match a {
        0 | 10 | 127 | 224..=255 => true,
        100 => (64..128).contains(&b),
        169 => b == 254,
        172 => (16..32).contains(&b),
        192 => (b == 0 && (c == 0 || c == 2)) || b == 168,
        198 => b == 18 || b == 19 || (b == 51 && c == 100),
        203 => b == 0 && c == 113,
        _ => false,
    };
    !reserved
}

/// Whether an IPv6 address is one a public host may have: in the global unicast range (`2000::/3`), outside the
/// documentation ranges (`2001:db8::/32`, `3fff::/20`); an IPv4 address mapped (`::ffff:a.b.c.d`) or translated
/// (`64:ff9b::a.b.c.d`) into IPv6 is judged as that IPv4 address.
#[must_use]
pub fn is_public_ipv6(address: Ipv6Addr) -> bool {
    if let Some(ipv4) = address.to_ipv4_mapped() {
        return is_public_ipv4(ipv4);
    }
    let segments = address.segments();
    if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        let [.., a, b, c, d] = address.octets();
        return is_public_ipv4(Ipv4Addr::new(a, b, c, d));
    }
    let global_unicast = segments[0] & 0xe000 == 0x2000;
    let documentation =
        (segments[0] == 0x2001 && segments[1] == 0x0db8) || (segments[0] == 0x3fff && segments[1] & 0xf000 == 0);
    global_unicast && !documentation
}
