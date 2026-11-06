//! A meeting chat's recap card, as Teams draws it under the meeting's end ("Meeting ended: 1h 1m 37s"), worked out from
//! the chat's own messages (no GPUI here: `crate::chat_view` draws it, and no read is made for it but the recording's
//! thumbnail and its expiry):
//!
//! - **What the meeting left**, by its call (`<callId>` of its end, `<Id type="callId">` of a recording's messages,
//!   `callId` of a transcript's): each recording ready (its sharing link, file, length and transcript), whether it was
//!   transcribed (a transcript, a recording with its transcript, a transcript alone), and whether a recording is still
//!   being saved (more stopped than ready). A meeting that left none of them has no card.
//! - **Its line**: the meeting's day and span on the account's clock ("Monday, October 19, 2026 6:30 PM - 7:00 PM"):
//!   its scheduled times when its end gives them, else its first recording's (its start and length), else its
//!   transcript's, else the call's own start and end.
//! - **Its picture**: the first recording's thumbnail, with its length ("28m 38s"), "2 recordings", or "Saving…" (and
//!   no thumbnail) while one is saved; under it "Expires in 103 days" (the recording's item's expiry, the days counted
//!   up), or "Set to expire" for several.
//! - **Its actions**: View recap (the Recap tab, at the first recording), the Transcript chip (the same, its transcript
//!   shown), the thumbnail (the recording played there) and Share → Copy link (the recap's Teams link, as the Recap
//!   tab's Share copies it).
//!
//! It is one card per meeting so a later card part (an AI summary) can be added to [`RecapCard`] beside the others.

use std::collections::HashMap;

use chrono::{DateTime, TimeZone};
use teams_proto::chat::{ChatEvent, RecordingContents, RecordingItem, RecordingStatus, chat_event, meeting_times};
use teams_proto::recap::{RecapPart, recap_link};
use teams_store::Message;

use crate::clock::HourCycle;
use crate::system_message::{days_left, duration_text};

/// The card's section label.
pub const CONTENT: &str = "Content";
/// The thumbnail's badge while a recording is saved.
pub const SAVING: &str = "Saving\u{2026}";
/// Under the thumbnail of several recordings that expire.
pub const SET_TO_EXPIRE: &str = "Set to expire";
/// The Transcript chip's words.
pub const TRANSCRIPT: &str = "Transcript";

/// A recording ready, as the card holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardRecording {
    /// Its sharing link: where the recap plays it, and its thumbnail's.
    pub link: String,
    /// Its file: where its expiry is read.
    pub file: Option<RecordingItem>,
    /// Its length, in seconds.
    pub duration: Option<u64>,
    /// When it began, ms since the epoch.
    pub started: Option<i64>,
    /// Its transcript's address, when it has one.
    pub transcript: Option<String>,
}

/// A meeting's recap card.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecapCard {
    /// The call it is of.
    pub call_id: Option<String>,
    /// The meeting's title as its recording names it (the chat's name comes first).
    pub title: Option<String>,
    /// The span its line shows, ms since the epoch.
    pub span: Option<(i64, i64)>,
    /// Its recordings ready, the earliest first.
    pub recordings: Vec<CardRecording>,
    /// Whether it was transcribed.
    pub transcript: bool,
    /// Whether a recording of it is still being saved.
    pub saving: bool,
    /// The meeting's organizer (an MRI) and their tenant.
    pub organizer: Option<String>,
    pub tenant: Option<String>,
    /// A transcript alone (a meeting transcribed and not recorded): its item's sharing link, file and address.
    pub transcribed: Option<CardRecording>,
}

impl RecapCard {
    /// Whether it is drawn: the meeting left a recording or a transcript, or a recording is being saved.
    #[must_use]
    pub fn shows(&self) -> bool {
        !self.recordings.is_empty() || self.transcript || self.saving
    }

    /// The recording View recap and the thumbnail open: the first.
    #[must_use]
    pub fn first_recording(&self) -> Option<&CardRecording> {
        self.recordings.first()
    }

    /// Whether it has a picture (a recording, ready or being saved).
    #[must_use]
    pub fn has_picture(&self) -> bool {
        !self.recordings.is_empty() || self.saving
    }

    /// The thumbnail's badge: "Saving…" while a recording is saved, "2 recordings" for several, else the recording's
    /// length.
    #[must_use]
    pub fn badge(&self) -> Option<String> {
        if self.saving {
            return Some(SAVING.to_owned());
        }
        match self.recordings.as_slice() {
            [] => None,
            [one] => one.duration.map(duration_text),
            several => Some(format!("{} recordings", several.len())),
        }
    }

    /// The line under the thumbnail once the first recording's expiry (`expires`, ms) is known: "Set to expire" for
    /// several recordings, "Expires in n days" for one ("Expires in 1 day"), "Expired" once past.
    #[must_use]
    pub fn expiry_line(&self, expires: i64, now: i64) -> String {
        if self.recordings.len() > 1 && !self.saving {
            return SET_TO_EXPIRE.to_owned();
        }
        match days_left(expires, now) {
            Some(1) => "Expires in 1 day".to_owned(),
            Some(days) => format!("Expires in {days} days"),
            None => "Expired".to_owned(),
        }
    }

