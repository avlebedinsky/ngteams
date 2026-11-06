//! A chat's system messages in Teams' words: what a call's start and end, a meeting recording, a transcript and the
//! thread's own events (members added, joined, left or removed, the chat's name, a meeting chat's policy) show, in the
//! chat (a line with its icon, or a recap card) and in the chat list's preview line. The words are the web client's
//! English ones; which one a message takes follows the web client's own choice (the meeting chat's, a group chat's
//! meeting, a call's).
//!
//! A meeting chat names everyone by name, the account too ("Nick started recording."), as Teams' meeting chat does. A
//! ready recording is the line "Recording has been saved to <owner>'s OneDrive and will expire in n days." in every
//! chat; a meeting chat's transcript is told by the meeting's recap card under its end, any other chat's recording and
//! transcript by a recap card of their own ([`crate::meeting_recap`]).
//!
//! `teams_proto::chat` reads the content ([`chat_event`], [`thread_activity`]); a shape it doesn't read is no event
//! here, and the views keep their placeholder for it.

use teams_proto::chat::{
    CallEnd, CallStart, ChatEvent, ChatPolicy, Members, MessageType, Recording, RecordingContents, RecordingItem,
    RecordingStatus, Removed, ThreadActivity, TopicUpdate, chat_event, is_call_media, thread_activity,
};

/// A meeting card's title when the chat has no name.
pub const MEETING: &str = "Meeting";
/// A day, ms: how far back shared history reaches is told in days.
const DAY_MS: u64 = 24 * 60 * 60 * 1_000;

/// Whether `conversation` is a meeting's chat: Teams' own test, its id naming a meeting (`19:meeting_…`).
#[must_use]
pub fn is_meeting_chat(conversation: &str) -> bool {
    conversation.contains("meeting_")
}

/// Whether `sender` is a thread itself (`19:…`): the sender of a thread's own events, never a person.
#[must_use]
pub fn is_thread(sender: &str) -> bool {
    sender.starts_with("19:")
}

/// Whether a message of `message_type` raises a notification, as the web client's chat notifications decide: a text
/// message (`Text`, `RichText/Html`) and a card (`RichText/Media_Card`). Its list of types also holds
/// `RichText/Media_CallRecording`, notified only when the recording's upload failed (a state no capture showed: never
/// here), and `ThreadActivity/AddMember`, dropped unless it is a pending chat request from outside the organization
/// (its sender, the thread, has no profile to name); a call's start or end, a transcript and every other thread event
/// are never notified.
#[must_use]
pub fn notifies(message_type: &str) -> bool {
    matches!(message_type, "Text" | "RichText/Html" | "RichText/Media_Card")
}

/// The icon before a system message's line, as Teams draws it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventIcon {
    /// Someone joined the chat, or was added to it.
    Joined,
    /// Someone was invited to the meeting.
    Invited,
    /// Someone left, or no longer has access.
    Left,
    /// Someone was removed.
    Removed,
    /// The chat's name set or taken away.
    Named,
    /// A meeting chat's policy.
    Info,
    /// A meeting's start or end (a group chat's meeting too).
    Meeting,
    /// A call's start or end.
    Call,
    /// A call missed.
    Missed,
    /// A recording started (Teams' record mark, in the brand colour).
    RecordingStarted,
    /// A recording stopped.
    RecordingStopped,
    /// A recording saved to OneDrive.
    RecordingSaved,
}

impl EventIcon {
    /// The selectors' name for it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Joined => "joined",
            Self::Invited => "invited",
            Self::Left => "left",
            Self::Removed => "removed",
            Self::Named => "named",
            Self::Info => "info",
            Self::Meeting => "meeting",
            Self::Call => "call",
            Self::Missed => "missed",
            Self::RecordingStarted => "recording-started",
            Self::RecordingStopped => "recording-stopped",
            Self::RecordingSaved => "recording-saved",
        }
    }
}

/// A piece of a system message's line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Span {
    Text(String),
    /// Drawn in bold: a meeting's length.
    Strong(String),
    /// Teams' "unverified" mark after the name before it: someone who joined anonymously.
    Unverified,
    /// " and will expire in n days" for the recording `file` of the sharing link `link` holds, once its item says when
    /// (nothing until then, or when it never expires).
    Expiry {
        link: String,
        file: RecordingItem,
    },
}

