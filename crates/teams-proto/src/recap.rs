//! A meeting's recap, as Teams' Recap tab reads it (`docs/protocol/recap.md`): what the meeting left (its recordings and
//! their transcripts, a transcript of a meeting that wasn't recorded) from the meeting content processing service, and
//! where each is read.
//!
//! - [`MeetingContents`] is a [`Read`] on [`Service::Mcps`] with the account's token for the meeting-artifacts app
//!   ([`MCPS_AUDIENCE`]): `GET contents/?threadId=<the meeting's chat>`, as the web client's recap asks it. Its answer
//!   ([`MeetingContents::read_answer`]) lists [`MeetingContent`]s: a `Recording` (its `location` the recording's
//!   sharing link, which the app plays through Graph as it plays any shared video) and a `TranscriptV2` (its
//!   `location` the transcript on the recording's item, in the organizer's OneDrive), each with the call, the item and
//!   their times (.NET ticks). Any other kind is left out.
//! - [`TranscriptLocation`] is where a transcript's `WebVTT` is read: the item's
//!   `media/transcripts/<id>/streamContent?is=1&applymediaedits=false` on the personal site that holds it, with the
//!   account's token for that site (the web client's own download of it, "Download as .vtt"). It is made only from a
//!   transcript's `location` on a personal site ([`DriveSite`]); a transcript anywhere else is none. Its language (the
//!   Recap player's captions menu names it) is read from the same item's transcripts
//!   (`items/<item>?select=media/transcripts&$expand=media/transcripts`, [`transcript_language`]).
//! - [`recap_parts`] pairs a meeting's recordings with their transcripts (by their item) into the parts the recap shows
//!   (Teams' Part 1, Part 2), the earliest first; [`recap_link`] is the link Teams' Share → Copy link copies.

use serde_json::Value;
use url::Url;

use crate::attachment::DriveSite;
use crate::endpoint::{Endpoint, Service};
use crate::read::Read;

/// The meeting-artifacts app MCPS takes a token for (the web client's `6bc3b958-…`, which the desktop client warms).
pub const MCPS_AUDIENCE: &str = "6bc3b958-689b-49f5-9006-36d165f30e00";

/// .NET ticks (100 ns since 0001-01-01) at the Unix epoch.
const EPOCH_TICKS: i64 = 621_355_968_000_000_000;

/// The most contents read from one answer: a meeting chat's recurring meeting may list many.
const MAX_CONTENTS: usize = 512;

/// `GET <MCPS base>/contents/?threadId=<chat>`: what a meeting left, as its recap lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetingContents {
    thread: String,
}

impl MeetingContents {
    /// The read of the meeting whose chat is `thread` (`19:meeting_…@thread.v2`).
    #[must_use]
    pub fn of(thread: impl Into<String>) -> Self {
        Self { thread: thread.into() }
    }

    /// The contents an answer lists, in its order; a resource of a kind the recap doesn't show, or one without a
    /// location, is left out. `None` when the body isn't the service's answer.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Option<Vec<MeetingContent>> {
        let answer: Value = serde_json::from_slice(body).ok()?;
        let resources = answer.get("resources")?.as_array()?;
        Some(
            resources
                .iter()
                .take(MAX_CONTENTS)
                .filter_map(MeetingContent::read)
                .collect(),
        )
    }
}

impl Read for MeetingContents {
    fn service(&self) -> Service {
        Service::Mcps
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["contents"]).with_query("threadId", self.thread.clone())
    }
}

/// What a content is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ContentKind {
    /// `Recording`: its location is the recording's sharing link.
    Recording,
    /// `TranscriptV2`: its location is the transcript on the recording's item.
    Transcript,
}

