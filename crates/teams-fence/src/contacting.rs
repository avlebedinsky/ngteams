//! Whether a request reaches other people: the one classifier the browser fence and the native transport share.
//!
//! [`contacting`] reads a [`Request`] (method, URL, the headers that change what it does, body) and answers a
//! [`Contact`]:
//! - [`Contact::None`]: it reaches nobody and is allowed. Every reading request is, and so is every request to a host
//!   that isn't a contacting service, and the named carve-outs on one (typing, read receipts, the account's own view
//!   of a chat, its own feed, settings, endpoints and token exchanges, reads sent as a `POST`);
//! - [`Contact::Write`]: a mapped write with its [`Target`], allowed only with a permit from [`crate::Fence::check`];
//! - [`Contact::Unreadable`]: a mutating request on a contacting service that no row maps or carves out: denied.
//!
//! The contacting services are the ones whose purpose is reaching people ([`Service`], [`service_of`]): the chat
//! service, calling, Microsoft Graph, Outlook and Substrate, `SharePoint` and `OneDrive`, AMS, the middle tier's team,
//! meeting and invite paths (its Outlook calendar and mail proxy among them), CSA and the teams' provisioning service.
//! On a Teams origin a mutating
//! request whose service-naming segments can't be read (empty with more path after them, not UTF-8 once decoded, or
//! holding `/ \ ; ? #` once decoded) is unreadable too. A request is mutating ([`Request::is_mutating`]) when its
//! method isn't `GET`, `HEAD` or `OPTIONS`, when it carries a body anyway, or when a method-override header names
//! another method; on a contacting service an overridden method is unreadable, since the fence can't tell which
//! method the service acts on. Bodies are read by a strict JSON reader (two keys alike in any case make a body
//! unreadable), path segments percent-decoded, query values exact, and a query key twice (in any case) makes a chat
//! service request unreadable.

mod calendar;
pub(crate) mod call;
mod channel;
mod chat;
pub(crate) mod json;
mod team;

use std::fmt;

use url::{Host, Url};

pub use call::{CALL_START_URL, PROXY_HOST as CALL_PROXY_HOST, TEAMS_ECHO_BOT_MRI, TEST_CALL_SCENARIO};
pub use chat::OWN_VIEW_PROPERTIES;

use crate::{Mri, Target};
use json::fold;

/// What a request does, as far as reaching people goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Contact {
    /// It reaches nobody: allowed.
    None,
    /// A mapped write: allowed only with a permit for its target.
    Write(Target),
    /// A mutating request on a contacting service that no row maps or carves out: denied.
    Unreadable,
}

/// A service whose purpose is reaching people: on it a mutating request is contacting unless a row maps or carves it
/// out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Service {
    /// The chat service: an AFD host's `/api/chatsvc/…`, the regional `*.msg.teams.microsoft.com` hosts.
    ChatService,
    /// Calling: the flight proxy and the Skype-era call hosts behind it (`*.conv.skype.com`, `*.cc.skype.com`,
    /// `*.broker.skype.com`), and every other `*.skype.com` host but AMS's.
    Calling,
    /// Microsoft Graph, every version, `$batch` included.
    Graph,
    /// Outlook, OWA and Substrate.
    Outlook,
    /// `SharePoint`, `OneDrive` and their media hosts.
    SharePoint,
    /// AMS and the async gateway: objects others can read.
    Ams,
    /// The middle tier (`/api/mt/…`) and the apps service (`/api/apps/…`): only its team, chat, channel, group,
    /// meeting and invite paths, and its Outlook calendar and mail proxy (`v2.0/me/…`), are contacting.
    MiddleTier,
    /// The chat service aggregator: an AFD host's `/api/csa/…`, `chatsvcagg.teams.microsoft.com`.
    Csa,
    /// The teams' provisioning service: an AFD host's `/fabric/…`, `templates.teams.microsoft.com`. It makes teams and
    /// channels and adds people to them.
    Provisioning,
}

