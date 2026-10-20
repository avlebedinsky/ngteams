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
//! [`Classification::Unknown`]. A run that chats over the push channel adds the messaging stack's endpoint
//! registration ([`Classifier::messaging`]); a run that calls, the calling stack's own-state and its call start
//! ([`Classifier::calling`]). Any method other than the three reading ones counts as mutating, including ones the
//! classifier has never heard of.
//!
//! An own-state rule matches the method, the exact origin (scheme, host and port) and the path segment by segment; a
//! `*` segment stands for exactly one non-empty segment (an Entra tenant). The query plays no part.

use std::collections::HashSet;
use std::fmt;

use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};
/// The chat service path of a request URL after its base, from `v1` on, each segment percent-decoded; `None` for
/// anything but the chat service. It is teams-fence's, so the mapping and the live member read recognize the chat
/// service by one check.
pub use teams_fence::chat_service_path;
use teams_fence::{Mri, Scope, Target, WriteKind};
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
    /// The account's own Trouter (push channel) session: authenticating its own endpoint's socket. Only a run that
    /// calls or chats allows it ([`crate::fence_ws::FrameClassifier::calling`],
    /// [`crate::fence_ws::FrameClassifier::messaging`], `crate::calling`).
    TrouterSession,
    /// Registering the account's own endpoint (its Trouter URL) with the registrar, which makes it reachable. Only a
    /// run that calls allows it for both stacks ([`Classifier::calling`]); a run that chats, for the messaging stack's
    /// endpoint only ([`Classifier::messaging`], `crate::calling`).
    EndpointRegistration,
    /// Reading the account's own settings and policies from the middle tier, which takes the names to read as a
    /// `POST` body. The calling stack stays off until it knows the calling policy. Only a run that calls allows it
    /// ([`Classifier::calling`], `crate::calling`).
    SettingsRead,
}

impl OwnKind {
    /// The name used in `fence.log`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EntraToken => "entra_token",
            Self::Authz => "authz",
            Self::EntraSignIn => "entra_sign_in",
            Self::TrouterSession => "trouter_session",
            Self::EndpointRegistration => "endpoint_registration",
            Self::SettingsRead => "settings_read",
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
    /// Whether the calling stack's own-state passes and call starts are mapped ([`Classifier::calling`]).
    calling: bool,
    /// Whether the messaging stack's endpoint registration passes ([`Classifier::messaging`]).
    messaging: bool,
    /// The participant the web client's test call is for: the echo service `lab.toml` approves.
    test_call_callee: Option<Mri>,
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
        Self {
            own,
            writes: false,
            calling: false,
            messaging: false,
            test_call_callee: None,
        }
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

    /// The write mapping ([`Classifier::mapped`]) plus the calling stack's own-state (`crate::calling::own_request`:
    /// the account's own endpoint registration and settings/policy reads) and one more write, the web client's test
    /// call start (`crate::calling::call_start`, a [`WriteKind::Call`] on [`Scope::New`] the fence judges, mapped only
    /// with [`Classifier::with_test_call_callee`]). What a run that calls (`echo-call`) uses.
    #[must_use]
    pub fn calling() -> Self {
        Self {
            calling: true,
            ..Self::mapped()
        }
    }

    /// The write mapping ([`Classifier::mapped`]) plus the messaging stack's endpoint registration
    /// (`crate::calling::messaging_request`: the account's own messaging endpoint, never the calling stack's), so its
    /// chat events reach its Trouter socket. What a run that chats over the push channel (`ab-trouter`) uses, with
    /// [`crate::fence_ws::FrameClassifier::messaging`].
    #[must_use]
    pub fn messaging() -> Self {
        Self {
            messaging: true,
            ..Self::mapped()
        }
    }

