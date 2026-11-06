//! Local search: the messages the account's store holds, found by their text, the newest first, for the title bar's
//! search ([`crate::global_search`]), whose results show them beside the service's.
//!
//! - **Only the store** ([`Core::search`]): the engine's index of each stored message's plain text.
//! - **The query** is what the user typed, at least [`MIN_QUERY`] characters; the store quotes it, so nothing typed is
//!   read as search syntax.
//! - **The hits** are those of the chats the chat list shows (a channel's or the account's own streams' are left
//!   out), at most [`MAX_HITS`]: the chat's name as the list has it, the sender (their name as the engine has it,
//!   "You" for the account), the time, and the text around the match with the matched terms marked.
//! - What the results show is traced ([`trace_hits`]: the hits' conversation and message ids), never a query or a text.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use gpui::{App, Entity, SharedString};
use teams_store::search::{MATCH_END, MATCH_START};

use crate::bridge::Core;
use crate::chat_list::{ChatList, RowKind};
use crate::trace::{TraceLine, ViewTrace};

/// The fewest characters (spaces left out) a query is looked for with.
pub const MIN_QUERY: usize = 2;
/// The most hits shown.
pub const MAX_HITS: usize = 50;
/// Who the account's own messages are from.
pub const YOU: &str = "You";

/// Where a hit opens: its chat, at the message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HitTarget {
    pub conversation: String,
    pub message: String,
    /// The message's arrival time (ms): how far back the chat view reads to show it.
    pub arrival_time: i64,
    /// In a channel, the thread it is in (its post's id), the service's search says: where the channel view opens.
    pub thread: Option<String>,
}

/// One hit as the views show it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub target: HitTarget,
    /// The chat's name, as the chat list has it.
    pub chat: SharedString,
    /// Who sent it.
    pub sender: SharedString,
    /// The text around the match, without its marks.
    pub snippet: SharedString,
    /// Where the matched terms are in `snippet` (byte ranges).
    pub matched: Vec<Range<usize>>,
}

/// Whether `query` is long enough to be looked for ([`MIN_QUERY`]).
#[must_use]
pub fn searchable(query: &str) -> bool {
    query.chars().filter(|ch| !ch.is_whitespace()).count() >= MIN_QUERY
}

/// A store snippet's text without its match marks, and where the marked matches are in it.
#[must_use]
pub fn split_snippet(snippet: &str) -> (String, Vec<Range<usize>>) {
    let mut text = String::with_capacity(snippet.len());
    let mut matched = Vec::new();
    let mut start = None;
    for ch in snippet.chars() {
        match ch {
            MATCH_START => start = Some(text.len()),
            MATCH_END => {
                if let Some(from) = start.take()
                    && from < text.len()
                {
                    matched.push(from..text.len());
                }
            }
            _ => text.push(ch),
        }
    }
    (text, matched)
}

/// The hits of `query` in the chats of `chats` (`(id, name)`, as the list shows them), at most [`MAX_HITS`], the
/// newest first; none for a query too short or when the engine can't answer. The store looks in these chats only, so
/// however many messages of the others match (a busy channel's), they never take the hits' places.
pub async fn find(core: &dyn Core, query: String, chats: Vec<(String, SharedString)>) -> Vec<Hit> {
    if !searchable(&query) {
        return Vec::new();
    }
    let within: Vec<String> = chats.iter().map(|(id, _)| id.clone()).collect();
    let Ok(found) = core.search(query, within, MAX_HITS).await else {
        return Vec::new();
    };
    let names: HashMap<String, SharedString> = chats.into_iter().collect();
    let mut senders: HashMap<String, SharedString> = HashMap::new();
    let mut hits = Vec::new();
    for hit in found {
        let Some(chat) = names.get(&hit.conversation_id) else {
            continue;
        };
        let sender = match &hit.sender {
            Some(mri) if mri == core.me() => SharedString::from(YOU),
            Some(mri) => {
                if let Some(name) = senders.get(mri) {
                    name.clone()
                } else {
                    let person = core.person(mri.clone()).await.ok();
                    let name: SharedString = person
                        .and_then(|person| person.name)
                        .or_else(|| hit.sender_name.clone())
                        .unwrap_or_else(|| mri.clone())
                        .into();
                    senders.insert(mri.clone(), name.clone());
                    name
                }
            }
            None => hit.sender_name.clone().unwrap_or_default().into(),
        };
        let (snippet, matched) = split_snippet(&hit.snippet);
        hits.push(Hit {
            target: HitTarget {
                conversation: hit.conversation_id,
                message: hit.message_id,
                arrival_time: hit.arrival_time,
                thread: None,
            },
            chat: chat.clone(),
            sender,
            snippet: snippet.into(),
            matched,
        });
        if hits.len() == MAX_HITS {
            break;
        }
    }
    hits
}

/// The longest text a translated message's card shows, in characters.
pub const SNIPPET_CHARS: usize = 200;

/// A translated message's text as its card shows it: on one line, at most [`SNIPPET_CHARS`] characters.
#[must_use]
pub fn snippet_line(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= SNIPPET_CHARS {
        return line;
    }
    let mut cut: String = line.chars().take(SNIPPET_CHARS - 1).collect();
    cut.push('…');
    cut
}

/// Where the words of `query` are in `text` (byte ranges, in order, ignoring case): what a translated message's card
/// marks, its match having been in the message as written.
#[must_use]
pub fn marked_terms(text: &str, query: &str) -> Vec<Range<usize>> {
    let lower = text.to_lowercase();
    // lowercasing may change lengths: only a text whose length it keeps is marked
    if lower.len() != text.len() {
        return Vec::new();
    }
    let mut matched: Vec<Range<usize>> = Vec::new();
    for term in query
        .split_whitespace()
        .map(str::to_lowercase)
        .filter(|term| !term.is_empty())
    {
        let mut from = 0;
        while let Some(at) = lower[from..].find(&term) {
            let start = from + at;
            let end = start + term.len();
            if text.is_char_boundary(start) && text.is_char_boundary(end) {
                matched.push(start..end);
            }
            from = end;
        }
    }
    matched.sort_by_key(|range| range.start);
    matched.dedup_by(|later, earlier| later.start < earlier.end);
    matched
}

/// The chats the account can find, listed or left out as empty, `(id, name)`: what hits may open (a favourite channel
/// isn't a chat).
#[must_use]
pub fn listed_chats(list: &Entity<ChatList>, cx: &App) -> Vec<(String, SharedString)> {
    list.read(cx)
        .findable_rows()
        .filter(|row| row.kind != RowKind::Channel)
        .map(|row| (row.id.clone(), row.name.clone()))
        .collect()
}

/// Traces the hits shown: their conversations and message ids, never a text.
pub fn trace_hits(trace: &ViewTrace, view: &'static str, hits: &[Hit]) {
    let mut conversations: Vec<&str> = hits
        .iter()
        .map(|hit| hit.target.conversation.as_str())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    conversations.sort_unstable();
    trace.record(
        &TraceLine::new(view, "shown")
            .conversations(conversations)
            .messages(hits.iter().map(|hit| hit.target.message.as_str())),
    );
}
