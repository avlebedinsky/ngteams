//! Local search (Ctrl+Shift+F): the messages the account's store holds, found by their text as the user types, the
//! newest first; a hit chosen opens its chat at the message. The chat switcher (Ctrl+K) lists the same hits under its
//! chats ([`crate::switcher`]).
//!
//! - **Only the store** ([`Core::search`]): the engine's index of each stored message's plain text. The service's
//!   search (Substrate) is never asked: its answers list other tenant users, which the lab's rules never let through,
//!   and the app has no request path of its own anyway.
//! - **The query** is what the user typed, at least [`MIN_QUERY`] characters; the store quotes it, so nothing typed is
//!   read as search syntax. Each change asks again; an answer to an earlier query is dropped.
//! - **The hits** are those of the chats the chat list shows (a channel's or the account's own streams' are left
//!   out), at most [`MAX_HITS`]: the chat's name as the list has it, the sender (their name as the engine has it,
//!   "You" for the account), the time, and the text around the match with the matched terms marked.
//! - What it shows is traced (`search opened`/`closed`, `search shown` with the hits' conversation and message ids),
//!   never a query or a text.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use chrono::Local;
use gpui::{
    App, AppContext as _, AsyncApp, Context, Entity, EventEmitter, FontWeight, HighlightStyle, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, Styled as _, StyledText, Subscription, Task, Window, div,
    px, rgb,
};
use gpui_component::IndexPath;
use gpui_component::command::{Command, CommandItem, CommandState};
use teams_store::search::{MATCH_END, MATCH_START};

use crate::bridge::Core;
use crate::chat_list::ChatList;
use crate::clock::{self, HourCycle};
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};

/// The fewest characters (spaces left out) a query is looked for with.
pub const MIN_QUERY: usize = 2;
/// The most hits shown.
pub const MAX_HITS: usize = 50;
/// The search's width, px.
pub const WIDTH: f32 = 640.0;
/// Who the account's own messages are from.
pub const YOU: &str = "You";

/// Where a hit opens: its chat, at the message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HitTarget {
    pub conversation: String,
    pub message: String,
    /// The message's arrival time (ms): how far back the chat view reads to show it.
    pub arrival_time: i64,
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

/// The chats the list shows, `(id, name)`: what hits may open.
#[must_use]
pub fn listed_chats(list: &Entity<ChatList>, cx: &App) -> Vec<(String, SharedString)> {
    list.read(cx)
        .all_rows()
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

/// A hit's row: the chat, the sender and the time (on `cycle`'s clock) over the text around the match, the matched
/// terms marked.
#[must_use]
pub fn hit_row(hit: &Hit, palette: Palette, cycle: HourCycle) -> gpui::Div {
    let when = clock::list_time(hit.target.arrival_time, &Local::now(), cycle);
    let marked = HighlightStyle {
        font_weight: Some(FontWeight::BOLD),
        color: Some(rgb(palette.brand).into()),
        ..HighlightStyle::default()
    };
    let snippet =
        StyledText::new(hit.snippet.clone()).with_highlights(hit.matched.iter().map(|range| (range.clone(), marked)));
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .child(
            div()
                .flex()
                .gap_2()
                .text_xs()
                .text_color(rgb(palette.muted_foreground))
                .child(div().font_weight(FontWeight::BOLD).child(hit.chat.clone()))
                .child(hit.sender.clone())
                .child(div().flex_1())
                .child(when),
        )
        .child(div().text_sm().truncate().child(snippet))
}

/// A hit as an item of the kit's command palette.
#[must_use]
pub fn hit_item(hit: &Hit) -> CommandItem {
    let shown = hit.clone();
    CommandItem::new()
        .label(hit.snippet.clone())
        .child(move |_, cx| hit_row(&shown, theme::current(cx), clock::cycle(cx)))
}

/// What the search tells the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchEvent {
    /// Open this chat at this message.
    Open(HitTarget),
    /// Close the search.
    Dismissed,
}

