//! `proto-lab probe chat <alias> [--with <alias>] [--save <name>]`: does the chat service take **our** `ic3` token
//! (the native client's, without CAE), and in which header form? Read-only (M1 Task 3).
//!
//! 1. The account's `ic3` access token is refreshed from its stored refresh token ([`TokenClient::refresh`], audience
//!    [`IC3_RESOURCE`]); nothing else is asked of Entra.
//! 2. The chat base is the account's `chatServiceAfd` GTM (else `chatService`), trimmed, from the region record
//!    `proto-lab auth` saved ([`region_file`]); it must be a base the fence's write mapping recognizes as the chat
//!    service ([`teams_fence::chat_service_path`]), checked before any request is sent.
//! 3. Only `GET`s go to that base: the chat service's own conversation list, one page as the web client asks for it
//!    ([`CONVERSATIONS_PATH`] with [`CONVERSATIONS_QUERY`], from the bundle's `myconversations` request), once in each
//!    [`HeaderForm`] (with and without `X-MS-Migration: True`, with and without `BehaviorOverride: redirectAs404`);
//!    then the fence's own 1:1 thread read of the account's chat with `--with` (a with b, b and c with a by default),
//!    through [`LiveMembershipSource`] with a [`ChatCredential::bearer`] made of that token and base.
//! 4. The report ([`ChatProbe::lines`]) shows statuses, header names, a challenge's scheme and parameter names (and its
//!    `error` value), a `Location`'s host, the thread read's verdict, and the token's claims (aud, exp, scopes,
//!    tenant): never a token, a header value otherwise, or a body. Redirects aren't followed.
//!
//! `--save <name>` hands the conversation-list answers ([`ChatProbe::reference_events`]) to
//! [`export::export_events`](crate::commands::export::export_events): sanitized as one capture, written as a local
//! reference copy under the lab's `reference/` dir, refused inside any git work tree, as `export` does. The name and
//! the destination are checked ([`export::check_destination`](crate::commands::export::check_destination)) before
//! any request goes out.
//!
//! A test reaches a local mock instead of the chat service only through [`Origin::Mock`], which a build without
//! `teams-fence`'s `test-support` feature refuses (and the `proto-lab` binary refuses to run in such a build).

use std::fmt::{self, Write as _};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::{HeaderMap, LOCATION, WWW_AUTHENTICATE};
use serde_json::{Map, Value, json};
use teams_auth::authz::Region;
use teams_auth::claims::TokenSummary;
use teams_auth::secrets::SecretStore;
use teams_auth::token::{Scope, TokenClient, TokenError};
use teams_fence::{
    ChatCredential, LiveMembershipSource, Membership, MembershipSource as _, Scope as FenceScope, chat_service_path,
};
use url::Url;

use crate::commands::auth::{region_file, summary_line};
use crate::config::{Alias, LabConfig};
use crate::scenario::ab_chat::one_to_one_id;

/// The audience of the chat service's Entra token.
pub const IC3_RESOURCE: &str = "https://ic3.teams.office.com";

/// The chat service's conversation list, relative to the chat base.
pub const CONVERSATIONS_PATH: &str = "v1/users/ME/conversations";

/// One page of it as the web client asks: `view=msnp24Equivalent`, its largest page (200), every conversation since
/// the start (`startTime=1`), every kind (`targetType=All`).
pub const CONVERSATIONS_QUERY: &str = "view=msnp24Equivalent&pageSize=200&startTime=1&targetType=All";

/// The GTM the chat base comes from: the web client's (`useChatServiceAfd`); [`Region::base`] falls back to
/// `chatService` when the region lacks it.
pub const CHAT_BASE_GTM: &str = "chatServiceAfd";

/// How long one read may take, end to end.
pub const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// The largest answer body kept, in bytes; a longer one is [`ReadFailure::TooLarge`].
pub const MAX_BODY: usize = 8 * 1024 * 1024;

