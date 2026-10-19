//! What a browser request is, as far as the fence is concerned.
//!
//! [`classify`] sorts a request into:
//! - [`Classification::Read`]: `GET`, `HEAD` or `OPTIONS`, which change nothing anyone else sees;
//! - [`Classification::Own`]: a mutating request on the seeded, reviewed own-state list (the Entra token endpoint and
//!   Teams' authz exchange), which changes only the signed-in account's own session; `proto-lab login` adds Entra's
//!   sign-in form ([`Classifier::sign_in`]);
//! - [`Classification::Write`]: a mapped write with its [`Target`], judged by [`teams_fence::Fence::check`];
//! - [`Classification::Unknown`]: everything else, denied.
//!
//! The seeded classifier ([`Classifier::seeded`], and [`Classifier::sign_in`] for `login`) maps no writes, so a run
//! that uses it is fail-closed: every mutating request outside the own-state list is [`Classification::Unknown`]. A
//! run that writes uses [`Classifier::mapped`], which also maps the chat service's writes on chats
//! (`docs/protocol/fence-targets.md`): messages, typing, edits, deletes, reactions, read receipts, new chats and member
//! changes, each to its [`Target`]. Anything the mapping doesn't recognize, a channel or team among them, stays
//! [`Classification::Unknown`]. Any method other than the three reading ones counts as mutating, including ones the
//! classifier has never heard of.
//!
//! An own-state rule matches the method, the exact origin (scheme, host and port) and the path segment by segment; a
//! `*` segment stands for exactly one non-empty segment (an Entra tenant). The query plays no part.

use percent_encoding::percent_decode_str;
use serde_json::Value;
use teams_fence::{AFD_HOSTS, Mri, REGIONAL_SUFFIX, Scope, Target, WriteKind};
use url::Url;

/// A reviewed kind of own-state request: it changes only the signed-in account's own session state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OwnKind {
    /// The Entra token endpoint: code exchange and refresh.
    EntraToken,
    /// Teams' authz exchange: an access token for a skypetoken and the regional endpoints.
    Authz,
    /// Entra's sign-in form: the username lookup, the password and "Stay signed in?". Only `proto-lab login` allows
    /// it ([`Classifier::sign_in`]).
    EntraSignIn,
}

impl OwnKind {
    /// The name used in `fence.log`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EntraToken => "entra_token",
            Self::Authz => "authz",
            Self::EntraSignIn => "entra_sign_in",
        }
    }
}

/// What the fence makes of one request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Classification {
    /// A reading method: let through.
    Read,
    /// A mutating request on the own-state list: let through.
    Own(OwnKind),
    /// A mapped write: let through only with a permit from the fence.
    Write(Target),
    /// A mutating request nothing classifies: denied.
    Unknown,
}

/// An exact origin and a path matched segment by segment; the query plays no part.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UrlPattern {
    origin: String,
    path: Vec<String>,
}

impl UrlPattern {
    /// `origin` is `scheme://host[:port]` as a browser serializes it (no default port, lowercase host); `path` starts
    /// with `/`, and a `*` segment stands for one non-empty segment.
    #[must_use]
    pub fn new(origin: &str, path: &str) -> Self {
        Self {
            origin: origin.to_owned(),
            path: segments(path).map(str::to_owned).collect(),
        }
    }

    /// Whether `url` has exactly this origin and a path this pattern matches.
    #[must_use]
    pub fn matches(&self, url: &Url) -> bool {
        if self.origin != url.origin().ascii_serialization() {
            return false;
        }
        let mut path = segments(url.path());
        let mut pattern = self.path.iter();
        loop {
            match (pattern.next(), path.next()) {
                (None, None) => return true,
                (Some(wanted), Some(segment)) if !segment.is_empty() && (wanted == "*" || wanted == segment) => {}
                _ => return false,
            }
        }
    }
}

/// One entry of the own-state list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnRule {
    kind: OwnKind,
    method: String,
    url: UrlPattern,
}

impl OwnRule {
    /// `origin` and `path` as [`UrlPattern::new`] takes them.
    #[must_use]
    pub fn new(kind: OwnKind, method: &str, origin: &str, path: &str) -> Self {
        Self {
            kind,
            method: method.to_ascii_uppercase(),
            url: UrlPattern::new(origin, path),
        }
    }

    fn matches(&self, method: &str, url: &Url) -> bool {
        self.method == method && self.url.matches(url)
    }
}

/// The path's segments after the leading `/`.
fn segments(path: &str) -> std::str::Split<'_, char> {
    path.strip_prefix('/').unwrap_or(path).split('/')
}