/// A system message's line: its icon, then its words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventLine {
    pub icon: EventIcon,
    pub spans: Vec<Span>,
}

impl EventLine {
    /// `text` alone after `icon`.
    #[must_use]
    pub fn plain(icon: EventIcon, text: impl Into<String>) -> Self {
        Self {
            icon,
            spans: vec![Span::Text(text.into())],
        }
    }

    /// Its words as plain text (a mark and an expiry not yet known left out).
    #[must_use]
    pub fn text(&self) -> String {
        self.spans
            .iter()
            .map(|span| match span {
                Span::Text(text) | Span::Strong(text) => text.as_str(),
                Span::Unverified | Span::Expiry { .. } => "",
            })
            .collect()
    }
}

/// A system message as Teams shows it in a chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shown {
    /// A line with its icon.
    Line(EventLine),
    /// A line with its icon, after the message's time: a call's or a recording's state, a meeting's start or end.
    TimedLine(EventLine),
    /// A 1:1 or group chat's transcript: its recap card ([`crate::meeting_recap::call_cards`]) when no recording of
    /// its call holds it.
    Transcript,
    /// Nothing: Teams draws nothing for it (a call with no length, a transcript's half of a recording, a meeting chat's
    /// transcript, a lone bot added).
    Nothing,
}

/// How people are named in a system message.
pub trait Naming {
    /// Whether `mri` is the account.
    fn is_me(&self, mri: &str) -> bool;
    /// Someone's name: their profile's, else `carried` (the name the event gives), else a placeholder; never an MRI.
    fn name(&self, mri: &str, carried: Option<&str>) -> String;
}

/// A meeting chat's naming: everyone by name, the account too, as Teams' meeting chat names them.
struct ByName<'n>(&'n dyn Naming);

impl Naming for ByName<'_> {
    fn is_me(&self, _: &str) -> bool {
        false
    }

    fn name(&self, mri: &str, carried: Option<&str>) -> String {
        self.0.name(mri, carried)
    }
}

/// Whether `mri` joined anonymously (a Teams visitor): Teams marks them unverified.
#[must_use]
pub fn is_unverified(mri: &str) -> bool {
    mri.starts_with("8:teamsvisitor:")
}

/// Who did it: "You" for the account.
fn actor(names: &dyn Naming, mri: &str) -> String {
    if names.is_me(mri) {
        "You".to_owned()
    } else {
        names.name(mri, None)
    }
}

/// Whom it was done to: "you" for the account.
fn object(names: &dyn Naming, mri: &str, carried: Option<&str>) -> String {
    if names.is_me(mri) {
        "you".to_owned()
    } else {
        names.name(mri, carried)
    }
}

/// A call's or a recording's length as Teams words it: `1h 2m 3s`, the parts that are zero left out.
#[must_use]
pub fn duration_text(seconds: u64) -> String {
    let (hours, minutes, seconds) = (seconds / 3_600, seconds / 60 % 60, seconds % 60);
    let parts: Vec<String> = [(hours, "h"), (minutes, "m"), (seconds, "s")]
        .into_iter()
        .filter(|(value, _)| *value > 0)
        .map(|(value, unit)| format!("{value}{unit}"))
        .collect();
    if parts.is_empty() {
        "0s".to_owned()
    } else {
        parts.join(" ")
    }
}

/// The people a system message names, whose names a view asks for.
#[must_use]
pub fn people(message_type: &str, content: &str) -> Vec<String> {
    if message_type == MessageType::AddMember.as_str() {
        return thread_activity(content)
            .map(|activity| activity.initiator.into_iter().chain(activity.targets).collect())
            .unwrap_or_default();
    }
    match chat_event(message_type, content) {
        Some(ChatEvent::CallStarted(start)) => start.initiator.into_iter().collect(),
        Some(ChatEvent::Recording(recording)) => [
            recording.initiator,
            recording.terminator,
            recording.organizer,
            recording.storage_owner,
        ]
        .into_iter()
        .flatten()
        .collect(),
        Some(ChatEvent::MembersJoined(members) | ChatEvent::MembersLeft(members)) => {
            members.members.into_iter().map(|member| member.mri).collect()
        }
        Some(ChatEvent::MembersRemoved(removed)) => std::iter::once(removed.initiator).chain(removed.targets).collect(),
        Some(ChatEvent::TopicUpdated(update)) => vec![update.initiator],
        _ => Vec::new(),
    }
}