impl Service {
    /// The name used in logs.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ChatService => "chat_service",
            Self::Calling => "calling",
            Self::Graph => "graph",
            Self::Outlook => "outlook",
            Self::SharePoint => "sharepoint",
            Self::Ams => "ams",
            Self::MiddleTier => "middle_tier",
            Self::Csa => "csa",
            Self::Provisioning => "provisioning",
        }
    }
}

/// What [`contacting`] needs to know beyond the request: the echo call's callee (the echo service `lab.toml`
/// approves), without which the test call start is unreadable, and the account's own MRI, which a chat service path
/// may name in place of `ME`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Policy {
    echo_callee: Option<Mri>,
    own_account: Option<Mri>,
}

impl Policy {
    /// A policy with no echo callee and no own account.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The same policy, mapping the web client's test call start to a call of its caller and `callee`.
    #[must_use]
    pub fn with_echo_callee(mut self, callee: Mri) -> Self {
        self.echo_callee = Some(callee);
        self
    }

    /// The same policy, reading `users/<account>` on the chat service as `users/ME`.
    #[must_use]
    pub fn with_own_account(mut self, account: Mri) -> Self {
        self.own_account = Some(account);
        self
    }
}

/// The method-override headers a service may act on in place of the request's method: the only headers whose values
/// [`contacting`] reads (in any case), so a caller that keeps header values for the classifier keeps these.
pub const OVERRIDE_HEADERS: [&str; 3] = ["x-http-method-override", "x-http-method", "x-method-override"];

/// The methods that only read.
const READING: [&str; 3] = ["GET", "HEAD", "OPTIONS"];

/// One request as the fence sees it. `Debug` shows the method, the URL without its credentials, query or fragment,
/// the header names and the body's length: never a header value, a query value or the body.
#[derive(Clone, PartialEq, Eq)]
pub struct Request<'a> {
    method: &'a str,
    url: &'a str,
    headers: Vec<(&'a str, &'a str)>,
    body: Option<&'a [u8]>,
}

impl fmt::Debug for Request<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let url = Url::parse(self.url).map_or_else(
            |_| "<unparsable>".to_owned(),
            |url| {
                let port = url.port().map(|port| format!(":{port}")).unwrap_or_default();
                format!(
                    "{}://{}{port}{}",
                    url.scheme(),
                    url.host_str().unwrap_or_default(),
                    url.path()
                )
            },
        );
        f.debug_struct("Request")
            .field("method", &self.method)
            .field("url", &url)
            .field(
                "headers",
                &self.headers.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            )
            .field("body_len", &self.body.map(<[u8]>::len))
            .finish()
    }
}

impl<'a> Request<'a> {
    /// A request with no headers and no body.
    #[must_use]
    pub fn new(method: &'a str, url: &'a str) -> Self {
        Self {
            method,
            url,
            headers: Vec::new(),
            body: None,
        }
    }

    /// The same request with one more header. Only a method override changes what the classifier makes of it; every
    /// other header is ignored.
    #[must_use]
    pub fn with_header(mut self, name: &'a str, value: &'a str) -> Self {
        self.headers.push((name, value));
        self
    }

    /// The same request with `body`: `None` for none, `Some` (even empty) for a body, whose bytes a row may read.
    #[must_use]
    pub fn with_body(mut self, body: Option<&'a [u8]>) -> Self {
        self.body = body;
        self
    }

    /// Whether the request can change anything: a method other than `GET`, `HEAD` and `OPTIONS` (in any case), a body
    /// on one of those, or a method override naming another method.
    #[must_use]
    pub fn is_mutating(&self) -> bool {
        !READING.iter().any(|reading| reading.eq_ignore_ascii_case(self.method))
            || self.body.is_some()
            || self.is_overridden()
    }

    /// Whether a method-override header names a method other than the request's own.
    fn is_overridden(&self) -> bool {
        self.headers.iter().any(|(name, value)| {
            OVERRIDE_HEADERS.iter().any(|header| header.eq_ignore_ascii_case(name))
                && !value.trim().eq_ignore_ascii_case(self.method)
        })
    }
}

