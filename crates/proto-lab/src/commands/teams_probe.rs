//! `proto-lab probe teams <alias>`: does CSA give the account's teams and channels to its own `chatsvcagg` token, and
//! how does the chat service give a channel's posts and their reply chains? Read-only (M2 Task 16).
//!
//! The lab accounts are members of teams of the tenant that people outside the lab are in too, so nothing of an answer
//! is shown: no team, channel or person name, no message text and no id.
//!
//! 1. The account's `chatsvcagg` and `ic3` access tokens are refreshed from its stored refresh token
//!    ([`TokenClient::refresh`], audiences [`CSA_RESOURCE`] and [`IC3_RESOURCE`]); nothing else is asked of Entra.
//! 2. The CSA base is the account's `chatSvcAggAfd` GTM ([`CSA_GTM`]), trimmed, from the region record `proto-lab auth`
//!    saved; it must be `https://<AFD host>/api/csa/<region>` exactly ([`csa_base`]). The chat base is the probe
//!    chat's ([`probe::chat_base`]). Both are checked before any request is sent.
//! 3. Only `GET`s, each with the token and `Accept: application/json`: CSA's teams and channels ([`TEAMS_PATH`] with
//!    [`TEAMS_QUERY`], the web client's query, token only), then on the chat service (with the native transport's
//!    `X-MS-Migration` and `BehaviorOverride`) the conversation list, and for up to [`CHANNELS_READ`] channels of the
//!    first team (its general channel first) the channel's thread, the newest page of its messages and, for one post
//!    of it, that post's reply chain (`<channel>;messageid=<post>`).
//! 4. The report ([`TeamsProbe::lines`]) shows per read its status, its header names, the answer's `Content-Type` and
//!    size, its JSON shape (every key path with the JSON types seen there, arrays' elements merged: [`shape_lines`])
//!    and counts; besides, the values of two protocol enums when they are plain words (a thread's `threadType`, a
//!    message's `messagetype`), and the chatsvcagg and ic3 tokens' claims (aud, exp, scopes, tenant). Never a token, a
//!    name, a text or an id: a key that might be one is shown as `?`.
//!
//! A test reaches a local mock instead of the services only through [`Origin::Mock`], which a build without
//! `teams-fence`'s `test-support` feature refuses (and the `proto-lab` binary refuses to run in such a build).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::SystemTime;

use serde_json::Value;
use teams_auth::authz::Region;
use teams_auth::claims::TokenSummary;
use teams_auth::secrets::SecretStore;
use teams_auth::token::{Scope, TokenClient};
use teams_fence::AFD_HOSTS;
use url::Url;

use super::auth::summary_line;
use super::probe::{
    self, Answer, CONVERSATIONS_PATH, CONVERSATIONS_QUERY, HeaderForm, IC3_RESOURCE, Origin, ProbeError, ReadFailure,
    at_origin, client, outcome_line, read,
};
use super::profile_probe::{json_type, plain_content_type};
use crate::config::{Alias, LabConfig};

/// The audience of CSA's token.
pub const CSA_RESOURCE: &str = "https://chatsvcagg.teams.microsoft.com";

/// The GTM CSA's base comes from.
pub const CSA_GTM: &str = "chatSvcAggAfd";

/// CSA's read of the account's teams (with their channels) and chats, relative to the CSA base.
pub const TEAMS_PATH: &str = "api/v3/teams/users/me";

/// Its query as the web client sent it in every capture of the full read (`ab-chat`, `abc-chat`, `load`).
pub const TEAMS_QUERY: &str = "isPrefetch=false&enableMembershipSummary=true&supportsAdditionalSystemGeneratedFolders=true&supportsSliceItems=true&enableEngageCommunities=false";

/// The query of a channel's messages, as the native client reads a conversation's newest page.
pub const MESSAGES_QUERY: &str = "view=msnp24Equivalent%7CsupportsMessageProperties&pageSize=200";