/// The authz endpoints of the commercial cloud, as the web client names them.
const AUTHZ: [(&str, &str); 4] = [
    ("https://teams.microsoft.com", "/api/authsvc/v1.0/authz"),
    ("https://teams.microsoft.com", "/api/authsvc/v1.0/authzcore"),
    ("https://authsvc.teams.microsoft.com", "/v1.0/authz"),
    ("https://authsvc.teams.microsoft.com", "/v1.0/authzcore"),
];

/// Entra's sign-in form on `login.microsoftonline.com`, as the sign-in page posts it (`*` is `common`,
/// `organizations` or a tenant): the username lookup, the password, and the answer to "Stay signed in?". Nothing
/// of multi-factor sign-in (`SAS/*`) or of the account's security info is on it: what those requests set off (a code
/// sent by text or call, a push to a phone) and whether verifying a method can be told apart from registering one has
/// never been captured, so `login` stops at any multi-factor prompt instead.
const SIGN_IN: [&str; 4] = ["/*/GetCredentialType", "/*/login", "/kmsi", "/*/kmsi"];

/// The classifier: the own-state list, and for a run that writes, the write mapping.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Classifier {
    own: Vec<OwnRule>,
    /// Whether the chat service's writes are mapped ([`Classifier::mapped`]).
    writes: bool,
}

impl Classifier {
    /// The seeded own-state list: `POST` to the Entra v2 token endpoint of any tenant on `login.microsoftonline.com`,
    /// and `POST` to Teams' authz endpoints.
    #[must_use]
    pub fn seeded() -> Self {
        let mut own = vec![OwnRule::new(
            OwnKind::EntraToken,
            "POST",
            "https://login.microsoftonline.com",
            "/*/oauth2/v2.0/token",
        )];
        own.extend(
            AUTHZ
                .iter()
                .map(|(origin, path)| OwnRule::new(OwnKind::Authz, "POST", origin, path)),
        );
        Self { own, writes: false }
    }

    /// The seeded list plus the write mapping (see the module docs): what a run that writes uses, together with a
    /// membership source that can read the targets' members (`teams_fence::LiveMembershipSource`).
    #[must_use]
    pub fn mapped() -> Self {
        Self {
            writes: true,
            ..Self::seeded()
        }
    }

    /// The seeded list plus Entra's sign-in form ([`OwnKind::EntraSignIn`]): what `proto-lab login` needs to type a
    /// lab account's password into Microsoft's sign-in page under the fence. The form changes only that account's own
    /// sign-in session. Nothing else uses it: `status` and the scenarios never sign in.
    #[must_use]
    pub fn sign_in() -> Self {
        let mut classifier = Self::seeded();
        classifier.own.extend(
            SIGN_IN
                .iter()
                .map(|path| OwnRule::new(OwnKind::EntraSignIn, "POST", "https://login.microsoftonline.com", path)),
        );
        classifier
    }

    /// The same list plus one more own-state rule. Tests use it to stand a local endpoint in for a Microsoft one.
    #[must_use]
    pub fn with_own(mut self, rule: OwnRule) -> Self {
        self.own.push(rule);
        self
    }

    /// Classifies one request: a read, an own-state request, a mapped write (with [`Classifier::mapped`]; the body
    /// names what some writes are), or unknown.
    #[must_use]
    pub fn classify(&self, method: &str, url: &str, body: Option<&[u8]>) -> Classification {
        let method = method.to_ascii_uppercase();
        if !is_mutating(&method) {
            return Classification::Read;
        }
        let Ok(url) = Url::parse(url) else {
            return Classification::Unknown;
        };
        if let Some(rule) = self.own.iter().find(|rule| rule.matches(&method, &url)) {
            return Classification::Own(rule.kind);
        }
        if self.writes
            && let Some(target) = map_write(&method, &url, body)
        {
            return Classification::Write(target);
        }
        Classification::Unknown
    }
}

/// The chat service path of `url` after its base, from `v1` on, each segment percent-decoded: `Some` for
/// `https://<AFD host>/api/chatsvc/<region>/v1/…` and `https://<name>.msg.teams.microsoft.com/v1/…` only (no port, no
/// user info).
#[must_use]
pub fn chat_service_path(url: &Url) -> Option<Vec<String>> {
    if url.scheme() != "https" || url.port().is_some() || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    let segments: Vec<&str> = url.path_segments()?.collect();
    let rest = if AFD_HOSTS.contains(&host.as_str()) {
        match segments.as_slice() {
            ["api", "chatsvc", region, rest @ ..] if !region.is_empty() => rest,
            _ => return None,
        }
    } else if host.strip_suffix(REGIONAL_SUFFIX).is_some_and(|name| !name.is_empty()) {
        segments.as_slice()
    } else {
        return None;
    };
    if rest.first() != Some(&"v1") {
        return None;
    }
    rest[1..]
        .iter()
        .map(|segment| percent_decode_str(segment).decode_utf8().ok().map(String::from))
        .collect()
}

