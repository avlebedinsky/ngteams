//! What a meeting chat's Recap and Shared tabs show, as Teams' do, worked out from what the engine read (no GPUI here:
//! [`crate::recap_view`] and [`crate::shared_view`] draw it).
//!
//! - **The recap's parts** ([`teams_proto::recap::recap_parts`]): each recording with its transcript, then a transcript
//!   of a meeting that wasn't recorded, the earliest first. Two parts or more show as "Part 1", "Part 2" beside the
//!   meeting's line ([`session_line`]: its day and the span of its parts, "Thursday, November 5, 2026 11:14 AM -
//!   11:18 AM"). A part with a recording plays it; one without shows the shared files and the panes across the
//!   whole width, and offers no "Watch in browser". A 1:1 or group chat's parts are its calls' ([`sessions`]): each
//!   call a session of the recap's menu with its own line, its parts beside it.
//! - **The transcript** ([`Transcript`]): its turns ([`teams_proto::transcript::turns`]), the paragraph being spoken at
//!   the player's time (highlighted, as Teams highlights it), and a search ([`Search`]): its matches in order, the
//!   current one ("1/3"), the next and the previous, wrapping round.
//! - **What was shared in the chat** ([`shared_items`]): the files its messages carry and the links they hold, the
//!   newest first, with who shared them and when: the Shared tab's list, and the recap's "Shared files" (files only).

use std::time::Duration;

use chrono::{DateTime, Local, TimeZone};
use teams_proto::chat::{
    ChatEvent, Node, RecordingContents, RecordingStatus, SharedFile, chat_event, parse_html, shared_files,
};
use teams_proto::recap::{RecapPart, recap_span};
use teams_proto::transcript::{Turn, find, paragraph_at, read_vtt, turns};
use teams_store::Message;

use crate::clock::HourCycle;

/// The note over a transcript, as Teams words it.
pub const AI_NOTICE: &str = "AI-generated content may be incorrect";
/// The shared files' card when the chat shared none.
pub const NO_FILES: &str = "No files were shared.";

/// The meeting's line: its day and its span on the account's clock, "Thursday, November 5, 2026 11:14 AM - 11:18
/// AM"; `None` when its parts give no start or no end (the recap shows the meeting's name instead).
#[must_use]
pub fn session_line<Tz: TimeZone>(parts: &[RecapPart], zone: &Tz, cycle: HourCycle) -> Option<String>
where
    Tz::Offset: std::fmt::Display,
{
    let (start, end) = recap_span(parts)?;
    let local = |ms: i64| DateTime::from_timestamp_millis(ms).map(|at| at.with_timezone(zone));
    let (start, end) = (local(start)?, local(end)?);
    Some(format!(
        "{} {} - {}",
        start.format("%A, %B %-d, %Y"),
        start.format(cycle.time_of_day()),
        end.format(cycle.time_of_day())
    ))
}

/// [`session_line`] in the local zone.
#[must_use]
pub fn local_session_line(parts: &[RecapPart], cycle: HourCycle) -> Option<String> {
    session_line(parts, &Local, cycle)
}

/// The recap's sessions as Teams' session menu lists them, the newest first, each the indices of its parts in order.
/// A meeting chat's parts are its meeting's one session; a 1:1 or group chat's (`by_call`) are its calls', a session
/// for each call (a part that names no call, as a transcript saved on this computer, one of its own), Teams' 1:1 Recap
/// listing each call recorded by its own day and span.
#[must_use]
pub fn sessions(parts: &[RecapPart], by_call: bool) -> Vec<Vec<usize>> {
    if parts.is_empty() {
        return Vec::new();
    }
    if !by_call {
        return vec![(0..parts.len()).collect()];
    }
    let mut sessions: Vec<(Option<&str>, Vec<usize>)> = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        let call = part.call_id.as_deref();
        match sessions.iter_mut().find(|(of, _)| call.is_some() && *of == call) {
            Some((_, indices)) => indices.push(index),
            None => sessions.push((call, vec![index])),
        }
    }
    // the parts come the earliest first: the session last begun first
    sessions.into_iter().rev().map(|(_, indices)| indices).collect()
}