/// A mutating request with its URL parsed, as the rows read it.
pub(crate) struct Parsed<'a> {
    /// The method in upper case.
    method: String,
    url: &'a Url,
    body: Option<&'a [u8]>,
    /// The query's pairs, percent-decoded (`+` as a space).
    query: Vec<(String, String)>,
}

impl Parsed<'_> {
    /// The value of `key` when it is the query's only key.
    pub(super) fn only_query(&self, key: &str) -> Option<&str> {
        match self.query.as_slice() {
            [(name, value)] if name == key => Some(value),
            _ => None,
        }
    }

    /// Whether a key is in the query twice, in any case.
    fn has_duplicate_key(&self) -> bool {
        let mut seen = std::collections::HashSet::new();
        self.query.iter().any(|(key, _)| !seen.insert(fold(key)))
    }
}

/// What `request` does, as far as reaching people goes (see the module docs).
#[must_use]
pub fn contacting(request: &Request<'_>, policy: &Policy) -> Contact {
    if !request.is_mutating() {
        return Contact::None;
    }
    let Ok(url) = Url::parse(request.url) else {
        // a mutating request whose host can't be told
        return Contact::Unreadable;
    };
    let service = match route(&url) {
        Route::Service(service) => service,
        Route::Other => return Contact::None,
        Route::Unreadable => return Contact::Unreadable,
    };
    if request.is_overridden() {
        return Contact::Unreadable;
    }
    let parsed = Parsed {
        method: request.method.to_ascii_uppercase(),
        url: &url,
        body: request.body,
        query: url
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect(),
    };
    match service {
        Service::ChatService if parsed.has_duplicate_key() => Contact::Unreadable,
        Service::ChatService => team::chat_service(&parsed).unwrap_or_else(|| chat::classify(&parsed, policy)),
        Service::Calling => call::call_start(&parsed.method, &url, parsed.body, policy.echo_callee.as_ref())
            .or_else(|| call::chat_call_start(&parsed.method, &url, parsed.body))
            .or_else(|| call::meeting_call_start(&parsed.method, &url, parsed.body))
            .or_else(|| call::channel_call_start(&parsed.method, &url, parsed.body))
            .or_else(|| call::follow_up(&parsed.method, &url, parsed.body))
            .map_or(Contact::Unreadable, Contact::Write),
        Service::Graph => Contact::Unreadable,
        Service::SharePoint => none_if(is_file_browser_prewarm(&parsed)),
        Service::Outlook => none_if(is_substrate_search(&parsed) || is_owa_startup(&parsed)),
        Service::Ams => none_if(is_token_exchange(&parsed)),
        Service::MiddleTier => calendar::classify(&parsed)
            .or_else(|| channel::middle_tier(&parsed))
            .or_else(|| team::middle_tier(&parsed))
            .unwrap_or_else(|| none_if(!is_middle_tier_contacting(&parsed))),
        Service::Csa => team::csa(&parsed).unwrap_or_else(|| none_if(is_csa_read(&parsed))),
        Service::Provisioning => channel::provisioning(&parsed)
            .or_else(|| team::provisioning(&parsed))
            .unwrap_or(Contact::Unreadable),
    }
}

/// [`Contact::None`] when `allowed`, else [`Contact::Unreadable`].
fn none_if(allowed: bool) -> Contact {
    if allowed { Contact::None } else { Contact::Unreadable }
}

/// Where a URL goes, as [`contacting`] reads it.
enum Route {
    /// A contacting service.
    Service(Service),
    /// A host or a Teams origin path that isn't a contacting service.
    Other,
    /// A Teams origin path whose service-naming segments can't be read ([`service_by_path`]).
    Unreadable,
}

