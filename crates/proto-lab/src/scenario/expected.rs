//! The reviewed **expected-deny** list of the writing scenarios (`ab-chat`, `ab-chat-full`, `echo-call`): the requests
//! and WebSocket frames of the web client that the fence denies and a scenario doesn't need.
//!
//! The list changes only how a run is judged, never a verdict: the fence never reads it, so a request on it is still
//! denied, kept and logged to `fence.log` like any other. A deny is expected ([`Review::expects`]) only when it is on
//! the list, or in one of the scenario's own classes (below), *and*:
//! - its cause is [`DenyCause::Unclassified`]: a partial or refused membership read, a request whose `fence.log` entry
//!   failed (own-state or listed), a malformed pause or a frame sent through another realm stays unexpected whatever
//!   its URL;
//! - its URL doesn't name a mapped target of the scenario (the a–b chat in `ab-chat` and `ab-chat-full`): nothing
//!   denied on the chat the scenario writes to is ever expected.
//!
//! The one exception to both is a forced deny ([`DenyCause::Forced`]) whose rule's label the scenario names
//! ([`Review::expecting_forced`]): the enforcement probes of `ab-chat`, `ab-chat-full`, `ab-image`, `ab-trouter` and
//! `abc-chat`, an earlier run's probe sent again on load (in `echo-call` too), `abc-chat`'s first messages, and
//! `ab-image`'s media writes. Any other forced deny is unexpected.
//!
//! A scenario may also name classes of its own besides the list ([`Review::expecting_requests`]), expected under the
//! same two conditions: `ab-trouter`'s are what comes up only with the push channel
//! (`super::ab_trouter::TROUTER_RUN_REQUESTS`), the calling stack's endpoint registration among them. Of the calling
//! stack's own-state (below), only that registration is expected, and only in an `ab-trouter` run.
//!
//! The classes are the ones the M0 acceptance runs of `ab-chat` showed on 2026-10-20 (both accounts) and that its
//! `load`, `ab-chat`, `ab-chat-full` and `echo-call` captures since repeat, plus two first seen in `ab-chat-full`'s
//! runs of 2026-10-20: the bot invokes an opening editor sends and the own activity feed's item read
//! (`48:notifications`, `isread`), and one first seen on 2026-10-21, once `ab-chat-full` mentioned b and in
//! `ab-image`'s run: the same read on the account's own mentions stream (`48:mentions`). Each entry says why the
//! scenario doesn't need it. A class that isn't here keeps failing the run until it is reviewed: writes on
//! conversations other than the scenario's (a tenant team's thread, say), the calling stack's
//! own-state and every request on a call (`echo-call` needs those: `docs/backlog/fence-call-follow-ups.md`).

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
}

impl Hosts {
    /// Whether `host` is one of these.
    #[must_use]
    pub fn covers(self, host: &str) -> bool {
        match self {
            Self::Exact(hosts) => hosts.iter().any(|exact| host.eq_ignore_ascii_case(exact)),
            Self::Under(domain) => host
                .len()
                .checked_sub(domain.len() + 1)
                .filter(|&dot| dot > 0)
                .is_some_and(|dot| {
                    host.as_bytes()[dot] == b'.'
                        && host.is_char_boundary(dot + 1)
                        && host[dot + 1..].eq_ignore_ascii_case(domain)
                }),
        }
    }
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
    /// region, a partition, an id).
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
            (Some("*"), Some(segment)) if !segment.is_empty() => {}
            (Some(want), Some(segment)) if want != "*" && want == segment => {}
            _ => return false,
        }
    }
}

/// The web client's own origins.
pub const TEAMS: Hosts = Hosts::Exact(&["teams.microsoft.com", "teams.cloud.microsoft"]);

const MIDDLE_TIER: &str = "a middle-tier lookup the client posts (profiles, settings, policies, entitlements): no \
                           state changes, and the scenario reads the chat from the chat service, not from these";
const APPS: &str = "the client's app catalogue and entitlements: the scenario uses no app";
const PROFILES_OUTSIDE: &str = "a profile lookup the read-by-POST list doesn't let through (`crate::post_read`): it \
                                names someone who isn't a lab account (a tenant member the client shows, an approved \
                                service) or isn't in the captured shape; the scenario needs only the lab accounts' \
                                profiles, which pass";