/// The line of the session made of `indices` of `parts` ([`session_line`] of its parts) in the local zone.
#[must_use]
pub fn local_line_of(parts: &[RecapPart], indices: &[usize], cycle: HourCycle) -> Option<String> {
    let chosen: Vec<RecapPart> = indices.iter().filter_map(|&index| parts.get(index).cloned()).collect();
    local_session_line(&chosen, cycle)
}

/// A part's name in the parts' row, counted from 1.
#[must_use]
pub fn part_label(index: usize) -> String {
    format!("Part {}", index + 1)
}

/// Whether the parts' row shows: a meeting of two parts or more.
#[must_use]
pub fn shows_parts(parts: &[RecapPart]) -> bool {
    parts.len() > 1
}

/// A transcript as the recap's pane shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Transcript {
    pub turns: Vec<Turn>,
}

impl Transcript {
    /// The transcript the `WebVTT` text `vtt` holds; `None` when it holds none.
    #[must_use]
    pub fn of(vtt: &str) -> Option<Self> {
        Some(Self {
            turns: turns(&read_vtt(vtt)?),
        })
    }

    /// The paragraph (turn, paragraph) being spoken at `at`.
    #[must_use]
    pub fn spoken_at(&self, at: Duration) -> Option<(usize, usize)> {
        paragraph_at(&self.turns, at)
    }
}

/// One place a search found its words: the paragraph (turn, paragraph) and where in its text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    pub turn: usize,
    pub paragraph: usize,
    pub range: std::ops::Range<usize>,
}

/// The transcript pane's row of the paragraph `at` (turn, paragraph): the note and who started it come first, then each
/// turn's header and its paragraphs, a row each.
#[must_use]
pub fn row_of(turns: &[Turn], at: (usize, usize)) -> usize {
    let before: usize = turns.iter().take(at.0).map(|turn| 1 + turn.paragraphs.len()).sum();
    2 + before + 1 + at.1
}

/// A search in a transcript: its query, every place its words stand (each counted, as Teams counts them) and the one
/// shown now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Search {
    pub query: String,
    pub matches: Vec<Match>,
    /// The match shown now, an index into `matches`.
    pub current: usize,
}

impl Search {
    /// The search for `query` in `transcript`, at its first match.
    #[must_use]
    pub fn new(transcript: &Transcript, query: &str) -> Self {
        let query = query.trim().to_owned();
        let matches = find(&transcript.turns, &query)
            .into_iter()
            .flat_map(|(turn, paragraph)| {
                let text = &transcript.turns[turn].paragraphs[paragraph].text;
                matched_ranges(text, &query)
                    .into_iter()
                    .map(move |range| Match { turn, paragraph, range })
            })
            .collect();
        Self {
            query,
            matches,
            current: 0,
        }
    }

    /// Where in the paragraph (turn, paragraph) the match shown now stands, when it is there.
    #[must_use]
    pub fn current_range(&self, turn: usize, paragraph: usize) -> Option<std::ops::Range<usize>> {
        self.matches
            .get(self.current)
            .filter(|shown| shown.turn == turn && shown.paragraph == paragraph)
            .map(|shown| shown.range.clone())
    }

    /// "1/3": the match shown and how many there are; "0/0" for none.
    #[must_use]
    pub fn label(&self) -> String {
        if self.matches.is_empty() {
            "0/0".to_owned()
        } else {
            format!("{}/{}", self.current + 1, self.matches.len())
        }
    }

    /// The paragraph of the match shown now.
    #[must_use]
    pub fn shown(&self) -> Option<(usize, usize)> {
        self.matches
            .get(self.current)
            .map(|shown| (shown.turn, shown.paragraph))
    }

    /// Goes to the next match, after the last the first.
    pub fn next(&mut self) {
        if !self.matches.is_empty() {
            self.current = (self.current + 1) % self.matches.len();
        }
    }