/// Which of the web client's extra chat headers a read carries (the token and `Accept` always go).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeaderForm {
    /// `X-MS-Migration: True` (Skype token migration).
    pub migration: bool,
    /// `BehaviorOverride: redirectAs404` (a wrong region answers 404 with a `Location`).
    pub behavior_override: bool,
}

impl HeaderForm {
    /// Every form, the web client's own first, the bare token last.
    pub const ALL: [Self; 4] = [
        Self {
            migration: true,
            behavior_override: true,
        },
        Self {
            migration: true,
            behavior_override: false,
        },
        Self {
            migration: false,
            behavior_override: true,
        },
        Self {
            migration: false,
            behavior_override: false,
        },
    ];

    /// The extra headers the form sends.
    fn headers(self) -> Vec<(&'static str, &'static str)> {
        let mut headers = Vec::new();
        if self.migration {
            headers.push(("X-MS-Migration", "True"));
        }
        if self.behavior_override {
            headers.push(("BehaviorOverride", "redirectAs404"));
        }
        headers
    }
}

impl fmt::Display for HeaderForm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.headers().into_iter().map(|(name, _)| name).collect();
        if names.is_empty() {
            f.write_str("token only")
        } else {
            write!(f, "token + {}", names.join(" + "))
        }
    }
}

/// Where the probe's reads go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The chat service, at the base the GTMs give.
    ChatService,
    /// A local mock (`http://127.0.0.1:<port>`) in place of the base's origin, with the base's path. Tests only:
    /// refused unless the build has `teams-fence`'s `test-support` feature.
    Mock(String),
}

/// What to probe.
#[derive(Clone, Debug)]
pub struct ProbeRequest<'a> {
    pub config: &'a LabConfig,
    /// The account whose token is probed.
    pub alias: Alias,
    /// The other account of the 1:1 chat whose thread the fence reads.
    pub partner: Alias,
    pub origin: Origin,
}

/// Why the probe couldn't run. None of these carries a token or an answer.
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("{0} has no MRI in lab.toml (sign it in with `proto-lab login`)")]
    NoMri(Alias),
    #[error("{0} and {1} have no 1:1 chat (one account twice, or an MRI that isn't 8:orgid:<object id>)")]
    NoChat(Alias, Alias),
    #[error("no a-b-c group chat to read: {0}")]
    NoGroupChat(String),
    #[error("can't read the region record {path} (run `proto-lab auth`): {source}")]
    Region { path: String, source: std::io::Error },
    #[error("{0} isn't a region record (run `proto-lab auth`)")]
    BadRegion(String),
    #[error("the region has neither a chatServiceAfd nor a chatService GTM")]
    NoChatBase,
    #[error("the chat base from the GTMs isn't a chat service base: {0:?}")]
    NotAChatBase(String),
    #[error("a mock origin is for tests only (a build with teams-fence's test-support feature)")]
    MockOrigin,
    #[error("the ic3 token: {0}")]
    Token(#[from] TokenError),
    #[error("the refreshed ic3 token isn't token-shaped, so it can't be a chat service credential")]
    BadToken,
    #[error("can't build the HTTPS client: {0}")]
    Client(#[source] reqwest::Error),
}

/// A `WWW-Authenticate` challenge as far as it may be shown: its scheme, its parameters' names, and the value of its
/// `error` parameter (`insufficient_claims` and the like), never the others' values. A scheme or a name that isn't
/// [`is_name`]-shaped, and anything that isn't a `name=value` pair (a token68 such as `Negotiate YIIabc==`), is shown
/// as [`HIDDEN`]: it may be part of a value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Challenge {
    pub scheme: String,
    pub parameters: Vec<String>,
    pub error: Option<String>,
}

/// What a challenge shows in place of a scheme, a name or an item that may be part of a value.
pub const HIDDEN: &str = "?";