/// The query of a channel's thread, as the native client reads one.
pub const THREAD_QUERY: &str = "view=msnp24Equivalent";

/// How many channels of the first team are read.
pub const CHANNELS_READ: usize = 2;

/// How deep [`shape_lines`] goes into an answer.
pub const SHAPE_DEPTH: usize = 5;

/// The chat service's form: the native transport's headers.
const CHAT_FORM: HeaderForm = HeaderForm {
    migration: true,
    behavior_override: true,
};

/// CSA's form: the token and `Accept` only.
const PLAIN: HeaderForm = HeaderForm {
    migration: false,
    behavior_override: false,
};

/// Which read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TeamsReadKind {
    /// CSA's `api/v3/teams/users/me`.
    Teams,
    /// The chat service's conversation list.
    Conversations,
    /// A channel's thread.
    Thread,
    /// A channel's newest messages.
    Messages,
    /// One post's reply chain.
    ReplyChain,
}

impl TeamsReadKind {
    /// How the report names it (no id).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Teams => "CSA api/v3/teams/users/me",
            Self::Conversations => "chat service v1/users/ME/conversations",
            Self::Thread => "chat service v1/threads/<channel>",
            Self::Messages => "chat service v1/users/ME/conversations/<channel>/messages",
            Self::ReplyChain => "chat service v1/users/ME/conversations/<channel>;messageid=<post>/messages",
        }
    }
}

/// One read: which, of which channel (its place among those read), and what came back. `Debug` leaves the body out.
#[derive(Clone, Debug)]
pub struct TeamsRead {
    pub kind: TeamsReadKind,
    /// The channel's place among those read (0: the team's general channel when it has one), for a channel's reads.
    pub channel: Option<usize>,
    pub outcome: Result<Answer, ReadFailure>,
}

/// What the probe found. Holds no token and shows no answer's value.
pub struct TeamsProbe {
    pub alias: Alias,
    /// The refreshed `chatsvcagg` token's claims; `None` for an opaque token.
    pub csa_token: Option<TokenSummary>,
    /// The refreshed `ic3` token's claims.
    pub ic3_token: Option<TokenSummary>,
    pub reads: Vec<TeamsRead>,
}

impl fmt::Debug for TeamsProbe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TeamsProbe")
            .field("alias", &self.alias)
            .field("reads", &self.reads.len())
            .finish_non_exhaustive()
    }
}

/// The CSA base from the region's GTMs ([`CSA_GTM`], trimmed) without a trailing slash: `https://<AFD host>/api/csa/
/// <region>` exactly, the region lowercase letters.
///
/// # Errors
/// [`ProbeError::NoCsaBase`] without one, [`ProbeError::NotACsaBase`] for any other form.
pub fn csa_base(region: &Region) -> Result<String, ProbeError> {
    let trimmed = region.base(CSA_GTM).ok_or(ProbeError::NoCsaBase)?;
    let base = trimmed.strip_suffix('/').unwrap_or(trimmed);
    let refused = || ProbeError::NotACsaBase(base.to_owned());
    let url = Url::parse(base).map_err(|_| refused())?;
    let segments: Vec<&str> = url.path_segments().map(Iterator::collect).unwrap_or_default();
    let plain = url.scheme() == "https"
        && url.host_str().is_some_and(|host| AFD_HOSTS.contains(&host))
        && url.port().is_none()
        && url.username().is_empty()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.as_str() == base;
    let path = matches!(
        segments.as_slice(),
        ["api", "csa", region] if !region.is_empty() && region.bytes().all(|byte| byte.is_ascii_lowercase())
    );
    if plain && path {
        Ok(base.to_owned())
    } else {
        Err(refused())
    }
}