/// One thing a meeting left, as MCPS lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetingContent {
    pub kind: ContentKind,
    /// `location`: a recording's sharing link, a transcript's address on its item (always `https`).
    pub location: String,
    /// The call it is of (`metadata.callId`).
    pub call_id: Option<String>,
    /// The drive and the item that hold it (`metadata.driveId`, `metadata.driveItemId`): a recording and its
    /// transcript share the item.
    pub drive_id: Option<String>,
    pub item_id: Option<String>,
    /// When it began and ended, ms since the epoch (`metadata.startTime`, `endTime`: .NET ticks).
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
    /// The item's sharing link (`metadata.webUrl`, a transcript's; empty on a recording's, whose location is it).
    pub web_url: Option<String>,
    /// The meeting's organizer and their tenant (`metadata.organizerId`, `metadata.ownerTenantId`).
    pub organizer_id: Option<String>,
    pub tenant_id: Option<String>,
}

impl MeetingContent {
    fn read(resource: &Value) -> Option<Self> {
        let kind = match resource.get("type")?.as_str()? {
            "Recording" => ContentKind::Recording,
            "TranscriptV2" => ContentKind::Transcript,
            _ => return None,
        };
        let location = text(resource.get("location"))?;
        if !location.starts_with("https://") {
            return None;
        }
        let metadata = resource.get("metadata");
        let field = |name: &str| text(metadata.and_then(|metadata| metadata.get(name)));
        let https = |name: &str| field(name).filter(|link| link.starts_with("https://"));
        Some(Self {
            kind,
            location,
            call_id: field("callId"),
            drive_id: field("driveId"),
            item_id: field("driveItemId"),
            start_ms: field("startTime").as_deref().and_then(ticks_ms),
            end_ms: field("endTime").as_deref().and_then(ticks_ms),
            web_url: https("webUrl"),
            organizer_id: field("organizerId"),
            tenant_id: field("ownerTenantId").or_else(|| field("organizerTenantId")),
        })
    }

    /// The link the recording is played and shared by: a recording's location, a transcript's item's sharing link.
    #[must_use]
    pub fn sharing_link(&self) -> Option<&str> {
        match self.kind {
            ContentKind::Recording => Some(&self.location),
            ContentKind::Transcript => self.web_url.as_deref(),
        }
    }
}

/// A string value, trimmed; `None` for an empty one or another kind of value.
fn text(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// .NET ticks written as digits, in ms since the epoch.
fn ticks_ms(text: &str) -> Option<i64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let ticks: i64 = text.parse().ok()?;
    // a time before the epoch is none a meeting has
    Some(ticks.checked_sub(EPOCH_TICKS)? / 10_000).filter(|ms| *ms >= 0)
}

/// Where a transcript's `WebVTT` is read: on the personal site that holds its item, with that site's token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptLocation {
    site: DriveSite,
    url: String,
    /// The item's transcripts' address (their languages).
    languages: String,
    /// The transcript's id among them.
    id: String,
}

impl TranscriptLocation {
    /// The `WebVTT` of the transcript a `TranscriptV2` content's `location` names
    /// (`<personal site>/_api/v2.1/drives/<drive>/items/<item>/versions/current/media/transcripts/<id>/content`, or the
    /// same without `versions/current`); `None` for an address of another shape, or off a personal site.
    #[must_use]
    pub fn of(location: &str) -> Option<Self> {
        let url = Url::parse(location).ok()?;
        if url.query().is_some() || url.fragment().is_some() {
            return None;
        }
        let site = DriveSite::from_web_url(location)?;
        let segments: Vec<&str> = url.path_segments()?.collect();
        // personal/<name>/_api/v2.1/drives/<drive>/items/<item>/[versions/current/]media/transcripts/<id>/content
        let rest = segments.get(2..)?;
        let (drive, item, id) = match rest {
            [
                "_api",
                "v2.1",
                "drives",
                drive,
                "items",
                item,
                "versions",
                "current",
                "media",
                "transcripts",
                id,
                "content",
            ]
            | [
                "_api",
                "v2.1",
                "drives",
                drive,
                "items",
                item,
                "media",
                "transcripts",
                id,
                "content",
            ] => (*drive, *item, *id),
            _ => return None,
        };
        let plain = |part: &str| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'!' | b'-' | b'_' | b'%'))
        };
        if !(plain(drive) && plain(item) && plain(id)) {
            return None;
        }
        let url = format!(
            "{}_api/v2.1/drives/{drive}/items/{item}/media/transcripts/{id}/streamContent?is=1&applymediaedits=false",
            site.base()
        );
        let languages = format!(
            "{}_api/v2.1/drives/{drive}/items/{item}?select=media%2Ftranscripts&%24expand=media%2Ftranscripts",
            site.base()
        );
        Some(Self {
            site,
            url,
            languages,
            id: id.to_owned(),
        })
    }

    /// The personal site the transcript is on: its token's audience.
    #[must_use]
    pub fn site(&self) -> &DriveSite {
        &self.site
    }

    /// The address its `WebVTT` is read at.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The address its item's transcripts are read at, as the web client's player reads them for their languages
    /// ([`transcript_language`]).
    #[must_use]
    pub fn languages_url(&self) -> &str {
        &self.languages
    }

    /// The transcript's id on its item.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
}