impl Challenge {
    /// Reads one challenge header value: `<scheme> name="value", name=value, …` (RFC 7235: a quoted value may hold
    /// commas and `\`-escaped characters), or `<scheme> <token68>`.
    #[must_use]
    pub fn read(value: &str) -> Self {
        let value = value.trim();
        let (scheme, rest) = value.split_once([' ', '\t']).unwrap_or((value, ""));
        let mut parameters = Vec::new();
        let mut error = None;
        for (name, value) in parameters_of(rest) {
            if name.eq_ignore_ascii_case("error") && is_plain(&value) {
                error = Some(value);
            }
            parameters.push(name);
        }
        Self {
            scheme: shown_name(scheme).to_owned(),
            parameters,
            error,
        }
    }
}

/// The `name=value` pairs of a challenge's parameter list, quoted values unquoted and unescaped. A name that isn't
/// [`is_name`]-shaped becomes [`HIDDEN`]; so does an item without a `=`, and a token68 (the whole list), with no
/// value.
fn parameters_of(text: &str) -> Vec<(String, String)> {
    let text = text.trim();
    if is_token68(text) {
        return vec![(HIDDEN.to_owned(), String::new())];
    }
    let mut pairs = Vec::new();
    let mut rest = text;
    loop {
        rest = rest.trim_start_matches([',', ' ', '\t']);
        if rest.is_empty() {
            return pairs;
        }
        let Some(equals) = rest.find(['=', ',']).filter(|&at| rest[at..].starts_with('=')) else {
            pairs.push((HIDDEN.to_owned(), String::new()));
            rest = rest.find(',').map_or("", |at| &rest[at..]);
            continue;
        };
        let name = shown_name(rest[..equals].trim()).to_owned();
        let (value, next) = value_at(rest[equals + 1..].trim_start());
        pairs.push((name, value));
        // anything between the value and the next comma is not a parameter of its own
        rest = next.find(',').map_or("", |at| &next[at..]);
    }
}

/// A parameter's value at the start of `text` and what follows it: a quoted string (its `\` escapes undone, ended by
/// the first unescaped `"`, or by the end of `text`) or the text up to the next comma.
fn value_at(text: &str) -> (String, &str) {
    let Some(quoted) = text.strip_prefix('"') else {
        let end = text.find(',').unwrap_or(text.len());
        return (text[..end].trim().to_owned(), &text[end..]);
    };
    let mut value = String::new();
    let mut chars = quoted.char_indices();
    while let Some((at, char)) = chars.next() {
        match char {
            '\\' => value.extend(chars.next().map(|(_, escaped)| escaped)),
            '"' => return (value, &quoted[at + 1..]),
            _ => value.push(char),
        }
    }
    (value, "")
}

/// `name` when it may be shown ([`is_name`]), [`HIDDEN`] otherwise.
fn shown_name(name: &str) -> &str {
    if is_name(name) { name } else { HIDDEN }
}

/// A scheme or parameter name that may be shown: short, letters, digits, `-`, `_` and `.` only (a subset of an RFC
/// 7230 token).
fn is_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

/// An RFC 7235 token68: letters, digits, `-`, `.`, `_`, `~`, `+`, `/`, then only `=` padding.
fn is_token68(text: &str) -> bool {
    let body = text.trim_end_matches('=');
    !body.is_empty()
        && body
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'+' | b'/'))
}

/// An OAuth error code: short, letters, digits and `_` only.
fn is_plain(value: &str) -> bool {
    (1..=64).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// An answer's header values and body: kept for `--save`, never shown (`Debug` gives only how much there is).
#[derive(Clone, PartialEq, Eq)]
struct Kept {
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl fmt::Debug for Kept {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Kept({} header value(s), {} body bytes)",
            self.headers.len(),
            self.body.len()
        )
    }
}

/// What a read got back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    pub status: u16,
    /// The answer's header names, lowercase, sorted, once each.
    pub header_names: Vec<String>,
    pub challenge: Option<Challenge>,
    /// A `Location`'s host (a region hop answers 404 with one), `"?"` when it has none; `None` without a `Location`.
    pub location_host: Option<String>,
    kept: Kept,
}