/// The search (see the module docs): the kit's command palette, its own filter off, over the store's hits.
pub struct Search {
    state: Entity<CommandState>,
    core: Arc<dyn Core>,
    list: Entity<ChatList>,
    trace: Arc<ViewTrace>,
    query: String,
    hits: Vec<Hit>,
    /// The query the hits are of; `None` before any was looked for.
    found_for: Option<String>,
    /// The search running, for the latest query: a later query drops it.
    searching: Option<Task<()>>,
    /// Escape, before the palette's own binding (which first clears the query).
    _escape: Subscription,
}

impl EventEmitter<SearchEvent> for Search {}

impl Search {
    pub fn new(
        core: Arc<dyn Core>,
        list: Entity<ChatList>,
        trace: Arc<ViewTrace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let state = cx.new(|cx| CommandState::new(window, cx));
        let this = cx.weak_entity();
        let escape = cx.intercept_keystrokes(move |event, _window, cx| {
            let keystroke = &event.keystroke;
            if keystroke.key == "escape" && keystroke.modifiers.number_of_modifiers() == 0 {
                let dismissed = this.update(cx, |_, cx| cx.emit(SearchEvent::Dismissed));
                if dismissed.is_ok() {
                    cx.stop_propagation();
                }
            }
        });
        Self {
            state,
            core,
            list,
            trace,
            query: String::new(),
            hits: Vec::new(),
            found_for: None,
            searching: None,
            _escape: escape,
        }
    }

    /// Moves the keyboard focus to the query field.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.state.update(cx, |state, cx| state.focus(window, cx));
    }

    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The hits shown, the newest first.
    #[must_use]
    pub fn hits(&self) -> &[Hit] {
        &self.hits
    }

    /// Whether a search runs.
    #[must_use]
    pub fn searching(&self) -> bool {
        self.searching.is_some()
    }

    /// Looks for `query`: the hits shown are its once they come; a query too short shows none at once.
    pub fn set_query(&mut self, query: &str, cx: &mut Context<Self>) {
        query.clone_into(&mut self.query);
        if !searchable(query) {
            self.searching = None;
            self.show(None, Vec::new(), cx);
            return;
        }
        let core = Arc::clone(&self.core);
        let chats = listed_chats(&self.list, cx);
        let asked = query.to_owned();
        self.searching = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            let hits = find(&*core, asked.clone(), chats).await;
            let _ = this.update(cx, |this, cx| {
                this.searching = None;
                this.show(Some(asked), hits, cx);
            });
        }));
    }

    fn show(&mut self, found_for: Option<String>, hits: Vec<Hit>, cx: &mut Context<Self>) {
        let changed = self.hits != hits || (found_for.is_some() && self.found_for != found_for);
        self.found_for = found_for;
        self.hits = hits;
        if changed {
            trace_hits(&self.trace, "search", &self.hits);
        }
        cx.notify();
    }

    fn confirm(&mut self, index: IndexPath, cx: &mut Context<Self>) {
        if let Some(hit) = self.hits.get(index.row) {
            cx.emit(SearchEvent::Open(hit.target.clone()));
        }
    }

    /// What the list says while it has no hit.
    fn empty_text(&self) -> &'static str {
        if !searchable(&self.query) {
            "Type at least two characters to search your messages"
        } else if self.searching.is_some() {
            "Searching…"
        } else {
            "No message matches"
        }
    }
}

impl Render for Search {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let on_query = cx.weak_entity();
        let on_confirm = cx.weak_entity();
        let empty: SharedString = self.empty_text().into();
        let command = Command::new(&self.state)
            .filterable(false)
            .placeholder("Search messages")
            .items(self.hits.iter().map(hit_item))
            .on_query(move |query, _, cx| {
                // a search already closed has nothing to look for
                let _ = on_query.update(cx, |this, cx| this.set_query(query, cx));
            })
            .on_confirm(move |index, _, cx| {
                let _ = on_confirm.update(cx, |this, cx| this.confirm(index, cx));
            })
            .empty(move |_, _, _| div().p_2().child(empty.clone()));
        div()
            .debug_selector(|| "search".to_owned())
            .w(px(WIDTH))
            .rounded_md()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .child(command)
    }
}