/// The contacting service `url` goes to, if any (see [`Service`]). Hosts are matched in lower case, a trailing dot
/// dropped, any scheme and port; a Teams origin's paths by their first two segments, each decoded on its own and in
/// any case. `None` also for a Teams origin path whose first segments can't be read, which [`contacting`] counts as
/// unreadable when the request is mutating.
#[must_use]
pub fn service_of(url: &Url) -> Option<Service> {
    match route(url) {
        Route::Service(service) => Some(service),
        Route::Other | Route::Unreadable => None,
    }
}

/// Whether `url` goes to a contacting service ([`service_of`]), or to a Teams origin path whose service-naming segments
/// can't be read: where anything sent could reach people. Its scheme plays no part, so a WebSocket's URL is read the
/// same way: the browser fence denies every frame on a socket there but Trouter's.
#[must_use]
pub fn on_contacting_service(url: &Url) -> bool {
    !matches!(route(url), Route::Other)
}

/// [`service_of`], with a Teams origin path whose first segments can't be read told apart.
fn route(url: &Url) -> Route {
    const TEAMS_ORIGINS: [&str; 3] = ["teams.microsoft.com", "teams.cloud.microsoft", "teams.live.com"];
    let Some(Host::Domain(host)) = url.host() else {
        return Route::Other;
    };
    let host = host.to_ascii_lowercase();
    let host = host.strip_suffix('.').unwrap_or(&host);
    if let Some(service) = service_by_host(host) {
        return Route::Service(service);
    }
    if TEAMS_ORIGINS.iter().any(|origin| under(host, origin)) {
        service_by_path(url)
    } else {
        Route::Other
    }
}

/// A contacting service told by its host alone.
fn service_by_host(host: &str) -> Option<Service> {
    const BY_HOST: [(&str, Service); 20] = [
        ("msg.teams.microsoft.com", Service::ChatService),
        ("templates.teams.microsoft.com", Service::Provisioning),
        ("chatsvcagg.teams.microsoft.com", Service::Csa),
        ("asyncgw.teams.microsoft.com", Service::Ams),
        ("asm.skype.com", Service::Ams),
        ("flightproxy.teams.microsoft.com", Service::Calling),
        ("api.fl.teams.microsoft.com", Service::Calling),
        ("skype.com", Service::Calling),
        ("graph.microsoft.com", Service::Graph),
        ("outlook.office.com", Service::Outlook),
        ("outlook.office365.com", Service::Outlook),
        ("outlook.cloud.microsoft", Service::Outlook),
        ("outlook.live.com", Service::Outlook),
        ("substrate.office.com", Service::Outlook),
        ("sharepoint.com", Service::SharePoint),
        ("sharepoint-df.com", Service::SharePoint),
        ("microsoftpersonalcontent.com", Service::SharePoint),
        ("onedrive.com", Service::SharePoint),
        ("onedrive.live.com", Service::SharePoint),
        ("svc.ms", Service::SharePoint),
    ];
    BY_HOST
        .iter()
        .find(|(suffix, _)| under(host, suffix))
        .map(|(_, service)| *service)
}

/// A contacting service on a Teams origin (`teams.microsoft.com`, `teams.cloud.microsoft`, `teams.live.com` and the
/// hosts under them), told by the path's first two segments, each decoded on its own: a later segment that doesn't
/// decode leaves the service to its row, which refuses it. [`Route::Unreadable`] when one of the two is empty with
/// more path after it (`//api/chatsvc/…`, `/api//chatsvc/…`, which a server merging slashes reads as the service),
/// isn't UTF-8 once decoded, or holds `/ \ ; ? #` once decoded (`/api/chatsvc%2Femea/…`).
fn service_by_path(url: &Url) -> Route {
    let raw: Vec<&str> = url.path_segments().map(Iterator::collect).unwrap_or_default();
    let mut leading = Vec::with_capacity(2);
    for (at, segment) in raw.iter().take(2).enumerate() {
        let Ok(decoded) = percent_encoding::percent_decode_str(segment).decode_utf8() else {
            return Route::Unreadable;
        };
        let empty_mid_path = decoded.is_empty() && at + 1 < raw.len();
        if empty_mid_path || decoded.contains(['/', '\\', ';', '?', '#']) {
            return Route::Unreadable;
        }
        leading.push(decoded.to_ascii_lowercase());
    }
    let service = match leading.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["api", "chatsvc"] => Service::ChatService,
        ["api", "mt" | "apps"] => Service::MiddleTier,
        ["api", "csa"] => Service::Csa,
        ["asm" | "ams", ..] => Service::Ams,
        ["fabric", ..] => Service::Provisioning,
        _ => return Route::Other,
    };
    Route::Service(service)
}

