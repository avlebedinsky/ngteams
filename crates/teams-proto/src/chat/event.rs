//! A chat's system messages, read from their `messagetype` and content into what Teams draws for them ([`ChatEvent`]):
//! a call's start and end (`Event/Call`), a meeting recording's state (`RichText/Media_CallRecording`), a transcript
//! (`RichText/Media_CallTranscript`), and the thread's own events that name people or the chat (`ThreadActivity/…`:
//! members joined, left or removed, the topic, the meeting chat's policy). The shapes are the ones the lab's calls and
//! the personal build's stores showed (`docs/protocol/chat.md`, "System messages").
//!
//! A shape no capture showed is no event ([`chat_event`] gives `None`): the view keeps its placeholder for it and never
//! guesses. `ThreadActivity/AddMember` is read by [`crate::chat::thread_activity`].

use scraper::{ElementRef, Html};
use serde_json::{Map, Value};

use crate::chat::html::bounded;

/// The start of the `messagetype` of a call's media messages: a meeting recording (`RichText/Media_CallRecording`), a
/// transcript (`…_CallTranscript`) and the call log's (`…_CallLogRecording`, `…_CallLogTranscript`). Their content is
/// a record (XML or JSON, ids in it), never text a reader is shown.
pub const CALL_MEDIA_PREFIX: &str = "RichText/Media_Call";

/// Whether a message of `message_type` is one of a call's media messages ([`CALL_MEDIA_PREFIX`]): what it says is
/// [`chat_event`]'s or nothing, never its content as text.
#[must_use]
pub fn is_call_media(message_type: &str) -> bool {
    message_type.starts_with(CALL_MEDIA_PREFIX)
}

/// What a system message says, as Teams draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatEvent {
    /// `Event/Call` with a `<partlist>` and no `<ended/>`: a call (or a meeting) began.
    CallStarted(CallStart),
    /// `Event/Call` with `<ended/>`: a call (or a meeting) ended.
    CallEnded(CallEnd),
    /// `RichText/Media_CallRecording`: a meeting recording started, stopped, or is ready.
    Recording(Recording),
    /// `RichText/Media_CallTranscript`: a meeting's transcript is ready.
    Transcript(Transcript),
    /// `ThreadActivity/MemberJoined`: people joined the chat (a meeting's: were invited, or joined for the meeting).
    MembersJoined(Members),
    /// `ThreadActivity/MemberLeft`: people left the chat.
    MembersLeft(Members),
    /// `ThreadActivity/DeleteMember`: someone left, or removed others.
    MembersRemoved(Removed),
    /// `ThreadActivity/TopicUpdate`: the chat's name set or taken away.
    TopicUpdated(TopicUpdate),
    /// `ThreadActivity/MeetingPolicyUpdated`: who may chat in a meeting's chat.
    MeetingChatPolicy(ChatPolicy),
}

/// A call's start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallStart {
    /// A group chat's "meeting" (`<conversationType>"huddleGroupCall"</conversationType>`).
    pub huddle: bool,
    /// Who started it: the first participant the start names (a group chat's meeting names its starter).
    pub initiator: Option<String>,
}

/// A call's end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallEnd {
    /// A group chat's "meeting".
    pub huddle: bool,
    /// How long it took, in seconds: the last participant's `<duration>`, as Teams reads it; `None` when none is
    /// given (Teams then draws nothing for it).
    pub duration: Option<u64>,
    /// Every participant's MRI (`<name>`), in order.
    pub participants: Vec<String>,
}

/// What a recording's message reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordingStatus {
    /// `Initial`, code `0`: it began.
    Started,
    /// `ChunkFinished`, code `202`: it stopped and is being saved.
    Stopped,
    /// `Success`, code `200`: it is saved and can be played.
    Ready,
}

/// What a recording holds (`RecordingContent`'s `contentTypes`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordingContents {
    /// `Recording`, or no `contentTypes` at all (a channel meeting's).
    Recording,
    /// `Transcript`: the transcript's half of a recording.
    Transcript,
    /// `Recording+Transcript`.
    RecordingAndTranscript,
}