    /// Goes to the previous match, before the first the last.
    pub fn previous(&mut self) {
        if !self.matches.is_empty() {
            self.current = (self.current + self.matches.len() - 1) % self.matches.len();
        }
    }
}

/// Where `query` stands in `text`, case aside, as byte ranges of `text` (none for a blank query): what a match's
/// highlight covers.
#[must_use]
pub fn matched_ranges(text: &str, query: &str) -> Vec<std::ops::Range<usize>> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    let lower = text.to_lowercase();
    let wanted = query.to_lowercase();
    // a text whose lowercase is another length (some scripts) is matched as written
    let (haystack, needle) = if lower.len() == text.len() {
        (lower.as_str(), wanted.as_str())
    } else {
        (text, query)
    };
    haystack
        .match_indices(needle)
        .map(|(at, found)| at..at + found.len())
        .filter(|range| text.is_char_boundary(range.start) && text.is_char_boundary(range.end))
        .collect()
}

/// What a chat shared: a file a message carries, or a link one holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SharedKind {
    File(SharedFile),
    Link,
}

/// One thing shared in a chat, as the Shared tab lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedItem {
    pub kind: SharedKind,
    /// The name shown: a file's name, a link's text (else the link).
    pub name: String,
    /// Where it opens.
    pub url: String,
    /// When it was shared, ms since the epoch.
    pub time: i64,
    /// Who shared it (their MRI), when the message says.
    pub sender: Option<String>,
    /// The name the message carried for them.
    pub sender_name: Option<String>,
}

impl SharedItem {
    /// Whether it is a file.
    #[must_use]
    pub fn is_file(&self) -> bool {
        matches!(self.kind, SharedKind::File(_))
    }
}

/// The files and links `messages` shared, the newest first (a deleted message's none), each link once per message.
#[must_use]
pub fn shared_items(messages: &[Message]) -> Vec<SharedItem> {
    let mut items: Vec<SharedItem> = messages
        .iter()
        .filter(|message| message.delete_time.is_none() && !message.content.is_empty())
        .flat_map(message_items)
        .collect();
    items.sort_by_key(|item| std::cmp::Reverse(item.time));
    items
}

fn message_items(message: &Message) -> Vec<SharedItem> {
    let item = |kind: SharedKind, name: String, url: String| SharedItem {
        kind,
        name,
        url,
        time: message.arrival_time,
        sender: message.sender.clone(),
        sender_name: message.sender_name.clone(),
    };
    let properties: Option<serde_json::Value> = message
        .properties
        .as_deref()
        .and_then(|properties| serde_json::from_str(properties).ok());
    let mut items: Vec<SharedItem> = shared_files(properties.as_ref().and_then(|properties| properties.get("files")))
        .files
        .into_iter()
        .filter_map(|file| {
            let url = file.open_link()?.to_owned();
            Some(item(SharedKind::File(file.clone()), file.name, url))
        })
        .collect();
    // a recording ready is shared as its link, as Teams lists it (the link its name)
    if let Some(ChatEvent::Recording(recording)) = chat_event(&message.message_type, &message.content)
        && recording.status == RecordingStatus::Ready
        && let Some(link) = recording.link
    {
        items.push(item(SharedKind::Link, link.clone(), link));
    }
    // a text message's links; another system message's record holds none a person shared
    if matches!(message.message_type.as_str(), "RichText/Html" | "Text") {
        let mut links = Vec::new();
        collect_links(&parse_html(&message.content).nodes, &mut links);
        let files: Vec<String> = items.iter().map(|shared| shared.url.clone()).collect();
        for (href, text) in links {
            let known = files.contains(&href) || items.iter().any(|shared| shared.url == href);
            if known || !(href.starts_with("https://") || href.starts_with("http://")) {
                continue;
            }
            let name = if text.trim().is_empty() {
                href.clone()
            } else {
                text.trim().to_owned()
            };
            items.push(item(SharedKind::Link, name, href));
        }
    }
    items
}