/// Probes CSA's teams read and the chat service's channel reads as `alias` (see the module docs).
///
/// # Errors
/// [`ProbeError`] when nothing could be read: no usable base in the region record (checked before any request), a
/// refused or failed refresh, a mock origin in a normal build. A read a service refuses is in the report instead.
pub async fn probe_teams<S: SecretStore>(
    store: &S,
    tokens: &TokenClient,
    config: &LabConfig,
    alias: Alias,
    origin: &Origin,
) -> Result<TeamsProbe, ProbeError> {
    if matches!(origin, Origin::Mock(_)) && !teams_fence::test_support_enabled() {
        return Err(ProbeError::MockOrigin);
    }
    let region = probe::read_region(config, alias)?;
    let (csa, chat) = (csa_base(&region)?, probe::chat_base(&region)?);
    let account = &config.account(alias).upn;
    let csa_token = tokens
        .refresh(store, account, &Scope::for_resource(CSA_RESOURCE))
        .await?;
    let ic3 = tokens
        .refresh(store, account, &Scope::for_resource(IC3_RESOURCE))
        .await?;
    let reader = Reader {
        client: client(origin)?,
        origin,
    };
    let mut reads = Vec::new();
    let teams_url = format!("{csa}/{TEAMS_PATH}?{TEAMS_QUERY}");
    let teams = reader.get(&teams_url, csa_token.access_token.expose(), PLAIN).await;
    let channels = teams.as_ref().map(first_team_channels).unwrap_or_default();
    reads.push(TeamsRead {
        kind: TeamsReadKind::Teams,
        channel: None,
        outcome: teams,
    });
    let chat_reads = ChatReads {
        reader: &reader,
        base: &chat,
        token: ic3.access_token.expose(),
    };
    chat_reads.read_all(&channels, &mut reads).await;
    Ok(TeamsProbe {
        alias,
        csa_token: TokenSummary::read(&csa_token.access_token),
        ic3_token: TokenSummary::read(&ic3.access_token),
        reads,
    })
}

/// Sends the probe's `GET`s.
struct Reader<'a> {
    client: reqwest::Client,
    origin: &'a Origin,
}

impl Reader<'_> {
    async fn get(&self, url: &str, token: &str, form: HeaderForm) -> Result<Answer, ReadFailure> {
        let Some(target) = at_origin(url, self.origin) else {
            return Err(ReadFailure::Network);
        };
        read(&self.client, target, token, form).await
    }
}

/// The chat service's reads of the probe.
struct ChatReads<'a> {
    reader: &'a Reader<'a>,
    base: &'a str,
    token: &'a str,
}

impl ChatReads<'_> {
    async fn read_all(&self, channels: &[String], reads: &mut Vec<TeamsRead>) {
        let list_url = format!("{}/{CONVERSATIONS_PATH}?{CONVERSATIONS_QUERY}", self.base);
        reads.push(TeamsRead {
            kind: TeamsReadKind::Conversations,
            channel: None,
            outcome: self.reader.get(&list_url, self.token, CHAT_FORM).await,
        });
        for (place, channel) in channels.iter().enumerate() {
            self.read_channel(place, channel, reads).await;
        }
    }

    async fn read_channel(&self, place: usize, channel: &str, reads: &mut Vec<TeamsRead>) {
        let thread_url = self.url(&["v1", "threads", channel], THREAD_QUERY);
        let thread = self.reader.get(&thread_url, self.token, CHAT_FORM).await;
        reads.push(TeamsRead {
            kind: TeamsReadKind::Thread,
            channel: Some(place),
            outcome: thread,
        });
        let messages_url = self.url(
            &["v1", "users", "ME", "conversations", channel, "messages"],
            MESSAGES_QUERY,
        );
        let messages = self.reader.get(&messages_url, self.token, CHAT_FORM).await;
        let post = messages.as_ref().ok().and_then(|answer| a_post(answer, channel));
        reads.push(TeamsRead {
            kind: TeamsReadKind::Messages,
            channel: Some(place),
            outcome: messages,
        });
        let Some(post) = post else {
            return;
        };
        let chain = format!("{channel};messageid={post}");
        let chain_url = self.url(
            &["v1", "users", "ME", "conversations", &chain, "messages"],
            MESSAGES_QUERY,
        );
        reads.push(TeamsRead {
            kind: TeamsReadKind::ReplyChain,
            channel: Some(place),
            outcome: self.reader.get(&chain_url, self.token, CHAT_FORM).await,
        });
    }

    /// `segments` under the chat base, each one encoded segment, with `query` as written.
    fn url(&self, segments: &[&str], query: &str) -> String {
        let Ok(mut url) = Url::parse(self.base) else {
            return String::new();
        };
        if let Ok(mut path) = url.path_segments_mut() {
            path.pop_if_empty().extend(segments);
        }
        url.set_query(Some(query));
        url.to_string()
    }
}