/// A meeting recording's message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recording {
    pub status: RecordingStatus,
    pub contents: RecordingContents,
    /// The meeting's title (`<Title>`).
    pub title: Option<String>,
    /// The recording's file name (`<OriginalName v>`).
    pub file_name: Option<String>,
    /// Its length in whole seconds (`RecordingContent`'s `duration`, a .NET time span); `None` when it can't be read.
    pub duration: Option<u64>,
    /// Who started it (`RecordingInitiatorId`'s `value`).
    pub initiator: Option<String>,
    /// Whether it started by itself (`isAutoRecording="True"`).
    pub automatic: bool,
    /// Its player's link (`<a href>`, empty until it is ready), when it is an `https` link.
    pub link: Option<String>,
}

impl Recording {
    /// The name its card shows: its file's name without the extension, else the meeting's title.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.file_name
            .as_deref()
            .map(|name| name.rsplit_once('.').map_or(name, |(stem, _)| stem))
            .filter(|name| !name.is_empty())
            .or(self.title.as_deref())
    }
}

/// A meeting's transcript.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transcript {
    /// The call it is of.
    pub call_id: Option<String>,
}

/// People who joined or left.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Members {
    pub initiator: Option<String>,
    pub members: Vec<Member>,
    /// The first member's `meetingMemberType` is `temp`: they joined for the meeting only.
    pub temporary: bool,
}

/// One of [`Members`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub mri: String,
    /// The name the event gives (`friendlyname`), when it gives one.
    pub name: Option<String>,
}

/// Someone left, or removed others.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Removed {
    pub initiator: String,
    /// Those removed, the initiator left out: none when the initiator left by themself.
    pub targets: Vec<String>,
}

/// The chat's name set or taken away.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopicUpdate {
    pub initiator: String,
    /// The new name; `None` when it was taken away.
    pub topic: Option<String>,
}

/// A meeting chat's policy, by Teams' `value`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatPolicy {
    /// `ChatEnabled`.
    Enabled,
    /// `ChatDisabled`.
    Disabled,
    /// `InMeeting`.
    InMeeting,
    /// `InMeetingRestrictOrganizer`.
    InMeetingByOrganizer,
    /// `ChatEnabledAnonymousDisabled`, `InMeetingAnonymousDisabled`, `InMeetingAnonymousDisabledRestrictOrganizer`.
    AnonymousDisabled,
}

/// A system message's event, from its `messagetype` and content; `None` for any other type, and for a shape no capture
/// showed.
#[must_use]
pub fn chat_event(message_type: &str, content: &str) -> Option<ChatEvent> {
    match message_type {
        "Event/Call" => call(content),
        "RichText/Media_CallRecording" => recording(content).map(ChatEvent::Recording),
        "RichText/Media_CallTranscript" => transcript(content).map(ChatEvent::Transcript),
        "ThreadActivity/MemberJoined" => members(content).map(ChatEvent::MembersJoined),
        "ThreadActivity/MemberLeft" => members(content).map(ChatEvent::MembersLeft),
        "ThreadActivity/DeleteMember" => removed(content).map(ChatEvent::MembersRemoved),
        "ThreadActivity/TopicUpdate" => topic(content).map(ChatEvent::TopicUpdated),
        "ThreadActivity/MeetingPolicyUpdated" => policy(content).map(ChatEvent::MeetingChatPolicy),
        _ => None,
    }
}

/// The elements of an XML content, parsed as HTML (names lowercased), in document order.
fn elements(html: &Html) -> impl Iterator<Item = ElementRef<'_>> {
    html.tree.root().descendants().filter_map(ElementRef::wrap)
}

/// An element's text, trimmed.
fn text_of(element: ElementRef<'_>) -> String {
    element.text().collect::<String>().trim().to_owned()
}

/// The text of the first element named `name`, when it has some.
fn first_text(html: &Html, name: &str) -> Option<String> {
    elements(html)
        .find(|element| element.value().name() == name)
        .map(text_of)
        .filter(|text| !text.is_empty())
}

/// The attribute `attribute` of the first element named `name`.
fn first_attribute<'h>(html: &'h Html, name: &str, attribute: &str) -> Option<&'h str> {
    elements(html)
        .find(|element| element.value().name() == name)
        .and_then(|element| element.value().attr(attribute))
}