/// Whether `host` is `domain` or a host under it, on a dot boundary.
fn under(host: &str, domain: &str) -> bool {
    host.strip_suffix(domain)
        .is_some_and(|rest| rest.is_empty() || rest.ends_with('.'))
}

/// The URL's path segments, each percent-decoded; `None` when one isn't UTF-8 once decoded.
pub(crate) fn decoded_segments(url: &Url) -> Option<Vec<String>> {
    url.path_segments()?
        .map(|segment| {
            percent_encoding::percent_decode_str(segment)
                .decode_utf8()
                .ok()
                .map(String::from)
        })
        .collect()
}

/// Whether the decoded path, compared in lower case, ends with `tail` (a `*` segment stands for any one).
fn path_ends_with(request: &Parsed<'_>, tail: &[&str]) -> bool {
    let Some(segments) = decoded_segments(request.url) else {
        return false;
    };
    segments.len() >= tail.len()
        && segments[segments.len() - tail.len()..]
            .iter()
            .zip(tail)
            .all(|(segment, wanted)| *wanted == "*" || segment.eq_ignore_ascii_case(wanted))
}

/// A Substrate search or suggestion read, sent as a `POST` on `substrate.office.com`: `/search/api/<version>/suggestions`
/// or `…/query`, the web client's title-bar search `/searchservice/api/<version>/query` (`docs/protocol/search.md`),
/// and the search's own telemetry `/search/api/<version>/events` (with or without its trailing slash): what the
/// account searched and was shown, told to the search service alone.
fn is_substrate_search(request: &Parsed<'_>) -> bool {
    request.method == "POST"
        && request
            .url
            .host_str()
            .is_some_and(|host| under(&host.to_ascii_lowercase(), "substrate.office.com"))
        && decoded_segments(request.url).is_some_and(|segments| {
            let lower: Vec<String> = segments.iter().map(|segment| segment.to_ascii_lowercase()).collect();
            let lower: Vec<&str> = lower.iter().map(String::as_str).collect();
            let lower = match lower.as_slice() {
                [rest @ .., ""] => rest,
                all => all,
            };
            match lower {
                ["search", "api", version, read] => {
                    is_version(version) && matches!(*read, "suggestions" | "query" | "events")
                }
                ["searchservice", "api", version, "query"] => is_version(version),
                _ => false,
            }
        })
}

/// The embedded Outlook calendar app's startup read, sent as a `POST`: `…/owa/startupdata.ashx` on
/// `outlook.office.com` (`docs/protocol/calendar.md`). It reads the account's own startup data and reaches nobody.
fn is_owa_startup(request: &Parsed<'_>) -> bool {
    request.method == "POST"
        && request
            .url
            .host_str()
            .is_some_and(|host| host.trim_end_matches('.').eq_ignore_ascii_case("outlook.office.com"))
        && decoded_segments(request.url).is_some_and(|segments| {
            matches!(segments.as_slice(), [owa, page]
                if owa.eq_ignore_ascii_case("owa") && page.eq_ignore_ascii_case("startupdata.ashx"))
        })
}