/// The ids of up to [`CHANNELS_READ`] channels of the answer's first team: its general channel (the one whose id is
/// the team's) first.
fn first_team_channels(answer: &Answer) -> Vec<String> {
    let Ok(value) = serde_json::from_slice::<Value>(answer.body()) else {
        return Vec::new();
    };
    let Some(team) = value
        .get("teams")
        .and_then(Value::as_array)
        .and_then(|teams| teams.first())
    else {
        return Vec::new();
    };
    let team_id = team.get("id").and_then(Value::as_str).unwrap_or_default();
    let mut ids: Vec<String> = team
        .get("channels")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|channel| channel.get("id").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    ids.sort_by_key(|id| id != team_id);
    ids.truncate(CHANNELS_READ);
    ids
}

/// A post of a channel's message page whose reply chain is read: the root a reply names (`…;messageid=<post>` in its
/// `conversationLink` or `conversationid`), else the newest message a person wrote.
fn a_post(answer: &Answer, channel: &str) -> Option<String> {
    let value: Value = serde_json::from_slice(answer.body()).ok()?;
    let messages = value.get("messages")?.as_array()?;
    let named = messages.iter().find_map(|message| {
        ["conversationLink", "conversationid"]
            .iter()
            .filter_map(|key| message.get(*key).and_then(Value::as_str))
            .find_map(|link| link.split_once(";messageid=").map(|(_, post)| post.to_owned()))
    });
    named.or_else(|| {
        messages
            .iter()
            .filter(|message| {
                message.get("conversationid").and_then(Value::as_str) == Some(channel)
                    && matches!(
                        message.get("messagetype").and_then(Value::as_str),
                        Some("RichText/Html" | "Text")
                    )
            })
            .find_map(|message| message.get("id").and_then(Value::as_str).map(str::to_owned))
    })
}

impl TeamsProbe {
    /// The report as lines for the operator, as of `now` (see the module docs for what they show and never show).
    #[must_use]
    pub fn lines(&self, now: SystemTime) -> Vec<String> {
        let alias = self.alias;
        let mut lines = vec![
            format!(
                "{alias}: chatsvcagg token: {}",
                summary_line(self.csa_token.as_ref(), now)
            ),
            format!("{alias}: ic3 token: {}", summary_line(self.ic3_token.as_ref(), now)),
        ];
        let listed = self.listed_ids();
        let teams = self.reads.iter().find(|read| read.kind == TeamsReadKind::Teams);
        for read in &self.reads {
            let channel = read
                .channel
                .map(|place| format!(" (channel {place})"))
                .unwrap_or_default();
            lines.push(format!(
                "{alias}: GET {}{channel}: {}",
                read.kind.as_str(),
                outcome_line(&read.outcome)
            ));
            let Ok(answer) = &read.outcome else {
                continue;
            };
            lines.push(format!("{alias}:   {}", size_line(answer)));
            let value = serde_json::from_slice::<Value>(answer.body()).ok();
            if let Some(value) = &value {
                lines.extend(
                    facts(read.kind, value)
                        .into_iter()
                        .map(|fact| format!("{alias}:   {fact}")),
                );
                lines.extend(
                    shape_lines(value, SHAPE_DEPTH)
                        .into_iter()
                        .map(|line| format!("{alias}:     {line}")),
                );
            }
        }
        if let Some(Ok(answer)) = teams.map(|read| &read.outcome) {
            lines.extend(
                listed_line(answer, &listed)
                    .into_iter()
                    .map(|line| format!("{alias}: {line}")),
            );
        }
        lines
    }