/// Every link in `nodes`, with its text, in order.
fn collect_links(nodes: &[Node], links: &mut Vec<(String, String)>) {
    for node in nodes {
        match node {
            Node::Link { href, children } => {
                let mut text = String::new();
                plain(children, &mut text);
                links.push((href.clone(), text));
            }
            Node::Paragraph(children)
            | Node::Span { children, .. }
            | Node::Heading { children, .. }
            | Node::Indented { children, .. } => collect_links(children, links),
            Node::Quote(_)
            | Node::List(_)
            | Node::Code(_)
            | Node::Text { .. }
            | Node::Break
            | Node::Mention { .. }
            | Node::Image(_)
            | Node::Rule => {}
        }
    }
}

fn plain(nodes: &[Node], into: &mut String) {
    for node in nodes {
        match node {
            Node::Text { text, .. } | Node::Mention { text, .. } => into.push_str(text),
            Node::Link { children, .. }
            | Node::Paragraph(children)
            | Node::Span { children, .. }
            | Node::Heading { children, .. }
            | Node::Indented { children, .. } => plain(children, into),
            Node::Break => into.push(' '),
            Node::Quote(_) | Node::List(_) | Node::Code(_) | Node::Image(_) | Node::Rule => {}
        }
    }
}

/// Whether a recording of the chat is still being saved: more recordings stopped than ready (a recording's
/// transcript half aside). MCPS lists a recording only once it is saved.
#[must_use]
pub fn recording_saving(messages: &[Message]) -> bool {
    let (mut stopped, mut ready) = (0_usize, 0_usize);
    for message in messages {
        if let Some(ChatEvent::Recording(recording)) = chat_event(&message.message_type, &message.content)
            && recording.contents != RecordingContents::Transcript
        {
            match recording.status {
                RecordingStatus::Stopped => stopped += 1,
                RecordingStatus::Ready => ready += 1,
                RecordingStatus::Started => {}
            }
        }
    }
    stopped > ready
}

/// What the Shared tab shows: everything, files, or links.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SharedFilter {
    #[default]
    Recent,
    Files,
    Links,
}

impl SharedFilter {
    /// Its pill's words.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Recent => "Recent",
            Self::Files => "Files",
            Self::Links => "Links",
        }
    }

    /// The selectors' name for it.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Recent => "recent",
            Self::Files => "files",
            Self::Links => "links",
        }
    }
}

/// The items the Shared tab lists under `filter` whose name or link holds `keyword` (case aside).
#[must_use]
pub fn filtered<'a>(items: &'a [SharedItem], filter: SharedFilter, keyword: &str) -> Vec<&'a SharedItem> {
    let keyword = keyword.trim().to_lowercase();
    items
        .iter()
        .filter(|item| match filter {
            SharedFilter::Recent => true,
            SharedFilter::Files => item.is_file(),
            SharedFilter::Links => !item.is_file(),
        })
        .filter(|item| {
            keyword.is_empty()
                || item.name.to_lowercase().contains(&keyword)
                || item.url.to_lowercase().contains(&keyword)
        })
        .collect()
}

/// The transcript's file name for a download: the meeting's name, `.vtt`, without the characters a file name can't
/// hold.
#[must_use]
pub fn transcript_file_name(title: &str) -> String {
    file_name(title, "Transcript", "vtt")
}

/// A recording's file name for a download when its item gives none: the meeting's name, `.mp4`, without the
/// characters a file name can't hold.
#[must_use]
pub fn recording_file_name(title: &str) -> String {
    file_name(title, "Recording", "mp4")
}

/// `title` made a file name with `extension` (`fallback` for a blank one).
fn file_name(title: &str, fallback: &str, extension: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|character| {
            if matches!(character, '/' | '\\' | '\0') {
                '-'
            } else {
                character
            }
        })
        .collect();
    let cleaned = cleaned.trim();
    format!("{}.{extension}", if cleaned.is_empty() { fallback } else { cleaned })
}