impl Answer {
    /// The answer's body, kept for a reference copy; never shown.
    pub(crate) fn body(&self) -> &[u8] {
        &self.kept.body
    }
}

/// Why a read got no answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadFailure {
    Timeout,
    Network,
    /// The body was longer than [`MAX_BODY`].
    TooLarge,
}

/// One conversation-list read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListRead {
    pub form: HeaderForm,
    pub outcome: Result<Answer, ReadFailure>,
}

/// What the probe found. Holds no token; the answers' header values and bodies only for [`Self::reference_events`].
/// `Debug` shows neither, nor the thread's member ids.
pub struct ChatProbe {
    pub alias: Alias,
    pub partner: Alias,
    /// The refreshed `ic3` token's claims; `None` for an opaque token.
    pub token: Option<TokenSummary>,
    /// The chat base, as the GTMs give it, trimmed and without a trailing slash.
    pub base: String,
    /// One read per [`HeaderForm::ALL`], in that order.
    pub conversations: Vec<ListRead>,
    /// The fence's live read of the 1:1 chat's thread.
    pub thread: Membership,
}

impl fmt::Debug for ChatProbe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChatProbe")
            .field("alias", &self.alias)
            .field("partner", &self.partner)
            .field("token", &self.token)
            .field("base", &self.base)
            .field("conversations", &self.conversations)
            .field("thread", &membership_line(&self.thread))
            .finish()
    }
}

/// Probes `request.alias`'s own `ic3` token against the chat service (see the module docs), reading the 1:1 chat's
/// thread through `source`, which gets the token's credential.
///
/// # Errors
/// [`ProbeError`] when nothing could be read: an account without an MRI, no usable chat base in the region record
/// (both checked before any request), a refused or failed refresh, a mock origin in a normal build. A read the chat
/// service refuses is in the report instead.
pub async fn probe_chat<S: SecretStore>(
    store: &S,
    tokens: &TokenClient,
    request: ProbeRequest<'_>,
    source: LiveMembershipSource,
) -> Result<ChatProbe, ProbeError> {
    if matches!(request.origin, Origin::Mock(_)) && !teams_fence::test_support_enabled() {
        return Err(ProbeError::MockOrigin);
    }
    let chat_id = chat_id(request.config, request.alias, request.partner)?;
    let base = chat_base(&read_region(request.config, request.alias)?)?;
    let account = &request.config.account(request.alias).upn;
    let ic3 = tokens
        .refresh(store, account, &Scope::for_resource(IC3_RESOURCE))
        .await?;
    let token = ic3.access_token.expose();
    let credential = ChatCredential::bearer(&base, token).ok_or(ProbeError::BadToken)?;

    let client = client(&request.origin)?;
    let list_url = format!("{base}/{CONVERSATIONS_PATH}?{CONVERSATIONS_QUERY}");
    let target = at_origin(&list_url, &request.origin).ok_or_else(|| ProbeError::NotAChatBase(base.clone()))?;
    let mut conversations = Vec::with_capacity(HeaderForm::ALL.len());
    for form in HeaderForm::ALL {
        let outcome = read(&client, target.clone(), token, form).await;
        conversations.push(ListRead { form, outcome });
    }

    source.set_credential(credential);
    let thread = source.members(&FenceScope::Conversation { id: chat_id }).await;
    Ok(ChatProbe {
        alias: request.alias,
        partner: request.partner,
        token: TokenSummary::read(&ic3.access_token),
        base,
        conversations,
        thread,
    })
}

/// The 1:1 chat of the two accounts, from their MRIs in lab.toml.
fn chat_id(config: &LabConfig, alias: Alias, partner: Alias) -> Result<String, ProbeError> {
    let mri = |alias: Alias| config.account(alias).mri.clone().ok_or(ProbeError::NoMri(alias));
    let (own, other) = (mri(alias)?, mri(partner)?);
    one_to_one_id(&own, &other).ok_or(ProbeError::NoChat(alias, partner))
}