/// Whom a system message names by name in any chat, the account too: a recording's owner ("saved to <name>'s
/// OneDrive").
#[must_use]
pub fn named_by_name(message_type: &str, content: &str) -> Option<String> {
    match chat_event(message_type, content)? {
        ChatEvent::Recording(recording) if recording.status == RecordingStatus::Ready => {
            recording.organizer.or(recording.storage_owner)
        }
        _ => None,
    }
}

/// What a system message of `message_type` with `content` shows in `conversation`, sent by `sender`; `None` for a
/// type or a shape this build doesn't show (the view's placeholder stands for it).
#[must_use]
pub fn shown(
    message_type: &str,
    content: &str,
    conversation: &str,
    sender: Option<&str>,
    names: &dyn Naming,
) -> Option<Shown> {
    let meeting = is_meeting_chat(conversation);
    let by_name = ByName(names);
    // the sender is told by the account's own naming: a call missed is someone else's
    let own = sender.is_some_and(|sender| names.is_me(sender));
    let names: &dyn Naming = if meeting { &by_name } else { names };
    if message_type == MessageType::AddMember.as_str() {
        return added(&thread_activity(content)?, names);
    }
    Some(match chat_event(message_type, content)? {
        ChatEvent::CallStarted(start) => call_started(&start, meeting, names),
        ChatEvent::CallEnded(end) => call_ended(&end, meeting, own),
        ChatEvent::Recording(recording) => recording_shown(&recording, names),
        // a meeting chat's transcript is told by the meeting's recap card
        ChatEvent::Transcript(_) if meeting => Shown::Nothing,
        ChatEvent::Transcript(_) => Shown::Transcript,
        ChatEvent::MembersJoined(members) => joined(&members, meeting, names),
        ChatEvent::MembersLeft(members) => left(&members, meeting, names),
        ChatEvent::MembersRemoved(removed) => removed_line(&removed, conversation, names),
        ChatEvent::TopicUpdated(update) => {
            Shown::Line(EventLine::plain(EventIcon::Named, topic_line(&update, meeting, names)))
        }
        ChatEvent::MeetingChatPolicy(policy) => Shown::Line(EventLine::plain(EventIcon::Info, policy_line(policy))),
    })
}

fn call_started(start: &CallStart, meeting: bool, names: &dyn Naming) -> Shown {
    let text = if start.huddle {
        // a group chat's meeting: who started it
        match start.initiator.as_deref() {
            Some(initiator) if names.is_me(initiator) => "You started a meeting.".to_owned(),
            Some(initiator) => format!("{} started a meeting.", names.name(initiator, None)),
            None => "Meeting started".to_owned(),
        }
    } else if meeting {
        "Meeting started".to_owned()
    } else {
        return Shown::TimedLine(EventLine::plain(EventIcon::Call, "Call started"));
    };
    Shown::TimedLine(EventLine::plain(EventIcon::Meeting, text))
}

fn call_ended(end: &CallEnd, meeting: bool, own: bool) -> Shown {
    let Some(duration) = end.duration.filter(|_| !end.participants.is_empty()) else {
        return Shown::Nothing;
    };
    let length = duration_text(duration);
    let (icon, words) = if end.huddle {
        (EventIcon::Meeting, "Meeting ended ")
    } else if meeting {
        (EventIcon::Meeting, "Meeting ended: ")
    } else if end.participants.len() < 2 && !own {
        // someone else's call that nobody else joined
        return Shown::TimedLine(EventLine::plain(EventIcon::Missed, "Call missed"));
    } else {
        (EventIcon::Call, "Call ended ")
    };
    Shown::TimedLine(EventLine {
        icon,
        spans: vec![Span::Text(words.to_owned()), Span::Strong(length)],
    })
}

