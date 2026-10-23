//! The reviewed **expected-deny** list of the writing scenarios: the contacting-service requests and the WebSocket
//! frames of the web client that the fence denies and a scenario doesn't need. Under the lab rule (CLAUDE.md) only
//! what could reach people is ever denied, so the list is short: each class says why the scenario doesn't need it.
//!
//! The list changes only how a run is judged, never a verdict: the fence never reads it, so a request on it is still
//! denied, kept and logged to `fence.log` like any other. A deny is expected ([`Review::expects`]) only when it is on
//! the list *and*:
//! - its cause is [`DenyCause::Unclassified`]: a partial or refused membership read, a request whose `fence.log` entry
//!   failed, a malformed pause or a frame sent through another realm stays unexpected whatever its URL;
//! - its URL doesn't name a mapped target of the scenario (the a–b chat in `ab-chat`, say): nothing denied on the chat
//!   the scenario writes to is ever expected.
//!
//! The one exception to both is a forced deny ([`DenyCause::Forced`]) whose rule's label the scenario names
//! ([`Review::expecting_forced`]): the enforcement probes, an earlier run's probe sent again on load, `abc-chat`'s
//! first messages, `ab-image`'s and `ab-file`'s media and file writes, and what a scenario holds back on purpose
//! (`ab-trouter-quiet`'s and the `own-presence` runs'). Any other forced deny is unexpected.
//!
//! What isn't here keeps failing the run until it is added with its reason: writes on a conversation other than the
//! scenario's, and every request on a call (`echo-call` needs those: `docs/backlog/fence-call-follow-ups.md`, so they
//! are no class of the list and the run fails on them).

use url::Url;

use crate::fence_http::{Denial, DenyCause};
use crate::fence_ws::FrameDenial;
use regex::Regex;

/// The hosts an entry covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hosts {
    /// Exactly these hosts.
    Exact(&'static [&'static str]),
    /// Any subdomain of this domain, not the domain itself.
    Under(&'static str),
    /// A host exactly one label under this domain (a tenant's own host: `<tenant>-my.sharepoint.com`), neither the
    /// domain itself nor a host deeper under it.
    OneLabelUnder(&'static str),
}

impl Hosts {
    /// Whether `host` is one of these.
    #[must_use]
    pub fn covers(self, host: &str) -> bool {
        match self {
            Self::Exact(hosts) => hosts.iter().any(|exact| host.eq_ignore_ascii_case(exact)),
            Self::Under(domain) => subdomain(host, domain).is_some(),
            Self::OneLabelUnder(domain) => subdomain(host, domain).is_some_and(|label| !label.contains('.')),
        }
    }
}

/// What `host` holds before `.<domain>` (in any case), when that is a non-empty name.
fn subdomain<'a>(host: &'a str, domain: &str) -> Option<&'a str> {
    host.len()
        .checked_sub(domain.len() + 1)
        .filter(|&dot| dot > 0)
        .filter(|&dot| {
            host.as_bytes()[dot] == b'.'
                && host.is_char_boundary(dot + 1)
                && host[dot + 1..].eq_ignore_ascii_case(domain)
        })
        .map(|dot| &host[..dot])
}

/// One class of denied requests on the list: `https` to one of `hosts` on the default port, with `method`, on `path`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExpectedRequest {
    /// What the requests are, for a reader.
    pub class: &'static str,
    pub hosts: Hosts,
    /// The method, in upper case.
    pub method: &'static str,
    /// The URL's path, one `/`-separated segment at a time; a `*` segment stands for exactly one non-empty segment (a
    /// region, a partition, an id), and a segment `*<suffix>` for one that ends with the suffix once percent-decoded,
    /// in any case (`*@thread.tacv2`: a channel's id).
    pub path: &'static str,
    /// Why the scenario doesn't need it.
    pub reason: &'static str,
}

impl ExpectedRequest {
    /// Whether `denial`'s request is of this class (whatever the deny's cause).
    #[must_use]
    pub fn covers(&self, denial: &Denial) -> bool {
        let Ok(url) = Url::parse(&denial.url) else {
            return false;
        };
        url.scheme() == "https"
            && url.port().is_none()
            && url.host_str().is_some_and(|host| self.hosts.covers(host))
            && denial.method.eq_ignore_ascii_case(self.method)
            && path_matches(self.path, url.path())
    }
}

/// One class of denied WebSocket frames on the list: every frame on a `wss` socket to one of `hosts` on the default
/// port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExpectedFrames {
    /// What the sockets are, for a reader.
    pub class: &'static str,
    pub hosts: Hosts,
    /// Why the scenario doesn't need them.
    pub reason: &'static str,
}

impl ExpectedFrames {
    /// Whether `denial`'s socket is of this class (whatever the deny's cause).
    #[must_use]
    pub fn covers(&self, denial: &FrameDenial) -> bool {
        Url::parse(&denial.url).is_ok_and(|url| {
            url.scheme() == "wss" && url.port().is_none() && url.host_str().is_some_and(|host| self.hosts.covers(host))
        })
    }
}

/// `path` segment by segment against `pattern` ([`ExpectedRequest::path`]).
fn path_matches(pattern: &str, path: &str) -> bool {
    let mut wanted = pattern.split('/');
    let mut segments = path.split('/');
    loop {
        match (wanted.next(), segments.next()) {
            (None, None) => return true,
            (Some(want), Some(segment)) if segment_matches(want, segment) => {}
            _ => return false,
        }
    }
}