pub(crate) fn read_region(config: &LabConfig, alias: Alias) -> Result<Region, ProbeError> {
    let file = region_file(config.paths(), alias);
    let shown = file.display().to_string();
    let text = std::fs::read_to_string(&file).map_err(|source| ProbeError::Region {
        path: shown.clone(),
        source,
    })?;
    serde_json::from_str::<Value>(&text)
        .ok()
        .as_ref()
        .and_then(Region::from_json)
        .ok_or(ProbeError::BadRegion(shown))
}

/// The chat base from the region's GTMs ([`CHAT_BASE_GTM`], else its fallback, trimmed: [`Region::base`]) without a
/// trailing slash; it must be a base the write mapping takes a chat service request under, with nothing after it.
pub(crate) fn chat_base(region: &Region) -> Result<String, ProbeError> {
    let trimmed = region.base(CHAT_BASE_GTM).ok_or(ProbeError::NoChatBase)?;
    let base = trimmed.strip_suffix('/').unwrap_or(trimmed);
    let refused = || ProbeError::NotAChatBase(base.to_owned());
    if base.contains(['?', '#']) {
        return Err(refused());
    }
    let probe = Url::parse(&format!("{base}/{CONVERSATIONS_PATH}")).map_err(|_| refused())?;
    match chat_service_path(&probe) {
        Some(path) if path.join("/") == CONVERSATIONS_PATH.trim_start_matches("v1/") => Ok(base.to_owned()),
        _ => Err(refused()),
    }
}

/// `url` as sent: as it is to the chat service, or with a mock's origin in place of its own.
pub(crate) fn at_origin(url: &str, origin: &Origin) -> Option<Url> {
    let url = Url::parse(url).ok()?;
    match origin {
        Origin::ChatService => Some(url),
        Origin::Mock(mock) => Url::parse(&format!(
            "{}{}?{}",
            mock.trim_end_matches('/'),
            url.path(),
            url.query().unwrap_or("")
        ))
        .ok(),
    }
}

/// The probe's HTTPS client: no proxy, no redirects, `https` only unless it serves a mock.
pub(crate) fn client(origin: &Origin) -> Result<reqwest::Client, ProbeError> {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .https_only(*origin == Origin::ChatService)
        .connect_timeout(READ_TIMEOUT)
        .timeout(READ_TIMEOUT)
        .build()
        .map_err(ProbeError::Client)
}