const TOKEN_EXCHANGE: &str = "a token exchange of the asynchronous media gateway or AMS (images, files): the scenario \
                              sends and reads plain text only";

/// The denied requests a writing scenario expects (see the module docs).
pub const REQUESTS: &[ExpectedRequest] = &[
    ExpectedRequest {
        class: "telemetry",
        hosts: Hosts::Under("events.data.microsoft.com"),
        method: "POST",
        path: "/OneCollector/1.0/",
        reason: "the client's telemetry: nothing the scenario does reads it back",
    },
    ExpectedRequest {
        class: "Aria logging",
        hosts: Hosts::Exact(&["browser.pipe.aria.microsoft.com"]),
        method: "POST",
        path: "/Collector/3.0/",
        reason: "the client's logging: nothing the scenario does reads it back",
    },
    ExpectedRequest {
        class: "admin center client log",
        hosts: Hosts::Exact(&["admin.microsoft.com"]),
        method: "POST",
        path: "/api/instrument/logclient",
        reason: "the client's logging: nothing the scenario does reads it back",
    },
    ExpectedRequest {
        class: "own chat properties",
        hosts: TEAMS,
        method: "PUT",
        path: "/api/chatsvc/*/v1/users/ME/properties",
        reason: "the account's own settings on the chat service, not a chat: the client keeps working from what it \
                 read, and the scenario's chat doesn't depend on them",
    },
    ExpectedRequest {
        class: "bot invokes",
        hosts: TEAMS,
        method: "POST",
        path: "/api/chatsvc/*/v1/agents/*/invoke",
        reason: "a bot invoke reaches the bot's own service, which the fence never allows: the ones seen \
                 (ab-chat-full, 2026-10-20) are the compose box's message extensions fetching their commands \
                 (`composeExtension/fetchCommands`) when an editor opens, and the scenario uses no message extension",
    },
    ExpectedRequest {
        class: "own activity feed item read",
        hosts: TEAMS,
        method: "PUT",
        path: "/api/chatsvc/*/v1/users/ME/conversations/48%3Anotifications/messages/*/properties",
        reason: "the account marking an item of its own activity feed read (`?name=isread`, `{\"isread\":\"true\"}`), \
                 seen once b's reaction reached a's feed (ab-chat-full, 2026-10-20): it changes only what the \
                 account's own feed shows, and the scenario reads nothing from the feed",
    },
    ExpectedRequest {
        class: "own mention read",
        hosts: TEAMS,
        method: "PUT",
        path: "/api/chatsvc/*/v1/users/ME/conversations/48%3Amentions/messages/*/properties",
        reason: "the account marking an item of its own mentions stream read (`?name=isread`, `{\"isread\":\"true\"}`), \
                 seen once a's mention reached b (ab-chat-full, 2026-10-21) and from b in ab-image (2026-10-21, six \
                 retries of one item): like the activity feed's, it changes only what the account's own mentions show, \
                 and the scenario reads the mention from the chat, not from that stream",
    },
    ExpectedRequest {
        class: "chat list by POST (CSA)",
        hosts: TEAMS,
        method: "POST",
        path: "/api/csa/*/api/v1/teams/users/me/chatsAndChannelsByIds",
        reason: "the client's chat list, read by POST: the scenario opens its chat by deep link, and the chat's own \
                 read is a GET",
    },
    ExpectedRequest {
        class: "short profiles beyond the lab",
        hosts: TEAMS,
        method: "POST",
        path: "/api/mt/part/*/beta/users/fetchShortProfile",
        reason: PROFILES_OUTSIDE,
    },
    ExpectedRequest {
        class: "user fetch beyond the lab",
        hosts: TEAMS,
        method: "POST",
        path: "/api/mt/part/*/beta/users/fetch",
        reason: PROFILES_OUTSIDE,
    },
    ExpectedRequest {
        class: "feature settings",
        hosts: TEAMS,
        method: "POST",
        path: "/api/mt/part/*/beta/users/fetchfeaturesettings",
        reason: MIDDLE_TIER,
    },
    ExpectedRequest {
        class: "aggregate settings",
        hosts: TEAMS,
        method: "POST",
        path: "/api/mt/part/*/beta/users/useraggregatesettings",
        reason: MIDDLE_TIER,
    },
    ExpectedRequest {
        class: "effective policies",
        hosts: TEAMS,
        method: "POST",
        path: "/api/mt/part/*/beta/users/effectivePolicies",
        reason: MIDDLE_TIER,
    },
    ExpectedRequest {
        class: "cookie exchange",
        hosts: TEAMS,
        method: "POST",
        path: "/api/mt/part/*/beta/users/*/cookiev2",
        reason: "a cookie for the client's embedded web content: the scenario opens none",
    },
    ExpectedRequest {
        class: "files availability",
        hosts: TEAMS,
        method: "POST",
        path: "/api/mt/part/*/beta/me/files/availability",
        reason: "whether the account's file storage is ready: no scenario sends a file (`ab-file` meets it and stops: \
                 the client then says the account needs OneDrive, `docs/backlog/file-sending.md`)",
    },
    ExpectedRequest {
        class: "artifact folder",
        hosts: TEAMS,
        method: "POST",
        path: "/api/mt/part/*/v1/mcps/initArtifactFolder",
        reason: "sets up a folder in the account's own storage for shared artifacts: the scenario shares none",
    },
    ExpectedRequest {
        class: "share-to-Teams app install",
        hosts: TEAMS,
        method: "POST",
        path: "/api/mt/part/*/v1/sharetoteams/installApp",
        reason: "installs the share-to-Teams app for the account: the scenario uses no app",
    },
    ExpectedRequest {
        class: "engagement surfaces",
        hosts: TEAMS,
        method: "PUT",
        path: "/api/mt/part/*/beta/me/engagementSurfaces/actions",
        reason: "records which of the client's own prompts the account has seen: the scenario needs no prompt",
    },
    ExpectedRequest {
        class: "user app entitlements (middle tier)",
        hosts: TEAMS,
        method: "POST",
        path: "/api/mt/part/*/beta/users/apps/aggregatedEntitlements",
        reason: APPS,
    },
    ExpectedRequest {
        class: "app definitions (middle tier)",
        hosts: TEAMS,
        method: "POST",
        path: "/api/mt/part/*/beta/users/apps/batchedDefinitions",
        reason: APPS,
    },
    ExpectedRequest {
        class: "app eligibilities (middle tier)",
        hosts: TEAMS,
        method: "POST",
        path: "/api/mt/part/*/beta/users/apps/eligibilities",
        reason: APPS,
    },
    ExpectedRequest {
        class: "team app entitlements (middle tier)",
        hosts: TEAMS,
        method: "POST",
        path: "/api/mt/part/*/beta/teams/apps/aggregatedEntitlements",
        reason: APPS,
    },
    ExpectedRequest {
        class: "user app entitlements",
        hosts: TEAMS,
        method: "POST",
        path: "/api/apps/part/*/beta/users/apps/aggregatedEntitlements",
        reason: APPS,
    },
    ExpectedRequest {
        class: "app definitions",
        hosts: TEAMS,
        method: "POST",
        path: "/api/apps/part/*/beta/users/apps/batchedDefinitions",
        reason: APPS,
    },
    ExpectedRequest {
        class: "app eligibilities",
        hosts: TEAMS,
        method: "POST",
        path: "/api/apps/part/*/beta/users/apps/eligibilities",
        reason: APPS,
    },
    ExpectedRequest {
        class: "team app entitlements",
        hosts: TEAMS,
        method: "POST",
        path: "/api/apps/part/*/beta/teams/apps/aggregatedEntitlements",
        reason: APPS,
    },
    ExpectedRequest {
        class: "presence reads",
        hosts: TEAMS,
        method: "POST",
        path: "/ups/*/v1/presence/getpresence/",
        reason: "a presence read the read-by-POST list doesn't let through (`crate::post_read`): it names someone who \
                 isn't a lab account, carries the client's endpoint id or isn't in the captured shape; the scenario \
                 needs no presence, and the lab accounts' reads pass",
    },
    ExpectedRequest {
        class: "push updates metadata",
        hosts: TEAMS,
        method: "POST",
        path: "/api/tps/api/v1/teams/users/me/metadata/pushupdates",
        reason: "the client's team metadata updates: the scenario's chat is a 1:1 chat, not a team",
    },
    ExpectedRequest {
        class: "async gateway AAD token exchange",
        hosts: Hosts::Under("asyncgw.teams.microsoft.com"),
        method: "POST",
        path: "/v1/*/aadtokenauth",
        reason: TOKEN_EXCHANGE,
    },
    ExpectedRequest {
        class: "async gateway skypetoken exchange",
        hosts: Hosts::Under("asyncgw.teams.microsoft.com"),
        method: "POST",
        path: "/v1/skypetokenauth",
        reason: TOKEN_EXCHANGE,
    },
    ExpectedRequest {
        class: "AMS skypetoken exchange",
        hosts: Hosts::Under("asm.skype.com"),
        method: "POST",
        path: "/v1/skypetokenauth",
        reason: TOKEN_EXCHANGE,
    },
    ExpectedRequest {
        class: "search suggestions",
        hosts: Hosts::Exact(&["substrate.office.com"]),
        method: "POST",
        path: "/search/api/v1/suggestions",
        reason: "Substrate's search suggestions (files, people): its answers list other people of the tenant, so it \
                 stays denied (the user's rule of 2026-10-21); the scenario searches nothing, and its mention picker \
                 offers b from the chat's members",
    },
    ExpectedRequest {
        class: "editor service config",
        hosts: Hosts::Exact(&["editor.svc.cloud.microsoft"]),
        method: "POST",
        path: "/NLEditor/Config/V2",
        reason: "the compose box's spelling and grammar service: the scenario types fixed text",
    },
    ExpectedRequest {
        class: "editor service language",
        hosts: Hosts::Exact(&["editor.svc.cloud.microsoft"]),
        method: "POST",
        path: "/NLEditor/api/V1/LanguageInfo",
        reason: "the compose box's spelling and grammar service: the scenario types fixed text",
    },
    ExpectedRequest {
        class: "AugLoop posts",
        hosts: Hosts::Exact(&["augloop.office.com"]),
        method: "POST",
        path: "/",
        reason: "the client's AI and suggestion features: the scenario uses none",
    },
];