/// The OneDrive file browser's frame as the client prewarms it: a document loaded by a form `POST` of the account's
/// own SharePoint token to its own personal site, `https://<tenant>-my.sharepoint.com/personal/<user>/_layouts/15/
/// filebrowser.aspx` (a host one label under `sharepoint.com`). It reaches nobody.
fn is_file_browser_prewarm(request: &Parsed<'_>) -> bool {
    let one_label_under = request.url.host_str().is_some_and(|host| {
        host.trim_end_matches('.')
            .to_ascii_lowercase()
            .strip_suffix(".sharepoint.com")
            .is_some_and(|label| !label.is_empty() && !label.contains('.'))
    });
    request.method == "POST"
        && one_label_under
        && decoded_segments(request.url).is_some_and(|segments| {
            matches!(segments.as_slice(), [personal, user, layouts, version, page]
                if personal.eq_ignore_ascii_case("personal")
                    && !user.is_empty()
                    && layouts.eq_ignore_ascii_case("_layouts")
                    && version == "15"
                    && page.eq_ignore_ascii_case("filebrowser.aspx"))
        })
}

/// AMS's token exchanges, which trade the account's own token for the media service's cookie: `POST
/// …/v1/skypetokenauth` and `POST …/v1/<id>/aadtokenauth`.
fn is_token_exchange(request: &Parsed<'_>) -> bool {
    request.method == "POST"
        && (path_ends_with(request, &["v1", "skypetokenauth"]) || path_ends_with(request, &["v1", "*", "aadtokenauth"]))
}

/// The middle tier's segments that name something other people are in or get: a team, chat, channel, conversation,
/// thread or group; and its Outlook REST proxy (`v2.0/me/calendars/…` is read through it), where an event or a message
/// reaches its attendees or recipients.
const MIDDLE_TIER_PEOPLE: [&str; 17] = [
    "team",
    "teams",
    "chat",
    "chats",
    "channel",
    "channels",
    "conversation",
    "conversations",
    "thread",
    "threads",
    "group",
    "groups",
    "v2.0",
    "event",
    "events",
    "message",
    "messages",
];

/// What a middle tier segment holding any of these names (in lower case) is about: a meeting, an invite, a calendar
/// (`calendars`, `calendarView`) or a mail (`sendmail`, `mailFolders`).
const MIDDLE_TIER_PEOPLE_WORDS: [&str; 4] = ["meeting", "invite", "calendar", "mail"];

/// Whether a mutating middle tier request is contacting: a segment of [`MIDDLE_TIER_PEOPLE`], or one holding a word of
/// [`MIDDLE_TIER_PEOPLE_WORDS`]. The app catalogue's entitlements of the account's teams (`POST
/// …/beta/teams/apps/aggregatedEntitlements`), a read, is carved out.
fn is_middle_tier_contacting(request: &Parsed<'_>) -> bool {
    if request.method == "POST" && path_ends_with(request, &["beta", "teams", "apps", "aggregatedEntitlements"]) {
        return false;
    }
    let Some(segments) = decoded_segments(request.url) else {
        return true;
    };
    segments
        .iter()
        .map(|segment| segment.to_ascii_lowercase())
        .any(|segment| {
            MIDDLE_TIER_PEOPLE.contains(&segment.as_str())
                || MIDDLE_TIER_PEOPLE_WORDS.iter().any(|word| segment.contains(word))
        })
}

/// CSA's read of chats and channels by id, sent as a `POST`: `…/api/<version>/teams/users/me/chatsAndChannelsByIds`.
fn is_csa_read(request: &Parsed<'_>) -> bool {
    request.method == "POST"
        && path_ends_with(request, &["api", "*", "teams", "users", "me", "chatsAndChannelsByIds"])
        && decoded_segments(request.url).is_some_and(|segments| {
            segments
                .len()
                .checked_sub(5)
                .and_then(|at| segments.get(at))
                .is_some_and(|version| is_version(&version.to_ascii_lowercase()))
        })
}

/// A version segment: `v` and digits, with an optional `.` part (`v1`, `v2`, `v1.0`).
fn is_version(segment: &str) -> bool {
    segment.strip_prefix('v').is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .split('.')
                .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    })
}
