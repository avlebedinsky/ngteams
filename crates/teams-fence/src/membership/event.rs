//! A calendar event's members, read fresh from the middle tier: the event's organizer and every attendee, each by
//! mail address ([`Mri::mail`]), for a write on an existing event ([`crate::Scope::Event`]: a change, a cancellation,
//! a delete).
//!
//! The read is the web client's own read of one event, `GET <middle tier base>/v2.0/me/calendars/events/<id>
//! ?shouldDecryptData=true`, with the account's token for the middle tier (`https://api.spaces.skype.com`): a
//! [`MiddleTierCredential`], taken from a request the armed lab page sent to the middle tier
//! ([`MiddleTierCredential::from_request`]) or, for a native client, from its [`super::CredentialSource`]
//! ([`MiddleTierCredential::bearer`]). The answer is [`Membership::Complete`] only when it is that event (its
//! `objectId` the id read), names its organizer's address and lists its attendees, each with an address; anything
//! else is partial.

use std::fmt;

use serde_json::Value;
use url::Url;
use zeroize::Zeroizing;

use super::{AFD_HOSTS, MAX_BODY, failure, is_token};
use crate::decision::{FetchFailure, Membership, PartialReason};
use crate::target::Mri;

/// The middle tier's base and the account's token for it, to read an event with. `Debug` never shows the token, and
/// it is wiped when dropped.
#[derive(Clone)]
pub struct MiddleTierCredential {
    /// `https://<AFD host>/api/mt/part/<partition>`, no trailing slash.
    base: Url,
    token: Zeroizing<String>,
}

impl fmt::Debug for MiddleTierCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MiddleTierCredential")
            .field("base", &self.base.as_str())
            .field("token", &"<redacted>")
            .finish()
    }
}

impl MiddleTierCredential {
    /// The credential of one request the page sent to the middle tier: `https` on an AFD host ([`AFD_HOSTS`]), a path
    /// under `/api/mt/part/<partition>/`, and an Entra token as `Authorization: Bearer <token>` (header name and scheme
    /// word in any case). `None` for any other request.
    pub fn from_request<'a>(url: &str, headers: impl IntoIterator<Item = (&'a str, &'a str)>) -> Option<Self> {
        let base = base_of(&Url::parse(url).ok()?)?;
        let token = headers.into_iter().find_map(|(name, value)| {
            if !name.eq_ignore_ascii_case("authorization") {
                return None;
            }
            let (word, token) = value.trim().split_once(' ')?;
            word.eq_ignore_ascii_case("bearer").then_some(token)
        })?;
        is_token(token).then(|| Self {
            base,
            token: Zeroizing::new(token.to_owned()),
        })
    }

    /// The credential of a native client: its middle-tier base as the region's GTM gives it (`https://<AFD
    /// host>/api/mt/part/<partition>`, one trailing slash allowed, nothing else) and its token for the middle tier.
    #[must_use]
    pub fn bearer(base: &str, token: &str) -> Option<Self> {
        if base.contains(['?', '#']) || base.trim() != base {
            return None;
        }
        let parsed = Url::parse(&format!("{}/", base.strip_suffix('/').unwrap_or(base))).ok()?;
        // the base's four segments and the empty one after its slash, nothing more
        let whole = parsed.path_segments()?.count() == 5;
        let base = base_of(&parsed)?;
        (whole && is_token(token)).then(|| Self {
            base,
            token: Zeroizing::new(token.to_owned()),
        })
    }

    /// The token, for the source that gave it to tell its session which token was refused. Never logged.
    #[must_use]
    pub fn expose_token(&self) -> &str {
        &self.token
    }

    /// The URL of the event `id`'s read on `origin` (the base's, or a test's mock).
    pub(super) fn event_url(&self, origin: &str, id: &str) -> Option<Url> {
        let mut url = Url::parse(origin).ok()?;
        let prefix: Vec<String> = self.base.path_segments()?.map(str::to_owned).collect();
        url.path_segments_mut()
            .ok()?
            .clear()
            .extend(prefix)
            .extend(["v2.0", "me", "calendars", "events", id]);
        url.set_query(Some("shouldDecryptData=true"));
        Some(url)
    }

    /// `https://<host>` of the base.
    pub(super) fn origin(&self) -> String {
        format!("https://{}", self.base.host_str().unwrap_or_default())
    }

    /// The `Authorization` header's value.
    pub(super) fn header(&self) -> Zeroizing<String> {
        Zeroizing::new(format!("Bearer {}", self.token.as_str()))
    }
}