/// The longest language tag taken (BCP 47's longest in practice).
const MAX_LANGUAGE_TAG: usize = 35;

/// The language (a BCP 47 tag, `en-US`) of transcript `id` in the answer to its item's transcripts' read
/// ([`TranscriptLocation::languages_url`]: `{media: {transcripts: [{id, languageTag, isDefault, …}]}}`): that
/// transcript's, else the item's default one's; `None` when neither names one, or the tag isn't one.
#[must_use]
pub fn transcript_language(body: &[u8], id: &str) -> Option<String> {
    fn tag_of(transcript: &Value) -> Option<&str> {
        transcript.get("languageTag")?.as_str()
    }
    let answer: Value = serde_json::from_slice(body).ok()?;
    let transcripts = answer.get("media")?.get("transcripts")?.as_array()?;
    let tag = transcripts
        .iter()
        .find(|transcript| transcript.get("id").and_then(Value::as_str) == Some(id))
        .and_then(tag_of)
        .or_else(|| {
            transcripts
                .iter()
                .find(|transcript| transcript.get("isDefault").and_then(Value::as_bool) == Some(true))
                .and_then(tag_of)
        })?;
    let fits = !tag.is_empty()
        && tag.len() <= MAX_LANGUAGE_TAG
        && tag.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
    fits.then(|| tag.to_owned())
}

/// One part of a meeting's recap: a recording with its transcript, or a transcript of a meeting that wasn't recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecapPart {
    /// The recording's sharing link, when the part has a recording.
    pub recording: Option<String>,
    /// The transcript's address on its item, when the part has a transcript.
    pub transcript: Option<String>,
    /// The part's item's sharing link (the recording's, else its transcript's item's).
    pub sharing_link: Option<String>,
    pub call_id: Option<String>,
    pub drive_id: Option<String>,
    pub item_id: Option<String>,
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
    pub organizer_id: Option<String>,
    pub tenant_id: Option<String>,
}

/// The parts the recap shows, the earliest first: each recording with the transcript on its item, then every
/// transcript no recording holds. A content without an item is a part of its own.
#[must_use]
pub fn recap_parts(contents: &[MeetingContent]) -> Vec<RecapPart> {
    let mut parts: Vec<RecapPart> = Vec::new();
    let same_item =
        |part: &RecapPart, content: &MeetingContent| content.item_id.is_some() && part.item_id == content.item_id;
    for content in contents.iter().filter(|content| content.kind == ContentKind::Recording) {
        if parts.iter().any(|part| same_item(part, content)) {
            continue;
        }
        parts.push(part_of(content));
    }
    for content in contents
        .iter()
        .filter(|content| content.kind == ContentKind::Transcript)
    {
        match parts.iter_mut().find(|part| same_item(part, content)) {
            Some(part) if part.transcript.is_none() => part.transcript = Some(content.location.clone()),
            Some(_) => {}
            None => parts.push(part_of(content)),
        }
    }
    parts.sort_by_key(|part| part.start_ms.unwrap_or(i64::MAX));
    parts
}

