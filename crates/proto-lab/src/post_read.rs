//! The reviewed **read-by-`POST`** list of the browser fence: requests the web client sends as a `POST` although they
//! only read, let through in their captured shape when nothing they name is outside the lab.
//!
//! It is a class of its own, neither own-state (it reads other accounts, not the signed-in one's session) nor a write
//! (nothing anyone else can notice changes): [`crate::classify::Classification::PostRead`], logged to `fence.log` as
//! allowed with `post_read` and the entry's kind. The user decided on 2026-10-21 to let the mention picker's people
//! lookups through, under the rule that a denied request may be loosened only when nothing it does or names leaves the
//! a–b–c lab: so the classifier gives the MRIs a lookup names, and the fence lets it through only when every one is a
//! lab account of `lab.toml`'s roster ([`teams_fence::Fence::lab_accounts_only`]); a lookup naming anyone else, an
//! approved service included, is denied as unknown.
//!
//! An entry matches only all of it ([`lookup`]): `POST`; one of the web client's two origins exactly ([`TEAMS_ORIGINS`],
//! no user info); the middle tier's path with a partition of a region and a number (`amer-03`) and the entry's last
//! segment, exactly; the query the client always sent, every pair once and nothing else (in any order); only the
//! headers the captures showed, with their `Content-Type` exactly; and a body, read strictly ([`strict_json`]), that is
//! a non-empty JSON list of strings, the MRIs to look up. Anything else stays unknown and denied. The list is asked only
//! after the write mapping ([`crate::classify`]): a write is judged only by the fence's check.
//!
//! An endpoint goes on the list only when its answer can name nobody but whom the request names: a profile lookup
//! answers with the profiles of exactly the MRIs it was given, and those must all be lab accounts.
//!
//! Not on the list, and denied: the Substrate people search the picker may also call (its answers list other people
//! of the tenant, so it reads outside the lab; the user decides that one), and the bot invokes an opening editor sends
//! (`composeExtension/fetchCommands`, whose body hands the account's Entra token to the app:
//! `docs/backlog/bot-invokes-and-message-extensions.md`).

use serde_json::Value;
use teams_fence::Mri;
use url::Url;

use crate::arming::TEAMS_ORIGINS;
use crate::classify::strict_json;

/// A reviewed kind of read by `POST`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PostReadKind {
    /// The middle tier's short profiles (`users/fetchShortProfile`): the names and ids the client shows for people.
    ShortProfile,
    /// The middle tier's full profiles (`users/fetch`).
    UserFetch,
}

impl PostReadKind {
    /// The name used in `fence.log`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ShortProfile => "short_profile",
            Self::UserFetch => "user_fetch",
        }
    }
}

/// One entry of the list: the last segment of its middle-tier path (`/api/mt/part/<partition>/beta/users/<name>`),
/// and why it is on the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PostRead {
    pub kind: PostReadKind,
    /// The path's last segment, exactly.
    pub name: &'static str,
    /// Why it may pass.
    pub reason: &'static str,
}

/// The query every captured profile lookup sent (ab-chat-full, ab-chat and load captures of 2026-10-20), each pair
/// exactly. `isMailAddress=true` (a lookup by address) was seen once and isn't on the list.
const LOOKUP_QUERY: [(&str, &str); 6] = [
    ("isMailAddress", "false"),
    ("enableGuest", "true"),
    ("skypeTeamsInfo", "true"),
    ("canBeSmtpAddress", "false"),
    ("includeIBBarredUsers", "true"),
    ("includeDisabledAccounts", "true"),
];

/// The list (see the module docs).
pub const POST_READS: &[PostRead] = &[
    PostRead {
        kind: PostReadKind::ShortProfile,
        name: "fetchShortProfile",
        reason: "reads the names and ids of the people it names and changes nothing: the mention picker needs them to \
                 offer the chat's other member, who otherwise shows as \"Unknown User\" (ab-chat-full, 2026-10-20). \
                 Allowed by the user on 2026-10-21, only for lab accounts",
    },
    PostRead {
        kind: PostReadKind::UserFetch,
        name: "fetch",
        reason: "reads the profiles of the people it names and changes nothing: the client resolves the chat's \
                 members with it beside the short profiles. Allowed by the user on 2026-10-21, only for lab accounts",
    },
];

