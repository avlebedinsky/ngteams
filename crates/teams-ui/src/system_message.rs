//! A chat's system messages in Teams' words: what a call's start and end, a meeting recording, a transcript and the
//! thread's own events (members added, joined, left or removed, the chat's name, a meeting chat's policy) show, in the
//! chat (a centred line, or a card) and in the chat list's preview line. The words are the web client's English ones;
//! which one a message takes follows the web client's own choice (the meeting chat's, a group chat's meeting, a call's).
//!
//! `teams_proto::chat` reads the content ([`chat_event`], [`thread_activity`]); a shape it doesn't read is no event
//! here, and the views keep their placeholder for it.

use teams_proto::chat::{
    CallEnd, CallStart, ChatEvent, ChatPolicy, Members, MessageType, Recording, RecordingContents, RecordingStatus,
    Removed, ThreadActivity, TopicUpdate, chat_event, is_call_media, thread_activity,
};

/// A meeting card's title when the chat has no name.
pub const MEETING: &str = "Meeting";
/// A transcript card's title.
pub const TRANSCRIPT: &str = "Transcript";
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

/// What a card shows a picture of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardKind {
    MeetingStarted,
    MeetingEnded,
    /// A recording that is ready: played in Teams on the web.
    Recording,
    /// A transcript: read in Teams on the web.
    Transcript,
}

impl CardKind {
    /// The trace's and the selectors' name for it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::MeetingStarted => "meeting-started",
            Self::MeetingEnded => "meeting-ended",
            Self::Recording => "recording",
            Self::Transcript => "transcript",
        }
    }

    /// Whether the card links to Teams on the web (a recording is played there, a transcript read there).
    #[must_use]
    pub const fn opens_on_the_web(self) -> bool {
        matches!(self, Self::Recording | Self::Transcript)
    }
}

/// A system message as Teams shows it in a chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shown {
    /// A centred line.
    Line(String),
    /// A centred line after the message's time: a call's or a recording's state.
    TimedLine(String),
    /// A card: a meeting's start or end (the chat's name for its title), a recording or a transcript.
    Card {
        kind: CardKind,
        /// The card's own title; `None` for a meeting's, which takes the chat's name.
        title: Option<String>,
        detail: String,
    },
    /// Nothing: Teams draws nothing for it (a call with no length, a transcript's half of a recording, a lone bot
    /// added).
    Nothing,
}

/// How people are named in a system message.
pub trait Naming {
    /// Whether `mri` is the account.
    fn is_me(&self, mri: &str) -> bool;
    /// Someone's name: their profile's, else `carried` (the name the event gives), else a placeholder; never an MRI.
    fn name(&self, mri: &str, carried: Option<&str>) -> String;
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
        Some(ChatEvent::Recording(recording)) => recording.initiator.into_iter().collect(),
        Some(ChatEvent::MembersJoined(members) | ChatEvent::MembersLeft(members)) => {
            members.members.into_iter().map(|member| member.mri).collect()
        }
        Some(ChatEvent::MembersRemoved(removed)) => std::iter::once(removed.initiator).chain(removed.targets).collect(),
        Some(ChatEvent::TopicUpdated(update)) => vec![update.initiator],
        _ => Vec::new(),
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
    if message_type == MessageType::AddMember.as_str() {
        return added(&thread_activity(content)?, names);
    }
    let own = sender.is_some_and(|sender| names.is_me(sender));
    Some(match chat_event(message_type, content)? {
        ChatEvent::CallStarted(start) => call_started(&start, meeting, names),
        ChatEvent::CallEnded(end) => call_ended(&end, meeting, own),
        ChatEvent::Recording(recording) => recording_shown(&recording, names),
        ChatEvent::Transcript(_) => Shown::Card {
            kind: CardKind::Transcript,
            title: Some(TRANSCRIPT.to_owned()),
            detail: "Transcript is available".to_owned(),
        },
        ChatEvent::MembersJoined(members) => Shown::Line(joined(&members, meeting, names)),
        ChatEvent::MembersLeft(members) => Shown::Line(left(&members, meeting, names)),
        ChatEvent::MembersRemoved(removed) => removed_line(&removed, conversation, names),
        ChatEvent::TopicUpdated(update) => Shown::Line(topic_line(&update, meeting, names)),
        ChatEvent::MeetingChatPolicy(policy) => Shown::Line(policy_line(policy).to_owned()),
    })
}