fn part_of(content: &MeetingContent) -> RecapPart {
    let recording = (content.kind == ContentKind::Recording).then(|| content.location.clone());
    let transcript = (content.kind == ContentKind::Transcript).then(|| content.location.clone());
    RecapPart {
        recording,
        transcript,
        sharing_link: content.sharing_link().map(str::to_owned),
        call_id: content.call_id.clone(),
        drive_id: content.drive_id.clone(),
        item_id: content.item_id.clone(),
        start_ms: content.start_ms,
        end_ms: content.end_ms,
        organizer_id: content.organizer_id.clone(),
        tenant_id: content.tenant_id.clone(),
    }
}

/// The meeting's whole span across its parts: the earliest start to the latest end (Teams' session line).
#[must_use]
pub fn recap_span(parts: &[RecapPart]) -> Option<(i64, i64)> {
    let start = parts.iter().filter_map(|part| part.start_ms).min()?;
    let end = parts.iter().filter_map(|part| part.end_ms).max()?;
    Some((start, end.max(start)))
}

/// Where Share → Copy link was asked for: the Recap tab's Share, or the chat's recap card's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkFrom {
    Recap,
    Card,
}

/// The link Teams' Recap tab's Share → Copy link copies for a part of the recap of the chat `thread`
/// ([`recap_link_from`] from [`LinkFrom::Recap`]).
#[must_use]
pub fn recap_link(part: &RecapPart, thread: &str) -> Option<String> {
    recap_link_from(part, thread, LinkFrom::Recap)
}

/// The link Teams' Share → Copy link copies for a part of the recap of the chat `thread`, asked for `from` the Recap
/// tab or the chat's card: `https://teams.microsoft.com/l/meetingrecap?…` with the part's item, its sharing link, the
/// chat, organizer, tenant and call, and what the recap holds; `None` without the item or the sharing link.
///
/// A meeting's (and, as no capture showed a group chat's, a group chat's) is `threadType=meeting` from the Recap tab's
/// shape. A 1:1 chat's call is `threadType=OneOnOneChat&meetingType=Unknown`; from its card it is the card's own
/// shape, `subType=RecapSharingLink_RecapChiclet` with the sharing link as `fileUrl` too and no organizer or tenant, as
/// browser Teams copies it.
#[must_use]
pub fn recap_link_from(part: &RecapPart, thread: &str, from: LinkFrom) -> Option<String> {
    let one_to_one = thread.ends_with("@unq.gbl.spaces");
    let card = one_to_one && from == LinkFrom::Card;
    let mut url = Url::parse("https://teams.microsoft.com/l/meetingrecap").ok()?;
    {
        let mut query = url.query_pairs_mut();
        let sharing_link = part.sharing_link.as_deref()?;
        query
            .append_pair("driveId", part.drive_id.as_deref()?)
            .append_pair("driveItemId", part.item_id.as_deref()?)
            .append_pair("sitePath", sharing_link);
        if card {
            query.append_pair("fileUrl", sharing_link);
        }
        query.append_pair("threadId", thread);
        if !card {
            if let Some(organizer) = &part.organizer_id {
                query.append_pair("organizerId", organizer);
            }
            if let Some(tenant) = &part.tenant_id {
                query.append_pair("tenantId", tenant);
            }
        }
        if let Some(call) = &part.call_id {
            query.append_pair("callId", call);
        }
        let holds = match (part.recording.is_some(), part.transcript.is_some()) {
            (true, true) => "RecordingAndTranscript",
            (true, false) => "Recording",
            (false, _) => "Transcript",
        };
        if one_to_one {
            query
                .append_pair("threadType", "OneOnOneChat")
                .append_pair("meetingType", "Unknown");
        } else {
            query.append_pair("threadType", "meeting");
        }
        let sub_type = if card {
            "RecapSharingLink_RecapChiclet"
        } else {
            "RecapSharingLink_RecapCore"
        };
        query.append_pair("subType", sub_type).append_pair("recapType", holds);
    }
    Some(url.into())
}