fn call(content: &str) -> Option<ChatEvent> {
    // Teams' own test: `<ended/>` anywhere, or a first `<partlist>` tag of `type="ended"`
    let first_tag = content.split_once('>').map_or(content, |(tag, _)| tag);
    let ended =
        content.contains("<ended/>") || (first_tag.starts_with("<partlist") && first_tag.contains("type=\"ended\""));
    if !ended && !content.contains("<partlist") {
        return None;
    }
    let huddle = content.contains("<conversationType>\"huddleGroupCall\"</conversationType>");
    let html = Html::parse_fragment(bounded(content).0);
    let participants: Vec<String> = elements(&html)
        .filter(|element| element.value().name() == "name")
        .map(text_of)
        .collect();
    if !ended {
        return Some(ChatEvent::CallStarted(CallStart {
            huddle,
            initiator: participants.first().filter(|name| !name.is_empty()).cloned(),
        }));
    }
    // the last participant's, as Teams reads it; one in a form no capture showed makes the whole unknown
    let duration = match elements(&html)
        .filter(|element| element.value().name() == "duration")
        .map(text_of)
        .last()
    {
        None => None,
        Some(text) => Some(digits(&text)?),
    };
    Some(ChatEvent::CallEnded(CallEnd {
        huddle,
        duration,
        participants,
    }))
}

/// A meeting's times as a call's event in its chat gives them (`<meetingDetails>`): when it was scheduled, and the
/// call it is of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetingTimes {
    /// Its scheduled start and end, ms since the epoch (`<startTime>`, `<endTime>`: `MM/dd/yyyy HH:mm:ss`, UTC), when
    /// both read and the end isn't before the start.
    pub scheduled: Option<(i64, i64)>,
    /// The call's id (`<callId>`), the one a recording's and a transcript's messages name.
    pub call_id: Option<String>,
}

/// The times an `Event/Call`'s `content` gives; `None` when it gives neither.
#[must_use]
pub fn meeting_times(content: &str) -> Option<MeetingTimes> {
    let html = Html::parse_fragment(bounded(content).0);
    let start = first_text(&html, "starttime").and_then(|text| utc_ms(&text));
    let end = first_text(&html, "endtime").and_then(|text| utc_ms(&text));
    let scheduled = start.zip(end).filter(|(start, end)| end >= start);
    let call_id = first_text(&html, "callid");
    (scheduled.is_some() || call_id.is_some()).then_some(MeetingTimes { scheduled, call_id })
}