fn recording_shown(recording: &Recording, names: &dyn Naming) -> Shown {
    let both = match recording.contents {
        // the transcript's half of a recording: Teams shows only the recording's
        RecordingContents::Transcript => return Shown::Nothing,
        RecordingContents::Recording => false,
        RecordingContents::RecordingAndTranscript => true,
    };
    let initiator = recording
        .initiator
        .as_deref()
        .filter(|_| !recording.automatic)
        .map(|mri| actor(names, mri));
    match recording.status {
        RecordingStatus::Started => Shown::TimedLine(EventLine::plain(
            EventIcon::RecordingStarted,
            match (initiator, both) {
                (Some(initiator), false) => format!("{initiator} started recording."),
                (Some(initiator), true) => format!("{initiator} started recording and transcription"),
                (None, false) => "Recording has started".to_owned(),
                (None, true) => "Recording and transcription has started".to_owned(),
            },
        )),
        RecordingStatus::Stopped => Shown::TimedLine(EventLine::plain(
            EventIcon::RecordingStopped,
            match (recording.terminator.as_deref(), both) {
                (_, true) => "Recording and transcription stopped".to_owned(),
                (Some(terminator), false) => format!("{} stopped recording.", actor(names, terminator)),
                (None, false) => "Recording stopped.".to_owned(),
            },
        )),
        // its recap card goes under it: a meeting chat's under the meeting's end, any other chat's under this line
        RecordingStatus::Ready => Shown::TimedLine(saved(recording, names)),
    }
}

/// A recording ready: "Recording has been saved to <owner>'s OneDrive and will expire in n days.", the expiry once its
/// item says it; the owner the meeting's organizer, or for a call's recording the one whose OneDrive holds it, named
/// by name (Teams' 1:1 chat says the account's own name there too).
fn saved(recording: &Recording, names: &dyn Naming) -> EventLine {
    let owner = recording.organizer.as_deref().or(recording.storage_owner.as_deref());
    let whose = owner.map_or_else(
        || "OneDrive".to_owned(),
        |organizer| format!("{}'s OneDrive", names.name(organizer, None)),
    );
    let mut spans = vec![Span::Text(format!("Recording has been saved to {whose}"))];
    if let (Some(link), Some(file)) = (&recording.link, &recording.file) {
        spans.push(Span::Expiry {
            link: link.clone(),
            file: file.clone(),
        });
    }
    spans.push(Span::Text(".".to_owned()));
    EventLine {
        icon: EventIcon::RecordingSaved,
        spans,
    }
}

/// " and will expire in n days": how long until `expires` (ms) from `now` (ms), the days counted up, as Teams counts
/// them; "" once it is past.
#[must_use]
pub fn expiry_clause(expires: i64, now: i64) -> String {
    match days_left(expires, now) {
        Some(1) => " and will expire in 1 day".to_owned(),
        Some(days) => format!(" and will expire in {days} days"),
        None => String::new(),
    }
}

/// The days until `expires` (ms) from `now` (ms), counted up (a part of a day is a day); `None` once it is past.
#[must_use]
pub fn days_left(expires: i64, now: i64) -> Option<i64> {
    let left = expires.checked_sub(now).filter(|left| *left > 0)?;
    let day = i64::try_from(DAY_MS).ok()?;
    Some((left + day - 1) / day)
}

/// "A", "A and B", "A and 3 others": Teams' list of the people a line names.
fn listed(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [one, two] => format!("{one} and {two}"),
        [one, rest @ ..] => {
            let others = if rest.len() == 1 { "other" } else { "others" };
            format!("{one} and {} {others}", rest.len())
        }
    }
}

/// [`listed`] as a line's spans, each name shown followed by Teams' unverified mark when it is someone's who joined
/// anonymously.
fn listed_spans(names: &[(String, bool)]) -> Vec<Span> {
    let name = |(name, unverified): &(String, bool)| {
        let mut spans = vec![Span::Text(name.clone())];
        if *unverified {
            spans.push(Span::Unverified);
        }
        spans
    };
    match names {
        [] => Vec::new(),
        [one] => name(one),
        [one, two] => {
            let mut spans = name(one);
            spans.push(Span::Text(" and ".to_owned()));
            spans.extend(name(two));
            spans
        }
        [one, rest @ ..] => {
            let others = if rest.len() == 1 { "other" } else { "others" };
            let mut spans = name(one);
            spans.push(Span::Text(format!(" and {} {others}", rest.len())));
            spans
        }
    }
}