fn call_started(start: &CallStart, meeting: bool, names: &dyn Naming) -> Shown {
    if start.huddle {
        // a group chat's meeting: who started it
        return Shown::TimedLine(match start.initiator.as_deref() {
            Some(initiator) if names.is_me(initiator) => "You started a meeting.".to_owned(),
            Some(initiator) => format!("{} started a meeting.", names.name(initiator, None)),
            None => "Meeting started".to_owned(),
        });
    }
    if meeting {
        return Shown::Card {
            kind: CardKind::MeetingStarted,
            title: None,
            detail: "Meeting started".to_owned(),
        };
    }
    Shown::TimedLine("Call started".to_owned())
}

fn call_ended(end: &CallEnd, meeting: bool, own: bool) -> Shown {
    let Some(duration) = end.duration.filter(|_| !end.participants.is_empty()) else {
        return Shown::Nothing;
    };
    let length = duration_text(duration);
    if end.huddle {
        return Shown::TimedLine(format!("Meeting ended {length}"));
    }
    if meeting {
        return Shown::Card {
            kind: CardKind::MeetingEnded,
            title: None,
            detail: format!("Meeting ended: {length}"),
        };
    }
    // someone else's call that nobody else joined
    if end.participants.len() < 2 && !own {
        return Shown::TimedLine("Call missed".to_owned());
    }
    Shown::TimedLine(format!("Call ended {length}"))
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
        RecordingStatus::Started => Shown::TimedLine(match (initiator, both) {
            (Some(initiator), false) => format!("{initiator} started recording."),
            (Some(initiator), true) => format!("{initiator} started recording and transcription"),
            (None, false) => "Recording has started".to_owned(),
            (None, true) => "Recording and transcription has started".to_owned(),
        }),
        RecordingStatus::Stopped => Shown::TimedLine(if both {
            "Recording and transcription stopped".to_owned()
        } else {
            "Recording stopped. Saving recording\u{2026}".to_owned()
        }),
        RecordingStatus::Ready => {
            let what = if both { "Recording and transcript" } else { "Recording" };
            Shown::Card {
                kind: CardKind::Recording,
                title: Some(recording.name().unwrap_or("Recording").to_owned()),
                detail: match recording.duration {
                    Some(duration) => format!("{what} \u{b7} {}", duration_text(duration)),
                    None => what.to_owned(),
                },
            }
        }
    }
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

/// `singular` for one person (`plural` for the account alone, or several).
fn verb<'a>(members: &Members, names: &dyn Naming, singular: &'a str, plural: &'a str) -> &'a str {
    match members.members.as_slice() {
        [one] if !names.is_me(&one.mri) => singular,
        _ => plural,
    }
}

fn member_names(members: &Members, names: &dyn Naming) -> String {
    let called: Vec<String> = members
        .members
        .iter()
        .map(|member| {
            if names.is_me(&member.mri) {
                "You".to_owned()
            } else {
                names.name(&member.mri, member.name.as_deref())
            }
        })
        .collect();
    listed(&called)
}

fn joined(members: &Members, meeting: bool, names: &dyn Naming) -> String {
    let who = member_names(members, names);
    if members.temporary {
        format!(
            "{who} {} temporarily joined the chat.",
            verb(members, names, "has", "have")
        )
    } else if meeting {
        format!("{who} {} invited to the meeting.", verb(members, names, "was", "were"))
    } else {
        format!("{who} joined the conversation.")
    }
}

fn left(members: &Members, meeting: bool, names: &dyn Naming) -> String {
    let who = member_names(members, names);
    if meeting && !members.temporary {
        format!("{who} left the chat.")
    } else {
        format!(
            "{who} no longer {} access to the chat.",
            verb(members, names, "has", "have")
        )
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
        return Some(Shown::Line(format!(
            "{} joined the conversation.",
            actor(names, initiator)
        )));
    }
    // a lone bot added: Teams shows nothing
    if let [one] = targets.as_slice()
        && one.starts_with("28:")
    {
        return Some(Shown::Nothing);
    }
    let whom: Vec<String> = targets.iter().map(|target| object(names, target, None)).collect();
    Some(Shown::Line(format!(
        "{} added {} to the chat{}.",
        actor(names, initiator),
        listed(&whom),
        share_clause(activity)
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
    // removed by the chat itself
    if removed.initiator == conversation || is_thread(&removed.initiator) {
        return match whom.as_slice() {
            [] => Shown::Nothing,
            [one] => Shown::Line(format!("{one} was removed from the chat.")),
            _ => Shown::Line(format!("{} were removed from the chat.", listed(&whom))),
        };
    }
    let initiator = actor(names, &removed.initiator);
    if whom.is_empty() {
        return Shown::Line(format!("{initiator} left the chat."));
    }
    Shown::Line(format!("{initiator} removed {} from the chat.", listed(&whom)))
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