    /// The conversation ids the chat service's list holds.
    fn listed_ids(&self) -> BTreeSet<String> {
        self.reads
            .iter()
            .filter(|read| read.kind == TeamsReadKind::Conversations)
            .filter_map(|read| read.outcome.as_ref().ok())
            .filter_map(|answer| serde_json::from_slice::<Value>(answer.body()).ok())
            .flat_map(|value| {
                value
                    .get("conversations")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            })
            .filter_map(|conversation| conversation.get("id").and_then(Value::as_str).map(str::to_owned))
            .collect()
    }
}

/// An answer's `Content-Type` and size.
fn size_line(answer: &Answer) -> String {
    let content_type = answer.header("content-type").map_or("none", plain_content_type);
    format!("content-type {content_type}; {} bytes", answer.body().len())
}

/// How many of CSA's teams and channels the chat service's list holds.
fn listed_line(answer: &Answer, listed: &BTreeSet<String>) -> Option<String> {
    let value: Value = serde_json::from_slice(answer.body()).ok()?;
    let teams = value.get("teams")?.as_array()?;
    let team_ids: Vec<&str> = teams
        .iter()
        .filter_map(|team| team.get("id").and_then(Value::as_str))
        .collect();
    let channel_ids: Vec<&str> = teams
        .iter()
        .flat_map(|team| team.get("channels").and_then(Value::as_array).into_iter().flatten())
        .filter_map(|channel| channel.get("id").and_then(Value::as_str))
        .collect();
    let count = |ids: &[&str]| ids.iter().filter(|id| listed.contains(**id)).count();
    Some(format!(
        "in the chat service's list: {} of {} teams, {} of {} channels",
        count(&team_ids),
        team_ids.len(),
        count(&channel_ids),
        channel_ids.len()
    ))
}

/// What a read's answer shows besides its shape: counts, and two protocol enums' plain values.
fn facts(kind: TeamsReadKind, value: &Value) -> Vec<String> {
    match kind {
        TeamsReadKind::Teams => team_facts(value),
        TeamsReadKind::Conversations => vec![format!(
            "conversations: {}",
            value.get("conversations").and_then(Value::as_array).map_or(0, Vec::len)
        )],
        TeamsReadKind::Thread => vec![
            format!(
                "threadType {}",
                plain_word(value.pointer("/properties/threadType").and_then(Value::as_str))
            ),
            format!(
                "productThreadType {}",
                plain_word(value.pointer("/properties/productThreadType").and_then(Value::as_str))
            ),
            format!(
                "members: {}",
                value.get("members").and_then(Value::as_array).map_or(0, Vec::len)
            ),
        ],
        TeamsReadKind::Messages | TeamsReadKind::ReplyChain => message_facts(value),
    }
}

fn team_facts(value: &Value) -> Vec<String> {
    let teams = value
        .get("teams")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut channels = 0;
    let mut general = 0;
    let mut flags: BTreeMap<String, usize> = BTreeMap::new();
    for team in &teams {
        let team_id = team.get("id").and_then(Value::as_str);
        for channel in team.get("channels").and_then(Value::as_array).into_iter().flatten() {
            channels += 1;
            if channel.get("id").and_then(Value::as_str) == team_id {
                general += 1;
            }
            count_true_flags(channel, "channel", &mut flags);
        }
        count_true_flags(team, "team", &mut flags);
    }
    let mut lines = vec![format!(
        "teams: {}; channels: {channels}, of them {general} with the team's id",
        teams.len()
    )];
    lines.extend(flags.into_iter().map(|(flag, count)| format!("{flag} true: {count}")));
    lines
}