/// `MM/dd/yyyy HH:mm:ss` (UTC) as ms since the epoch.
fn utc_ms(text: &str) -> Option<i64> {
    let (date, time) = text.trim().split_once(' ')?;
    let mut date = date.split('/');
    let (month, day, year) = (digits(date.next()?)?, digits(date.next()?)?, digits(date.next()?)?);
    let mut time = time.split(':');
    let (hours, minutes, seconds) = (digits(time.next()?)?, digits(time.next()?)?, digits(time.next()?)?);
    if date.next().is_some() || time.next().is_some() {
        return None;
    }
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hours > 23 || minutes > 59 || seconds > 59 {
        return None;
    }
    let days = days_from_civil(i64::try_from(year).ok()?, month, day);
    let seconds = days * 86_400 + i64::try_from(hours * 3_600 + minutes * 60 + seconds).ok()?;
    Some(seconds * 1_000)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's `days_from_civil`); `month` 1–12.
fn days_from_civil(year: i64, month: u64, day: u64) -> i64 {
    let (month, day) = (i64::try_from(month).unwrap_or(1), i64::try_from(day).unwrap_or(1));
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year - era * 400;
    let of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let of_cycle = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
    era * 146_097 + of_cycle - 719_468
}

/// A number written as digits only (as every captured count and length is); anything else is no number.
fn digits(text: &str) -> Option<u64> {
    (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

fn recording(content: &str) -> Option<Recording> {
    let html = Html::parse_fragment(bounded(content).0);
    let status = first_attribute(&html, "recordingstatus", "status")?;
    let code = first_attribute(&html, "recordingstatus", "code")?;
    let status = match (status, code) {
        ("Initial", "0") => RecordingStatus::Started,
        ("ChunkFinished", "202") => RecordingStatus::Stopped,
        ("Success", "200") => RecordingStatus::Ready,
        _ => return None,
    };
    let contents = match first_attribute(&html, "recordingcontent", "contenttypes") {
        None | Some("Recording") => RecordingContents::Recording,
        Some("Transcript") => RecordingContents::Transcript,
        Some("Recording+Transcript") => RecordingContents::RecordingAndTranscript,
        Some(_) => return None,
    };
    let initiator = elements(&html).find(|element| element.value().name() == "recordinginitiatorid");
    Some(Recording {
        status,
        contents,
        title: first_text(&html, "title"),
        file_name: first_attribute(&html, "originalname", "v")
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned),
        duration: first_attribute(&html, "recordingcontent", "duration").and_then(time_span),
        initiator: initiator
            .and_then(|element| element.value().attr("value"))
            .map(str::trim)
            .filter(|mri| !mri.is_empty())
            .map(str::to_owned),
        automatic: initiator.and_then(|element| element.value().attr("isautorecording")) == Some("True"),
        link: first_attribute(&html, "a", "href")
            .map(str::trim)
            .filter(|link| link.starts_with("https://"))
            .map(str::to_owned),
    })
}

/// A .NET time span (`[d.]h:mm:ss[.fffffff]`) in whole seconds.
fn time_span(text: &str) -> Option<u64> {
    let mut parts = text.trim().split(':');
    let (hours, minutes, seconds) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let (days, hours) = hours.split_once('.').unwrap_or(("0", hours));
    let whole = seconds.split_once('.').map_or(seconds, |(whole, _)| whole);
    let (minutes, whole) = (digits(minutes)?, digits(whole)?);
    (minutes < 60 && whole < 60).then_some(())?;
    Some(((digits(days)? * 24 + digits(hours)?) * 60 + minutes) * 60 + whole)
}

fn transcript(content: &str) -> Option<Transcript> {
    // captured with every quote escaped (`{\"scopeId\":…}`); read as JSON either way
    let object = serde_json::from_str::<Map<String, Value>>(content)
        .or_else(|_| serde_json::from_str::<Map<String, Value>>(&content.replace("\\\"", "\"")))
        .ok()?;
    if object.get("isDeleted") != Some(&Value::Bool(false)) {
        return None;
    }
    Some(Transcript {
        call_id: object.get("callId").and_then(Value::as_str).map(str::to_owned),
    })
}

fn members(content: &str) -> Option<Members> {
    let object = serde_json::from_str::<Map<String, Value>>(content).ok()?;
    let list = object.get("members")?.as_array()?;
    let mut members = Vec::with_capacity(list.len());
    for entry in list {
        let mri = entry.get("id")?.as_str()?.trim();
        if mri.is_empty() {
            return None;
        }
        let name = match entry.get("friendlyname") {
            None | Some(Value::Null) => None,
            Some(Value::String(name)) => Some(name.trim().to_owned()).filter(|name| !name.is_empty()),
            Some(_) => return None,
        };
        members.push(Member {
            mri: mri.to_owned(),
            name,
        });
    }
    if members.is_empty() {
        return None;
    }
    let temporary = list
        .first()
        .and_then(|first| first.get("meetingMemberType"))
        .and_then(Value::as_str)
        == Some("temp");
    Some(Members {
        initiator: object.get("initiator").and_then(Value::as_str).map(str::to_owned),
        members,
        temporary,
    })
}

fn removed(content: &str) -> Option<Removed> {
    let html = Html::parse_fragment(bounded(content).0);
    let initiator = first_text(&html, "initiator")?;
    let targets = elements(&html)
        .filter(|element| element.value().name() == "target")
        .map(text_of)
        .filter(|target| !target.is_empty() && *target != initiator)
        .collect();
    Some(Removed { initiator, targets })
}

fn topic(content: &str) -> Option<TopicUpdate> {
    let html = Html::parse_fragment(bounded(content).0);
    Some(TopicUpdate {
        initiator: first_text(&html, "initiator")?,
        topic: first_text(&html, "value"),
    })
}

fn policy(content: &str) -> Option<ChatPolicy> {
    let html = Html::parse_fragment(bounded(content).0);
    match first_text(&html, "value")?.as_str() {
        "ChatEnabled" => Some(ChatPolicy::Enabled),
        "ChatDisabled" => Some(ChatPolicy::Disabled),
        "InMeeting" => Some(ChatPolicy::InMeeting),
        "InMeetingRestrictOrganizer" => Some(ChatPolicy::InMeetingByOrganizer),
        "ChatEnabledAnonymousDisabled"
        | "InMeetingAnonymousDisabled"
        | "InMeetingAnonymousDisabledRestrictOrganizer" => Some(ChatPolicy::AnonymousDisabled),
        _ => None,
    }
}