/// `singular` for one person (`plural` for the account alone, or several).
fn verb<'a>(members: &Members, names: &dyn Naming, singular: &'a str, plural: &'a str) -> &'a str {
    match members.members.as_slice() {
        [one] if !names.is_me(&one.mri) => singular,
        _ => plural,
    }
}

fn member_names(members: &Members, names: &dyn Naming) -> Vec<Span> {
    let called: Vec<(String, bool)> = members
        .members
        .iter()
        .map(|member| {
            let name = if names.is_me(&member.mri) {
                "You".to_owned()
            } else {
                names.name(&member.mri, member.name.as_deref())
            };
            (name, is_unverified(&member.mri))
        })
        .collect();
    listed_spans(&called)
}

/// `who` and then `rest`, after `icon`.
fn named_line(icon: EventIcon, mut who: Vec<Span>, rest: String) -> Shown {
    who.push(Span::Text(rest));
    Shown::Line(EventLine { icon, spans: who })
}

fn joined(members: &Members, meeting: bool, names: &dyn Naming) -> Shown {
    let who = member_names(members, names);
    if members.temporary {
        let has = verb(members, names, "has", "have");
        named_line(EventIcon::Joined, who, format!(" {has} temporarily joined the chat."))
    } else if meeting {
        let was = verb(members, names, "was", "were");
        named_line(EventIcon::Invited, who, format!(" {was} invited to the meeting."))
    } else {
        named_line(EventIcon::Joined, who, " joined the conversation.".to_owned())
    }
}

fn left(members: &Members, meeting: bool, names: &dyn Naming) -> Shown {
    let who = member_names(members, names);
    if meeting && !members.temporary {
        named_line(EventIcon::Left, who, " left the chat.".to_owned())
    } else {
        let has = verb(members, names, "has", "have");
        named_line(EventIcon::Left, who, format!(" no longer {has} access to the chat."))
    }
}

/// `ThreadActivity/AddMember`, as Teams words it: who added whom (and how much history they were shared), or who
/// joined by themself.
fn added(activity: &ThreadActivity, names: &dyn Naming) -> Option<Shown> {
    // with no initiator it is a shape no capture showed: the view's placeholder stands for it
    let initiator = activity.initiator.as_deref()?;
    let targets: Vec<&String> = activity
        .targets
        .iter()
        .filter(|target| target.as_str() != initiator)
        .collect();
    if targets.is_empty() {
        return Some(Shown::Line(EventLine::plain(
            EventIcon::Joined,
            format!("{} joined the conversation.", actor(names, initiator)),
        )));
    }
    // a lone bot added: Teams shows nothing
    if let [one] = targets.as_slice()
        && one.starts_with("28:")
    {
        return Some(Shown::Nothing);
    }
    let whom: Vec<String> = targets.iter().map(|target| object(names, target, None)).collect();
    Some(Shown::Line(EventLine::plain(
        EventIcon::Joined,
        format!(
            "{} added {} to the chat{}.",
            actor(names, initiator),
            listed(&whom),
            share_clause(activity)
        ),
    )))
}

/// How much history an addition shared, only as far as the event says it plainly: all of it for a share time of `0`
/// (as captured), the past days (rounded, as Teams rounds them) for a known share time at least a day before a known
/// event time; nothing for anything else (a share time absent, in another form, or too close to tell).
fn share_clause(activity: &ThreadActivity) -> String {
    match (activity.share_history_time.as_known(), activity.event_time.as_known()) {
        (Some(0), _) => " and shared all chat history".to_owned(),
        (Some(since), Some(at)) => match at.saturating_sub(*since).saturating_add(DAY_MS / 2) / DAY_MS {
            0 => String::new(),
            1 => " and shared chat history from the past day".to_owned(),
            days => format!(" and shared chat history from the past {days} days"),
        },
        _ => String::new(),
    }
}