/// Counts an object's boolean keys that are `true`, as `<what>.<key>`.
fn count_true_flags(object: &Value, what: &str, flags: &mut BTreeMap<String, usize>) {
    for (key, value) in object.as_object().into_iter().flatten() {
        if value.as_bool() == Some(true) {
            *flags.entry(format!("{what}.{}", shown_key(key))).or_default() += 1;
        }
    }
}

fn message_facts(value: &Value) -> Vec<String> {
    let messages = value
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut types: BTreeMap<String, usize> = BTreeMap::new();
    let (mut chained_ids, mut chained_links, mut parents) = (0, 0, 0);
    for message in &messages {
        let text = |key: &str| message.get(key).and_then(Value::as_str).unwrap_or_default();
        *types
            .entry(plain_word(message.get("messagetype").and_then(Value::as_str)))
            .or_default() += 1;
        chained_ids += usize::from(text("conversationid").contains(";messageid="));
        chained_links += usize::from(text("conversationLink").contains(";messageid="));
        parents += usize::from(message.pointer("/properties/parentmessageid").is_some());
    }
    let types: Vec<String> = types
        .into_iter()
        .map(|(kind, count)| format!("{kind} {count}"))
        .collect();
    vec![
        format!("messages: {}", messages.len()),
        format!("messagetype: {}", types.join(", ")),
        format!("conversationid with ;messageid=: {chained_ids}; conversationLink with ;messageid=: {chained_links}"),
        format!("properties.parentmessageid: {parents}"),
    ]
}

/// A protocol enum's value when it is a plain word (`RichText/Html`, `space`), `?` otherwise, `none` without one.
fn plain_word(value: Option<&str>) -> String {
    match value {
        None => "none".to_owned(),
        Some(word)
            if (1..=40).contains(&word.len())
                && word.bytes().next().is_some_and(|byte| byte.is_ascii_alphabetic())
                && word.bytes().all(|byte| byte.is_ascii_alphabetic() || byte == b'/') =>
        {
            word.to_owned()
        }
        Some(_) => "?".to_owned(),
    }
}

/// An answer's JSON shape: every key path (arrays as `[]`, their elements merged) with the JSON types seen there,
/// down to `depth` levels; a key that might be a value (an id, a name) is `?`.
#[must_use]
pub fn shape_lines(value: &Value, depth: usize) -> Vec<String> {
    let mut paths: BTreeMap<String, BTreeSet<&'static str>> = BTreeMap::new();
    collect_shape(value, "", depth, &mut paths);
    paths
        .into_iter()
        .map(|(path, types)| {
            let types: Vec<&str> = types.into_iter().collect();
            format!("{}: {}", if path.is_empty() { "." } else { &path }, types.join("|"))
        })
        .collect()
}

fn collect_shape(value: &Value, path: &str, depth: usize, paths: &mut BTreeMap<String, BTreeSet<&'static str>>) {
    paths.entry(path.to_owned()).or_default().insert(json_type(value));
    if depth == 0 {
        return;
    }
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                let child_path = if path.is_empty() {
                    shown_key(key).to_owned()
                } else {
                    format!("{path}.{}", shown_key(key))
                };
                collect_shape(child, &child_path, depth - 1, paths);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_shape(item, &format!("{path}[]"), depth - 1, paths);
            }
        }
        _ => {}
    }
}

/// A key that may be shown: letters, digits, `@`, `.`, `_`, `$` and `-`, short, with no run of four digits or of eight
/// hex digits with a digit among them (an id's); `?` otherwise.
fn shown_key(key: &str) -> &str {
    let plain = (1..=48).contains(&key.len())
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'@' | b'.' | b'_' | b'-' | b'$'));
    let digits = key.split(|char: char| !char.is_ascii_digit()).any(|run| run.len() >= 4);
    let hex = key
        .split(|char: char| !char.is_ascii_hexdigit())
        .any(|run| run.len() >= 8 && run.bytes().any(|byte| byte.is_ascii_digit()));
    if plain && !digits && !hex { key } else { "?" }
}