    /// The part of the recap Share's link names: the first recording's, else the transcript alone's.
    fn part(&self) -> Option<RecapPart> {
        let (shown, recorded) = match self.recordings.first() {
            Some(recording) => (recording, true),
            None => (self.transcribed.as_ref()?, false),
        };
        let file = shown.file.as_ref()?;
        Some(RecapPart {
            recording: recorded.then(|| shown.link.clone()),
            transcript: shown.transcript.clone(),
            sharing_link: Some(shown.link.clone()),
            call_id: self.call_id.clone(),
            drive_id: Some(file.drive.clone()),
            item_id: Some(file.item.clone()),
            start_ms: shown.started,
            end_ms: None,
            organizer_id: self
                .organizer
                .as_deref()
                .map(|organizer| organizer.strip_prefix("8:orgid:").unwrap_or(organizer).to_owned()),
            tenant_id: self.tenant.clone(),
        })
    }

    /// The link Share → Copy link copies: the recap's Teams link for the meeting whose chat is `thread`, as the Recap
    /// tab's Share copies it; `None` without a recording's or a transcript's file.
    #[must_use]
    pub fn share_link(&self, thread: &str) -> Option<String> {
        recap_link(&self.part()?, thread)
    }

    /// Its line: the span's day and times on `cycle`'s clock in `zone`.
    #[must_use]
    pub fn span_line<Tz: TimeZone>(&self, zone: &Tz, cycle: HourCycle) -> Option<String>
    where
        Tz::Offset: std::fmt::Display,
    {
        let (start, end) = self.span?;
        let local = |ms: i64| DateTime::from_timestamp_millis(ms).map(|at| at.with_timezone(zone));
        let (start, end) = (local(start)?, local(end)?);
        Some(format!(
            "{} {} - {}",
            start.format("%A, %B %-d, %Y"),
            start.format(cycle.time_of_day()),
            end.format(cycle.time_of_day())
        ))
    }
}

/// What one call's messages said, as they are read.
#[derive(Default)]
struct Gathered {
    card: RecapCard,
    scheduled: Option<(i64, i64)>,
    started_at: Option<i64>,
    ended_at: Option<i64>,
    stopped: usize,
}

/// Each meeting's recap card in a meeting chat's `messages` (in any order), by the call it is of (`None`: a call that
/// names none, as a shape no capture showed would).
#[must_use]
pub fn recap_cards(messages: &[Message]) -> HashMap<Option<String>, RecapCard> {
    let mut ordered: Vec<&Message> = messages
        .iter()
        .filter(|message| message.delete_time.is_none())
        .collect();
    ordered.sort_by_key(|message| message.arrival_time);
    let mut calls: HashMap<Option<String>, Gathered> = HashMap::new();
    for message in ordered {
        gather(message, &mut calls);
    }
    calls
        .into_iter()
        .map(|(call, gathered)| {
            let mut card = settled(gathered);
            card.call_id.clone_from(&call);
            (call, card)
        })
        .collect()
}

fn gather(message: &Message, calls: &mut HashMap<Option<String>, Gathered>) {
    match chat_event(&message.message_type, &message.content) {
        Some(ChatEvent::CallStarted(_)) => {
            let times = meeting_times(&message.content);
            let call = calls.entry(times.and_then(|times| times.call_id)).or_default();
            call.started_at.get_or_insert(message.arrival_time);
        }
        Some(ChatEvent::CallEnded(_)) => {
            let times = meeting_times(&message.content);
            let scheduled = times.as_ref().and_then(|times| times.scheduled);
            let call = calls.entry(times.and_then(|times| times.call_id)).or_default();
            call.ended_at = Some(message.arrival_time);
            // a meeting not scheduled gives the year 1 (`01/01/0001`): no span
            call.scheduled = scheduled.filter(|(start, _)| *start > 0).or(call.scheduled);
        }
        Some(ChatEvent::Transcript(transcript)) => {
            calls.entry(transcript.call_id).or_default().card.transcript = true;
        }
        Some(ChatEvent::Recording(recording)) => {
            let call = calls.entry(recording.call_id.clone()).or_default();
            let card = &mut call.card;
            card.title = card.title.take().or_else(|| recording.title.clone());
            card.organizer = card.organizer.take().or_else(|| recording.organizer.clone());
            card.tenant = card.tenant.take().or_else(|| recording.tenant.clone());
            let ready = || {
                Some(CardRecording {
                    link: recording.link.clone()?,
                    file: recording.file.clone(),
                    duration: recording.duration,
                    started: recording.started,
                    transcript: recording.transcript.clone(),
                })
            };
            match (recording.contents, recording.status) {
                (RecordingContents::Transcript, RecordingStatus::Ready) => {
                    card.transcript = true;
                    if card.transcribed.is_none() {
                        card.transcribed = ready();
                    }
                }
                (RecordingContents::Transcript, _) | (_, RecordingStatus::Started) => {}
                (contents, RecordingStatus::Ready) => {
                    card.transcript |= contents == RecordingContents::RecordingAndTranscript;
                    if let Some(ready) = ready()
                        && !card.recordings.iter().any(|known| known.link == ready.link)
                    {
                        card.recordings.push(ready);
                    }
                }
                (_, RecordingStatus::Stopped) => call.stopped += 1,
            }
        }
        _ => {}
    }
}

fn settled(mut gathered: Gathered) -> RecapCard {
    let card = &mut gathered.card;
    card.recordings
        .sort_by_key(|recording| recording.started.unwrap_or(i64::MAX));
    card.saving = gathered.stopped > card.recordings.len();
    let span_of = |recording: &CardRecording| {
        let start = recording.started?;
        let length = i64::try_from(recording.duration?).ok()?;
        Some((start, start.saturating_add(length.saturating_mul(1_000))))
    };
    card.span = gathered
        .scheduled
        .or_else(|| card.recordings.first().and_then(span_of))
        .or_else(|| card.transcribed.as_ref().and_then(span_of))
        .or_else(|| gathered.started_at.zip(gathered.ended_at));
    gathered.card
}