fn removed_line(removed: &Removed, conversation: &str, names: &dyn Naming) -> Shown {
    if removed.targets.first().is_some_and(|target| target.starts_with("28:")) {
        return Shown::Nothing;
    }
    let whom: Vec<String> = removed
        .targets
        .iter()
        .map(|target| object(names, target, None))
        .collect();
    let line = |icon, text: String| Shown::Line(EventLine::plain(icon, text));
    // removed by the chat itself
    if removed.initiator == conversation || is_thread(&removed.initiator) {
        return match whom.as_slice() {
            [] => Shown::Nothing,
            [one] => line(EventIcon::Removed, format!("{one} was removed from the chat.")),
            _ => line(
                EventIcon::Removed,
                format!("{} were removed from the chat.", listed(&whom)),
            ),
        };
    }
    let initiator = actor(names, &removed.initiator);
    if whom.is_empty() {
        return line(EventIcon::Left, format!("{initiator} left the chat."));
    }
    line(
        EventIcon::Removed,
        format!("{initiator} removed {} from the chat.", listed(&whom)),
    )
}

fn topic_line(update: &TopicUpdate, meeting: bool, names: &dyn Naming) -> String {
    let initiator = actor(names, &update.initiator);
    match (&update.topic, meeting) {
        (Some(topic), true) => format!("{initiator} named the meeting {topic}."),
        (Some(topic), false) => format!("{initiator} changed the group name to {topic}."),
        (None, _) => format!("{initiator} removed the group name for this conversation."),
    }
}

const fn policy_line(policy: ChatPolicy) -> &'static str {
    match policy {
        ChatPolicy::Enabled => "Chat has been turned on for this meeting.",
        ChatPolicy::Disabled => "Chat has been turned off for this meeting.",
        ChatPolicy::InMeeting => "Chat has been turned on for the duration of the meeting.",
        ChatPolicy::InMeetingByOrganizer => {
            "Chat has been turned on for the duration of the meeting due to org policy."
        }
        ChatPolicy::AnonymousDisabled => {
            "Anonymous participants can't send messages. Others can send messages during the meeting."
        }
    }
}

/// What the chat list's preview line says for a message, as Teams' list says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Preview {
    /// The message's own text (a text message).
    Text,
    /// Teams' words for it.
    Said(String),
    /// Nothing: the line comes from an older message.
    Skipped,
}

/// The preview of a message of `message_type` with `content` in `conversation`: a meeting chat's meeting started or
/// ended, a group chat's meeting, a recording's state and a transcript in Teams' words; a text message's own text;
/// anything else skipped (a 1:1 or group call, a thread's event, a type this build doesn't show).
#[must_use]
pub fn preview(message_type: &str, content: &str, conversation: &str) -> Preview {
    if message_type == MessageType::Text.as_str() || message_type == MessageType::RichTextHtml.as_str() {
        return Preview::Text;
    }
    let meeting = is_meeting_chat(conversation);
    match chat_event(message_type, content) {
        Some(ChatEvent::CallStarted(start)) if start.huddle || meeting => Preview::Said("Meeting started".to_owned()),
        Some(ChatEvent::CallEnded(end)) if end.huddle => Preview::Said("Meeting ended".to_owned()),
        Some(ChatEvent::CallEnded(end)) if meeting => match end.duration.filter(|_| !end.participants.is_empty()) {
            Some(duration) => Preview::Said(format!("Meeting ended: {}", duration_text(duration))),
            None => Preview::Skipped,
        },
        Some(ChatEvent::Recording(recording)) => recording_preview(&recording),
        Some(ChatEvent::Transcript(_)) => Preview::Said("Transcript is available".to_owned()),
        // a call's media message this build doesn't read (a deleted transcript, a recording in another state, the
        // call log's): its content is a record with ids in it, never shown as text
        None if is_call_media(message_type) => Preview::Skipped,
        // a message of another `RichText…` type (a card, …) keeps its own text, as before
        None if message_type.starts_with("RichText") => Preview::Text,
        _ => Preview::Skipped,
    }
}

fn recording_preview(recording: &Recording) -> Preview {
    if recording.contents == RecordingContents::Transcript {
        return Preview::Skipped;
    }
    Preview::Said(
        match recording.status {
            RecordingStatus::Started => "Recording has started",
            RecordingStatus::Stopped => "Recording has stopped",
            RecordingStatus::Ready => "Recording is ready",
        }
        .to_owned(),
    )
}