/// The middle-tier base a URL is under: `https`, no user info or port, an AFD host, and a path that begins with
/// `api/mt/part/<partition>` (a lowercase region, `-`, a number).
fn base_of(url: &Url) -> Option<Url> {
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() || url.port().is_some() {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    let host = AFD_HOSTS.iter().find(|afd| **afd == host)?;
    let mut segments = url.path_segments()?;
    let (api, mt, part, partition) = (segments.next()?, segments.next()?, segments.next()?, segments.next()?);
    let partition_ok = partition.split_once('-').is_some_and(|(region, number)| {
        !region.is_empty()
            && region.bytes().all(|byte| byte.is_ascii_lowercase())
            && !number.is_empty()
            && number.bytes().all(|byte| byte.is_ascii_digit())
    });
    (api == "api" && mt == "mt" && part == "part" && partition_ok)
        .then(|| Url::parse(&format!("https://{host}/api/mt/part/{partition}")).ok())
        .flatten()
}

/// One GET of an event with `credential`, its body read up to [`MAX_BODY`].
pub(super) async fn fetch(
    client: &reqwest::Client,
    url: Url,
    credential: &MiddleTierCredential,
) -> Result<Vec<u8>, PartialReason> {
    let mut response = client
        .get(url)
        .header("Authorization", credential.header().as_str())
        .header("Accept", "application/json")
        .header("x-ms-migration", "True")
        .send()
        .await
        .map_err(|error| failure(&error))?;
    let status = response.status();
    if !status.is_success() {
        return Err(PartialReason::FetchFailed(FetchFailure::Status(status.as_u16())));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| failure(&error))? {
        if body.len() + chunk.len() > MAX_BODY {
            return Err(PartialReason::Truncated);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Reads one event's answer as the membership of the event `id`: [`Membership::Complete`] with its organizer and its
/// attendees (each by address, [`Mri::mail`]) when it is an object whose `objectId` is `id`, with a string
/// `organizerAddress` and an `attendees` array every entry of which has a string `address` (a plain one, or an
/// `emailAddress` object's), or no `attendees` key at all (an appointment: its organizer alone); anything else is
/// malformed.
#[must_use]
pub fn parse_event(id: &str, event: &Value) -> Membership {
    let malformed = Membership::Partial(PartialReason::FetchFailed(FetchFailure::Malformed));
    let Some(object) = event.as_object() else {
        return malformed;
    };
    if object.get("objectId").and_then(Value::as_str) != Some(id) {
        return malformed;
    }
    let Some(organizer) = object
        .get("organizerAddress")
        .and_then(Value::as_str)
        .filter(|address| address.contains('@'))
    else {
        return malformed;
    };
    // an appointment (nobody invited) reads back with no `attendees` key at all (docs/protocol/calendar.md, "A
    // series"): it reaches its organizer alone; a key that is there and not a list is malformed
    let none = Vec::new();
    let listed = match object.get("attendees") {
        None => &none,
        Some(Value::Array(listed)) => listed,
        Some(_) => return malformed,
    };
    let attendees = listed
        .iter()
        .map(|attendee| {
            let inner = attendee.get("emailAddress").unwrap_or(attendee);
            inner
                .get("address")
                .and_then(Value::as_str)
                .filter(|address| address.contains('@'))
                .map(Mri::mail)
        })
        .collect::<Option<Vec<Mri>>>();
    let Some(attendees) = attendees else {
        return malformed;
    };
    let mut members = vec![Mri::mail(organizer)];
    members.extend(attendees);
    members.sort();
    members.dedup();
    Membership::Complete(members)
}