/// The write a chat service request makes, if the mapping knows it (`docs/protocol/fence-targets.md`).
fn map_write(method: &str, url: &Url, body: Option<&[u8]>) -> Option<Target> {
    let path = chat_service_path(url)?;
    let path: Vec<&str> = path.iter().map(String::as_str).collect();
    let name = || {
        url.query_pairs()
            .find(|(key, _)| key == "name")
            .map(|(_, value)| value.into_owned())
    };
    let on = |kind, chat: &str| chat_scope(chat).map(|scope| Target::new(kind, scope));
    match (method, path.as_slice()) {
        ("POST", ["users", "ME", "conversations", chat, "messages"]) => on(message_kind(body?)?, chat),
        ("PUT", ["users", "ME", "conversations", chat, "messages", _]) => on(WriteKind::Edit, chat),
        ("DELETE", ["users", "ME", "conversations", chat, "messages", _]) => on(WriteKind::Delete, chat),
        ("PUT" | "DELETE", ["users", "ME", "conversations", chat, "messages", _, "properties"])
            if name()? == "emotions" =>
        {
            on(WriteKind::Reaction, chat)
        }
        ("PUT", ["users", "ME", "conversations", chat, "properties"]) if name()? == "consumptionhorizon" => {
            on(WriteKind::ReadReceipt, chat)
        }
        ("POST", ["threads"]) => Some(Target::new(WriteKind::ChatCreate, Scope::New).with_added(body_members(body?)?)),
        ("PUT", ["threads", chat, "members"]) => {
            Some(on(WriteKind::MemberChange, chat)?.with_added(body_members(body?)?))
        }
        ("PUT", ["threads", chat, "members", member]) if is_member(member) => {
            Some(on(WriteKind::MemberChange, chat)?.with_added([Mri::new(*member)]))
        }
        ("DELETE", ["threads", chat, "members", member]) if is_member(member) => on(WriteKind::MemberChange, chat),
        _ => None,
    }
}

/// A chat's conversation id: `19:…@unq.gbl.spaces` (1:1) or `19:…@thread.v2` (group and meeting chats), never a
/// channel or team (`@thread.tacv2`, `@thread.skype`) or a reply chain within one (`;messageid=`).
fn chat_scope(id: &str) -> Option<Scope> {
    let chat = id.starts_with("19:")
        && (id.ends_with("@unq.gbl.spaces") || id.ends_with("@thread.v2"))
        && !id.contains([';', '/', '?', '#']);
    chat.then(|| Scope::Conversation { id: id.to_owned() })
}

/// What a message post is, by its `messagetype`: typing (`Control/Typing`, `Control/ClearTyping`) or a message (`Text`,
/// `RichText`, `RichText/…`). Any other type, or a body that isn't a JSON object with one, is unknown.
fn message_kind(body: &[u8]) -> Option<WriteKind> {
    let body: Value = serde_json::from_slice(body).ok()?;
    match body.get("messagetype")?.as_str()? {
        "Control/Typing" | "Control/ClearTyping" => Some(WriteKind::Typing),
        "Text" | "RichText" => Some(WriteKind::Message),
        kind if kind.starts_with("RichText/") => Some(WriteKind::Message),
        _ => None,
    }
}

/// The members a thread body names (`{"members": [{"id": "<mri>", …}, …]}`): all of them, or `None` when the list is
/// missing, empty, or has a member without a string id.
fn body_members(body: &[u8]) -> Option<Vec<Mri>> {
    let body: Value = serde_json::from_slice(body).ok()?;
    let members: Vec<Mri> = body
        .get("members")?
        .as_array()?
        .iter()
        .map(|member| member.get("id")?.as_str().map(Mri::new))
        .collect::<Option<_>>()?;
    (!members.is_empty()).then_some(members)
}

/// One member named in a path: an MRI, not the `ALL` every-member form.
fn is_member(segment: &str) -> bool {
    segment.contains(':') && !segment.eq_ignore_ascii_case("ALL")
}

/// Whether a method can change anything: every method but `GET`, `HEAD` and `OPTIONS` (in any case).
#[must_use]
pub fn is_mutating(method: &str) -> bool {
    !["GET", "HEAD", "OPTIONS"]
        .iter()
        .any(|reading| reading.eq_ignore_ascii_case(method))
}

/// Classifies one request with the seeded classifier.
#[must_use]
pub fn classify(method: &str, url: &str, body: Option<&[u8]>) -> Classification {
    Classifier::seeded().classify(method, url, body)
}
