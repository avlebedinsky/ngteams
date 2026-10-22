//! The media service's (AMS) object creation, mapped once for an experiment (M2 Task 21,
//! `docs/backlog/ams-image-sending.md`): what [`crate::classify::Classifier::with_media_object_creation`] adds.
//!
//! The web client sends an image by first creating an object on the async gateway, naming the one conversation that
//! may read it (`docs/protocol/chat.md`, "Images"); the object's content and any later reader (`forward`) name the
//! object alone, and stay denied: nothing maps them. The creation is mapped only in the exact shape `ab-image`
//! captured on 2026-10-20 and 2026-10-21 (eleven tries over two runs, all alike), and only for the one conversation
//! the classifier is given (the a–b chat, from `lab.toml`): its target is [`WriteKind::MediaObject`] on that
//! conversation, judged by [`teams_fence::Fence::check`] as a message there would be (its members fetched fresh, all
//! lab accounts). [`object_creation`] takes a request only when all of it matches:
//! - `POST` to [`CREATION_URL`] exactly: the async gateway's host, no user info, no port but the default, no query,
//!   no fragment;
//! - only the headers the captures showed, each once ([`CREATION_HEADERS`]), with the values the captures showed for
//!   those that change what the request does ([`PINNED`], each of them there): `content-type`, the sharing mode, the
//!   migration flag and the test-user flag; `Origin`, when there, one of the web client's origins. The rest name
//!   nothing and change nothing: the account's own credential (`authorization`, to the host it goes to anyway), the
//!   answer's format (`accept`), the page and the browser (`Referer`, `User-Agent`, the `sec-ch-ua` hints, the
//!   client's version) and a correlation id (`ms-cv`); a paused request doesn't keep their values;
//! - a body read strictly ([`strict_json`]) that is an object of exactly the four captured keys, each in its captured
//!   form: `type` `pish/image`, `sharingMode` `Inline`, `filename` `MicrosoftTeams-image.png` (the client renames a
//!   pasted image), and `permissions` an object with one key, the given conversation, whose value is `["read"]`.
//!
//! Anything else, another host (the GTM's `*.asm.skype.com`, the web client's `/asm/…` and `/ams/…` routes), a
//! `permissions` naming another conversation or more than one, the `@:composer` key the bundle can add, another
//! `type` or sharing mode, stays [`crate::classify::Classification::Unknown`] and denied.

use serde_json::Value;
use teams_fence::{Scope, Target, WriteKind};
use url::Url;

use crate::arming::TEAMS_ORIGINS;
use crate::classify::strict_json;

/// The object creation's URL, exactly as captured (and as [`Url`] serializes it).
pub const CREATION_URL: &str = "https://us-prod.asyncgw.teams.microsoft.com/v1/objects/";

/// The headers a captured creation carried as the fence paused it, by name in any case: the eleven of 2026-10-20 and
/// 2026-10-21 had these, the first of each run with the three `sec-ch-ua` hints too. A header of any other name makes
/// a creation unknown.
pub const CREATION_HEADERS: [&str; 14] = [
    "Origin",
    "Referer",
    "User-Agent",
    "accept",
    "authorization",
    "content-type",
    "ms-cv",
    "sec-ch-ua",
    "sec-ch-ua-mobile",
    "sec-ch-ua-platform",
    "x-ams-post-sharing-mode",
    "x-ms-client-version",
    "x-ms-migration",
    "x-ms-test-user",
];

/// The headers a creation must carry, each with the one value every capture showed, by lower-cased name.
pub const PINNED: [(&str, &str); 4] = [
    ("content-type", "application/json"),
    ("x-ams-post-sharing-mode", "Inline"),
    ("x-ms-migration", "True"),
    ("x-ms-test-user", "False"),
];

/// The headers whose values [`object_creation`] reads, by lower-cased name: a paused request keeps these values
/// (`crate::fence_http::PausedRequest`), and of every other header only the name.
pub const HEADER_VALUES_READ: [&str; 5] = [
    "content-type",
    "origin",
    "x-ams-post-sharing-mode",
    "x-ms-migration",
    "x-ms-test-user",
];

/// The body's `type`: an image.
pub const OBJECT_TYPE: &str = "pish/image";

/// The body's `sharingMode`, as the header names it too.
pub const SHARING_MODE: &str = "Inline";

/// The name the web client gives a pasted image.
pub const FILE_NAME: &str = "MicrosoftTeams-image.png";

/// The keys of a creation's body, exactly.
const BODY_KEYS: [&str; 4] = ["filename", "permissions", "sharingMode", "type"];

/// The write a request is when it creates an object for `chat` in the captured shape (see the module docs):
/// [`WriteKind::MediaObject`] on that conversation; `None` for anything else.
#[must_use]
pub fn object_creation(
    method: &str,
    url: &Url,
    headers: &[(String, Option<String>)],
    body: Option<&[u8]>,
    chat: &str,
) -> Option<Target> {
    if method != "POST" || url.as_str() != CREATION_URL || !are_creation_headers(headers) {
        return None;
    }
    let body = strict_json(body?)?;
    let object = body.as_object()?;
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let shaped = keys == BODY_KEYS
        && object["type"] == OBJECT_TYPE
        && object["sharingMode"] == SHARING_MODE
        && object["filename"] == FILE_NAME;
    if !shaped || !readable_by(&object["permissions"], chat) {
        return None;
    }
    Some(Target::new(
        WriteKind::MediaObject,
        Scope::Conversation { id: chat.to_owned() },
    ))
}

/// Whether `permissions` is `{"<chat>": ["read"]}` exactly.
fn readable_by(permissions: &Value, chat: &str) -> bool {
    let Some(permissions) = permissions.as_object() else {
        return false;
    };
    permissions.len() == 1
        && permissions
            .get(chat)
            .is_some_and(|rights| rights.as_array().is_some_and(|rights| rights == &[Value::from("read")]))
}

/// Whether every header is one of [`CREATION_HEADERS`] (by name in any case), none twice; each of [`PINNED`] is there
/// with exactly its value; and `Origin`, if there, is one of the web client's origins. A header whose value isn't
/// known (`None`) has none of these values.
fn are_creation_headers(headers: &[(String, Option<String>)]) -> bool {
    let mut seen = Vec::new();
    for (name, _) in headers {
        let name = name.to_ascii_lowercase();
        if seen.contains(&name) || !CREATION_HEADERS.iter().any(|known| known.eq_ignore_ascii_case(&name)) {
            return false;
        }
        seen.push(name);
    }
    let value = |wanted: &str| {
        headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(wanted))
            .map(|(_, value)| value.as_deref())
    };
    PINNED.iter().all(|&(name, pinned)| value(name) == Some(Some(pinned)))
        && value("origin").is_none_or(|origin| origin.is_some_and(|origin| TEAMS_ORIGINS.contains(&origin)))
}