    /// The participant the web client's test call is for, which its start doesn't name (`crate::calling::call_start`):
    /// the echo service `lab.toml` approves. Without one, a test call start is unknown.
    #[must_use]
    pub fn with_test_call_callee(mut self, callee: Mri) -> Self {
        self.test_call_callee = Some(callee);
        self
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
        if self.messaging
            && let Some(kind) = crate::calling::messaging_request(&method, &url, body)
        {
            return Classification::Own(kind);
        }
        if self.calling {
            if let Some(kind) = crate::calling::own_request(&method, &url, body) {
                return Classification::Own(kind);
            }
            if let Some(target) = crate::calling::call_start(&method, &url, body, self.test_call_callee.as_ref()) {
                return Classification::Write(target);
            }
        }
        if self.writes
            && let Some(target) = map_write(&method, &url, body)
        {
            return Classification::Write(target);
        }
        Classification::Unknown
    }
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
/// `RichText`, `RichText/…`). Any other type, or a body that isn't a JSON object with one (read by [`strict_json`]), is
/// unknown.
fn message_kind(body: &[u8]) -> Option<WriteKind> {
    let body = strict_json(body)?;
    match body.get("messagetype")?.as_str()? {
        "Control/Typing" | "Control/ClearTyping" => Some(WriteKind::Typing),
        "Text" | "RichText" => Some(WriteKind::Message),
        kind if kind.starts_with("RichText/") => Some(WriteKind::Message),
        _ => None,
    }
}

/// The members a thread body names (`{"members": [{"id": "<mri>", …}, …]}`): all of them, or `None` when the body
/// isn't read by [`strict_json`], or the list is missing, empty, or has a member that isn't an object with a string id.
fn body_members(body: &[u8]) -> Option<Vec<Mri>> {
    let body = strict_json(body)?;
    let members: Vec<Mri> = body
        .get("members")?
        .as_array()?
        .iter()
        .map(|member| member.get("id")?.as_str().map(Mri::new))
        .collect::<Option<_>>()?;
    (!members.is_empty()).then_some(members)
}

/// A request body as one JSON value, read so that the fence and the chat service can't read it differently: `None`
/// for anything but a single JSON value, and for a body in which any object has two keys a case-insensitive reader
/// could take for one (an exact duplicate, `members` beside `Members`, `id` beside `ID`). A .NET service may bind keys
/// in any case and keep either duplicate, so such a body is ambiguous and its write stays unknown. With every key
/// unique in any case, an exact lookup ([`Value::get`]) finds the one key the service binds, or nothing: a key the
/// fence would miss only because of its case (`Members` alone) reads as missing, which is unknown too.
pub(crate) fn strict_json(body: &[u8]) -> Option<Value> {
    let mut reader = serde_json::Deserializer::from_slice(body);
    let value = Strict.deserialize(&mut reader).ok()?;
    reader.end().ok()?;
    Some(value)
}

/// A key as a case-insensitive reader compares it: upper-cased, then lower-cased, so `ſ` and `K` (Kelvin) fold with
/// `s` and `k`. Where it folds more than such a reader would (`ß` and `ss`), a body is refused that didn't need to be.
fn fold(key: &str) -> String {
    key.to_uppercase().to_lowercase()
}

/// [`strict_json`]'s reader: a [`Value`], refusing an object with two keys of one [`fold`].
struct Strict;

impl<'de> DeserializeSeed<'de> for Strict {
    type Value = Value;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Strict {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JSON whose objects have no two keys alike in any case")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Value, E> {
        Ok(Value::from(value))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Value, E> {
        Ok(Value::from(value))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Value, E> {
        Ok(Value::from(value))
    }

    fn visit_str<E>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Value, E> {
        Ok(Value::String(value))
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(Strict)? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut object = Map::new();
        let mut seen = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(fold(&key)) {
                return Err(de::Error::custom("two keys alike in some case"));
            }
            let value = map.next_value_seed(Strict)?;
            object.insert(key, value);
        }
        Ok(Value::Object(object))
    }
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