/// One path segment against one segment of a pattern: `*`, `*<suffix>` or the segment exactly.
fn segment_matches(want: &str, segment: &str) -> bool {
    match want.strip_prefix('*') {
        Some("") => !segment.is_empty(),
        Some(suffix) => percent_encoding::percent_decode_str(segment)
            .decode_utf8()
            .is_ok_and(|decoded| {
                decoded.len() > suffix.len() && decoded.to_lowercase().ends_with(&suffix.to_lowercase())
            }),
        None => want == segment,
    }
}

/// The web client's own origins.
pub const TEAMS: Hosts = Hosts::Exact(&["teams.microsoft.com", "teams.cloud.microsoft"]);

/// The denied requests a writing scenario expects (see the module docs).
pub const REQUESTS: &[ExpectedRequest] = &[
    ExpectedRequest {
        class: "bot invokes",
        hosts: TEAMS,
        method: "POST",
        path: "/api/chatsvc/*/v1/agents/*/invoke",
        reason: "a bot invoke reaches the bot's own service with the account's token, which the fence never allows (no \
                 token goes to a third party): the ones seen are the compose box's message extensions fetching their \
                 commands (`composeExtension/fetchCommands`) when an editor opens, and the scenario uses no message \
                 extension",
    },
    ExpectedRequest {
        class: "own channel entry",
        hosts: TEAMS,
        method: "PUT",
        path: "/api/chatsvc/*/v1/threads/*@thread.tacv2/members/*",
        reason: "the account's own member entry in a team channel (`?returnThreadMetadata`), sent as the client lists \
                 the account's teams: a channel's members can't be read in full, so a write there stays denied, and \
                 the scenario uses no channel",
    },
    ExpectedRequest {
        class: "channel properties",
        hosts: TEAMS,
        method: "PUT",
        path: "/api/chatsvc/*/v1/users/ME/conversations/*@thread.tacv2/properties",
        reason: "a property of a team channel the client sets as it lists the account's teams, beyond the account's \
                 own view of it (which passes): the scenario uses no channel",
    },
    ExpectedRequest {
        class: "file browser prewarm",
        hosts: Hosts::OneLabelUnder("sharepoint.com"),
        method: "POST",
        path: "/personal/*/_layouts/15/filebrowser.aspx",
        reason: "the OneDrive file browser's frame, which the client prewarms (`preWarmFB`) as a document loaded by a \
                 form `POST` of the account's own SharePoint token to its personal site: it reaches nobody, but on \
                 SharePoint every write is contacting unless a row carves it out, and none does; the scenario \
                 sends no file",
    },
];

/// The denied WebSocket frames a writing scenario expects (see the module docs).
pub const FRAMES: &[ExpectedFrames] = &[ExpectedFrames {
    class: "Trouter delivery responses",
    hosts: Hosts::Under("trouter.teams.microsoft.com"),
    reason: "the answer to a delivery of the push channel (a socket.io message), which goes back to whoever sent the \
             delivery: no delivery is answered, an incoming call's among them; the service delivers again what went \
             unanswered, and the scenario reads its chats from the chat service",
}];

/// The list's class of `denial`'s request, whatever the deny's cause.
#[must_use]
pub fn request_class(denial: &Denial) -> Option<&'static ExpectedRequest> {
    REQUESTS.iter().find(|entry| entry.covers(denial))
}

/// The list's class of `denial`'s socket, whatever the deny's cause.
#[must_use]
pub fn frame_class(denial: &FrameDenial) -> Option<&'static ExpectedFrames> {
    FRAMES.iter().find(|entry| entry.covers(denial))
}

/// How a writing scenario judges its denies: the list, the forced denies it names, and its mapped targets (see the
/// module docs).
#[derive(Clone, Debug, Default)]
pub struct Review {
    /// The labels of the forced denies the scenario expects.
    forced: &'static [&'static str],
    /// Each finds a mapped target of the scenario in a URL.
    mapped: Vec<Regex>,
}

impl Review {
    /// The list alone: no forced deny is expected, and no target is mapped.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            forced: &[],
            mapped: Vec::new(),
        }
    }

    /// Expects the forced denies whose rule has one of these labels ([`crate::fence_http::ForcedDeny::labelled`],
    /// [`crate::fence_ws::ForcedFrameDeny::labelled`]).
    #[must_use]
    pub const fn expecting_forced(mut self, labels: &'static [&'static str]) -> Self {
        self.forced = labels;
        self
    }

    /// Makes every deny whose URL `names` finds a match in unexpected, the list notwithstanding.
    #[must_use]
    pub fn with_mapped_target(mut self, names: Regex) -> Self {
        self.mapped.push(names);
        self
    }

    /// Whether `denial` is expected (see the module docs).
    #[must_use]
    pub fn expects(&self, denial: &Denial) -> bool {
        if denial.cause == DenyCause::Forced {
            return self.expects_label(denial.label.as_deref());
        }
        denial.cause == DenyCause::Unclassified
            && !self.mapped.iter().any(|names| names.is_match(&denial.url))
            && request_class(denial).is_some()
    }

    /// Whether `denial` is expected: a frame on a listed socket, denied as [`DenyCause::Unclassified`], or a forced deny
    /// whose rule's label the scenario names.
    #[must_use]
    pub fn expects_frame(&self, denial: &FrameDenial) -> bool {
        if denial.cause == DenyCause::Forced {
            return self.expects_label(denial.label.as_deref());
        }
        denial.cause == DenyCause::Unclassified && frame_class(denial).is_some()
    }

    /// Whether a forced deny labelled `label` is one the scenario expects.
    fn expects_label(&self, label: Option<&str>) -> bool {
        label.is_some_and(|label| self.forced.contains(&label))
    }
}