/// The headers every captured profile lookup carried as the fence paused it (the denied lookups of 2026-10-20), by name
/// in any case; a header of any other name makes a lookup unknown. Their values play no part but `Content-Type`'s
/// ([`LOOKUP_CONTENT_TYPE`]) and `Origin`'s (the request's own origin, when it is there): the client's own description,
/// the account's own bearer token and cookies for the middle tier, and the page it came from.
const LOOKUP_HEADERS: [&str; 17] = [
    "Accept",
    "Cookie",
    "Origin",
    "Referer",
    "User-Agent",
    "authorization",
    "content-type",
    "x-client-ui-language",
    "x-ms-client-caller",
    "x-ms-client-request-type",
    "x-ms-client-type",
    "x-ms-client-version",
    "x-ms-migration",
    "x-ms-object-id",
    "x-ms-request-id",
    "x-ms-session-id",
    "x-ms-test-user",
];

/// The `Content-Type` every captured profile lookup sent, exactly: the body is read as that JSON.
const LOOKUP_CONTENT_TYPE: &str = "application/json;charset=UTF-8";

/// What a mutating request is to the list: its kind and the MRIs its body names, or `None` when it matches no entry
/// exactly (see the module docs). The classifier asks only once the write mapping didn't take the request.
#[must_use]
pub fn lookup(
    method: &str,
    url: &Url,
    headers: &[(String, String)],
    body: Option<&[u8]>,
) -> Option<(PostReadKind, Vec<Mri>)> {
    let teams = TEAMS_ORIGINS.contains(&url.origin().ascii_serialization().as_str());
    if method != "POST" || !teams || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let name = middle_tier_users(url.path())?;
    let entry = POST_READS.iter().find(|entry| entry.name == name)?;
    if !is_lookup_query(url) || !are_lookup_headers(url, headers) {
        return None;
    }
    Some((entry.kind, mri_list(body?)?))
}

/// Whether every header is one of [`LOOKUP_HEADERS`] (by name in any case), none twice, `Content-Type` is there with
/// exactly [`LOOKUP_CONTENT_TYPE`], and `Origin`, if there, is `url`'s own origin.
fn are_lookup_headers(url: &Url, headers: &[(String, String)]) -> bool {
    let mut seen = Vec::new();
    for (name, _) in headers {
        let name = name.to_ascii_lowercase();
        if seen.contains(&name) || !LOOKUP_HEADERS.iter().any(|known| known.eq_ignore_ascii_case(&name)) {
            return false;
        }
        seen.push(name);
    }
    let value = |wanted: &str| {
        headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(wanted))
            .map(|(_, value)| value.as_str())
    };
    value("content-type") == Some(LOOKUP_CONTENT_TYPE)
        && value("origin").is_none_or(|origin| origin == url.origin().ascii_serialization())
}

/// The last segment of `/api/mt/part/<partition>/beta/users/<name>`, the partition a region and a number (`amer-03`).
fn middle_tier_users(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/api/mt/part/")?;
    let (partition, rest) = rest.split_once('/')?;
    let name = rest.strip_prefix("beta/users/")?;
    (is_partition(partition) && !name.is_empty() && !name.contains('/')).then_some(name)
}

/// A middle-tier partition: a lowercase region, `-`, a number (`amer-03`).
pub(crate) fn is_partition(partition: &str) -> bool {
    partition.split_once('-').is_some_and(|(region, number)| {
        !region.is_empty()
            && region.bytes().all(|byte| byte.is_ascii_lowercase())
            && !number.is_empty()
            && number.bytes().all(|byte| byte.is_ascii_digit())
    })
}

/// Whether the query is [`LOOKUP_QUERY`]: each of its pairs once, nothing else, in any order.
fn is_lookup_query(url: &Url) -> bool {
    let mut pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(key, value)| (key.into(), value.into()))
        .collect();
    let mut wanted: Vec<(String, String)> = LOOKUP_QUERY
        .iter()
        .map(|&(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
    pairs.sort();
    wanted.sort();
    pairs == wanted
}

/// A body read by [`strict_json`] that is a non-empty JSON list of strings: the MRIs, as given.
fn mri_list(body: &[u8]) -> Option<Vec<Mri>> {
    let Value::Array(names) = strict_json(body)? else {
        return None;
    };
    let names: Vec<Mri> = names
        .iter()
        .map(|name| name.as_str().map(Mri::new))
        .collect::<Option<_>>()?;
    (!names.is_empty()).then_some(names)
}