/// One `GET` of `url` with the token and the form's headers.
pub(crate) async fn read(
    client: &reqwest::Client,
    url: Url,
    token: &str,
    form: HeaderForm,
) -> Result<Answer, ReadFailure> {
    let mut builder = client
        .get(url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json");
    for (name, value) in form.headers() {
        builder = builder.header(name, value);
    }
    let mut response = builder.send().await.map_err(|error| failure(&error))?;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| failure(&error))? {
        if body.len() + chunk.len() > MAX_BODY {
            return Err(ReadFailure::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(answer(status, &headers, body))
}

fn answer(status: u16, headers: &HeaderMap, body: Vec<u8>) -> Answer {
    let mut header_names: Vec<String> = headers.keys().map(|name| name.as_str().to_owned()).collect();
    header_names.sort();
    header_names.dedup();
    let text = |value: &reqwest::header::HeaderValue| String::from_utf8_lossy(value.as_bytes()).into_owned();
    let challenge = headers.get(WWW_AUTHENTICATE).map(|value| Challenge::read(&text(value)));
    let location_host = headers.get(LOCATION).map(|value| {
        Url::parse(&text(value))
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .unwrap_or_else(|| "?".to_owned())
    });
    Answer {
        status,
        header_names,
        challenge,
        location_host,
        kept: Kept {
            headers: headers
                .iter()
                .map(|(name, value)| (name.as_str().to_owned(), text(value)))
                .collect(),
            body,
        },
    }
}

fn failure(error: &reqwest::Error) -> ReadFailure {
    if error.is_timeout() {
        ReadFailure::Timeout
    } else {
        ReadFailure::Network
    }
}

impl ChatProbe {
    /// The report as lines for the operator, as of `now` (see the module docs for what they show and never show).
    #[must_use]
    pub fn lines(&self, now: SystemTime) -> Vec<String> {
        let alias = self.alias;
        let mut lines = vec![
            format!("{alias}: ic3 token: {}", summary_line(self.token.as_ref(), now)),
            format!("{alias}: chat base {}", self.base),
        ];
        for read in &self.conversations {
            lines.push(format!(
                "{alias}: GET {CONVERSATIONS_PATH} ({}): {}",
                read.form,
                outcome_line(&read.outcome)
            ));
        }
        lines.push(format!(
            "{alias}: GET v1/threads/<the {alias}-{} 1:1 chat> (the fence's live read, token only): {}",
            self.partner,
            membership_line(&self.thread)
        ));
        lines
    }

    /// The conversation-list reads as capture-like events for a reference copy: per read its form, the URL as sent
    /// to the chat service (never a mock's), the request's header names, the status, the answer's headers and its
    /// body (JSON when it parses). The token is in none of them; the sanitizer still runs over everything.
    #[must_use]
    pub fn reference_events(&self) -> Vec<Value> {
        let url = format!("{}/{CONVERSATIONS_PATH}?{CONVERSATIONS_QUERY}", self.base);
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_millis());
        self.conversations
            .iter()
            .zip(1_u64..)
            .map(|(read, seq)| {
                let mut request_headers = vec!["Authorization", "Accept"];
                request_headers.extend(read.form.headers().into_iter().map(|(name, _)| name));
                let mut event = json!({
                    "seq": seq,
                    "ts_ms": ts_ms,
                    "type": "probe_read",
                    "form": read.form.to_string(),
                    "method": "GET",
                    "url": url,
                    "request_headers": request_headers,
                });
                match &read.outcome {
                    Ok(answer) => {
                        event["status"] = json!(answer.status);
                        event["response_headers"] = Value::Object(response_headers(answer));
                        event["body"] = body_value(&answer.kept.body);
                    }
                    Err(failure) => event["error"] = json!(format!("{failure:?}")),
                }
                event
            })
            .collect()
    }
}

pub(crate) fn response_headers(answer: &Answer) -> Map<String, Value> {
    let mut map = Map::new();
    for (name, value) in &answer.kept.headers {
        map.insert(name.clone(), json!(value));
    }
    map
}

pub(crate) fn body_value(body: &[u8]) -> Value {
    serde_json::from_slice(body).unwrap_or_else(|_| json!(String::from_utf8_lossy(body)))
}

pub(crate) fn outcome_line(outcome: &Result<Answer, ReadFailure>) -> String {
    let answer = match outcome {
        Ok(answer) => answer,
        Err(failure) => return format!("no answer ({failure:?})"),
    };
    let mut line = format!("{}; headers: {}", answer.status, answer.header_names.join(", "));
    if let Some(challenge) = &answer.challenge {
        let _ = write!(
            line,
            "; WWW-Authenticate: {} ({})",
            challenge.scheme,
            challenge.parameters.join(", ")
        );
        if let Some(error) = &challenge.error {
            let _ = write!(line, ", error {error}");
        }
    }
    if let Some(host) = &answer.location_host {
        let _ = write!(line, "; Location on {host} (not followed)");
    }
    line
}

pub(crate) fn membership_line(membership: &Membership) -> String {
    match membership {
        Membership::Complete(members) => {
            format!("complete: the {} members the chat id names", members.len())
        }
        Membership::Partial(reason) => format!("partial: {reason}"),
        Membership::OrgWide => "org-wide".to_owned(),
    }
}