/// The denied WebSocket frames a writing scenario expects (see the module docs).
pub const FRAMES: &[ExpectedFrames] = &[
    ExpectedFrames {
        class: "Trouter frames",
        hosts: Hosts::Under("trouter.teams.microsoft.com"),
        reason: "the push channel's session: b reads the chat from the chat service when it opens it, so the scenario \
                 needs no push delivery",
    },
    ExpectedFrames {
        class: "AugLoop frames",
        hosts: Hosts::Exact(&["augloop.office.com"]),
        reason: "the client's AI and suggestion features: the scenario uses none",
    },
];

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
    /// Classes this scenario expects besides the list ([`Review::expecting_requests`]).
    requests: &'static [ExpectedRequest],
}

impl Review {
    /// The list alone: no forced deny is expected, and no target is mapped.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            forced: &[],
            mapped: Vec::new(),
            requests: &[],
        }
    }

    /// Expects the forced denies whose rule has one of these labels ([`crate::fence_http::ForcedDeny::labelled`]).
    #[must_use]
    pub const fn expecting_forced(mut self, labels: &'static [&'static str]) -> Self {
        self.forced = labels;
        self
    }

    /// Expects the denies of these classes too, under the same conditions as the list's: classes only one scenario
    /// meets (`ab-trouter`'s calling-stack registration and presence, which come up only once the push channel is).
    #[must_use]
    pub const fn expecting_requests(mut self, requests: &'static [ExpectedRequest]) -> Self {
        self.requests = requests;
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
            return denial
                .label
                .as_deref()
                .is_some_and(|label| self.forced.contains(&label));
        }
        denial.cause == DenyCause::Unclassified
            && !self.mapped.iter().any(|names| names.is_match(&denial.url))
            && (request_class(denial).is_some() || self.requests.iter().any(|entry| entry.covers(denial)))
    }

    /// Whether `denial` is expected: a frame on a listed socket, denied as [`DenyCause::Unclassified`].
    #[must_use]
    pub fn expects_frame(&self, denial: &FrameDenial) -> bool {
        denial.cause == DenyCause::Unclassified && frame_class(denial).is_some()
    }
}
