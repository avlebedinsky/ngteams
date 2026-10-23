//! The chat switcher (Ctrl+K): the UI kit's command palette over the chat list's rows, matched fuzzily on their names
//! as the user types ([`fuzzy_score`]): Enter opens the chat highlighted, Escape closes the switcher whatever was typed.
//! It is the live test's way into a chat: the keyboard, no screen coordinates.
//!
//! It reads the chat list's rows (one model for both), so a chat that changes while it is open shows changed. Under
//! the chats it lists the messages the query finds in the store ([`crate::search`]): one chosen opens its chat at the
//! message. Last come the app's commands the query matches ([`COMMAND_NAMES`]: "Copy diagnostics").

use std::sync::Arc;

use gpui::{
    App, AppContext as _, AsyncApp, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Task, Window, div, px, rgb,
};
use gpui_component::IndexPath;
use gpui_component::command::{Command, CommandGroup, CommandItem, CommandState};

use crate::bridge::Core;
use crate::chat_list::ChatList;
use crate::search::{self, Hit, HitTarget};
use crate::theme;

/// The most matches the switcher lists.
pub const MAX_MATCHES: usize = 50;
/// The switcher's width, px.
pub const WIDTH: f32 = 520.0;

/// How well `query` matches `name` (higher is better), or `None` when it doesn't: every character of the query
/// (spaces left out, any case) must appear in the name in order. Characters that follow each other, ones at the start
/// of a word and a match at the name's start count more.
#[must_use]
pub fn fuzzy_score(query: &str, name: &str) -> Option<u32> {
    let wanted: Vec<char> = query
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if wanted.is_empty() {
        return Some(0);
    }
    let chars: Vec<char> = name.chars().flat_map(char::to_lowercase).collect();
    // the best score of the query so far with its last character matched at each place of the name
    let mut best: Vec<Option<u32>> = chars
        .iter()
        .enumerate()
        .map(|(at, &ch)| (ch == wanted[0]).then(|| 1 + word_start(&chars, at)))
        .collect();
    for &want in &wanted[1..] {
        best = (0..chars.len())
            .map(|at| (chars[at] == want).then(|| after(&best, at)).flatten())
            .map(|score| score.map(|(score, at)| score + 1 + word_start(&chars, at)))
            .collect();
    }
    best.into_iter().flatten().max()
}

/// What a match at `at` gains for starting a word.
fn word_start(chars: &[char], at: usize) -> u32 {
    if at == 0 || !chars[at - 1].is_alphanumeric() {
        3
    } else {
        0
    }
}

/// The best score of a match before `at`, with what following it right away gains, and `at`.
fn after(best: &[Option<u32>], at: usize) -> Option<(u32, usize)> {
    best[..at]
        .iter()
        .enumerate()
        .filter_map(|(before, score)| score.map(|score| score + if before + 1 == at { 4 } else { 0 }))
        .max()
        .map(|score| (score, at))
}

/// The rows of `rows` (`(id, name)`, the list's order) that `query` matches, the best first; equal ones keep the
/// list's order. At most [`MAX_MATCHES`].
#[must_use]
pub fn matches(query: &str, rows: &[(String, SharedString)]) -> Vec<(String, SharedString)> {
    let mut scored: Vec<(u32, usize)> = rows
        .iter()
        .enumerate()
        .filter_map(|(at, (_, name))| fuzzy_score(query, name).map(|score| (score, at)))
        .collect();
    scored.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(&right.1)));
    scored
        .into_iter()
        .take(MAX_MATCHES)
        .map(|(_, at)| rows[at].clone())
        .collect()
}

/// What the switcher tells the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SwitcherEvent {
    /// Open this chat.
    Open(String),
    /// Open this chat at this message (a message the query found).
    OpenMessage(HitTarget),
    /// Close the switcher.
    Dismissed,
    /// Put the diagnostics on the clipboard (the "Copy diagnostics" command).
    CopyDiagnostics,
}

/// The palette's section of the chats; the messages found are the next.
const CHATS: usize = 0;
/// The palette's section of the messages found.
const MESSAGES: usize = 1;
/// The palette's section of the app's commands.
const COMMANDS: usize = 2;

/// The commands the palette offers, matched on their names as the chats are.
pub const COMMAND_NAMES: [&str; 1] = ["Copy diagnostics"];

/// The switcher (see the module docs).
pub struct Switcher {
    state: Entity<CommandState>,
    list: Entity<ChatList>,
    core: Arc<dyn Core>,
    query: String,
    matches: Vec<(String, SharedString)>,
    /// The messages the query finds, the newest first.
    hits: Vec<Hit>,
    /// The search for the latest query: a later query drops it.
    searching: Option<Task<()>>,
    _observing: Subscription,
    /// Escape, before the palette's own binding (which first clears the query).
    _escape: Subscription,
}

impl EventEmitter<SwitcherEvent> for Switcher {}

impl Switcher {
    pub fn new(list: Entity<ChatList>, core: Arc<dyn Core>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|cx| CommandState::new(window, cx));
        let observing = cx.observe(&list, |this: &mut Self, _, cx| this.refilter(cx));
        let this = cx.weak_entity();
        let escape = cx.intercept_keystrokes(move |event, _window, cx| {
            let keystroke = &event.keystroke;
            if keystroke.key == "escape" && keystroke.modifiers.number_of_modifiers() == 0 {
                let dismissed = this.update(cx, |_, cx| cx.emit(SwitcherEvent::Dismissed));
                if dismissed.is_ok() {
                    cx.stop_propagation();
                }
            }
        });
        let mut switcher = Self {
            state,
            list,
            core,
            query: String::new(),
            matches: Vec::new(),
            hits: Vec::new(),
            searching: None,
            _observing: observing,
            _escape: escape,
        };
        switcher.refilter(cx);
        switcher
    }

    /// Moves the keyboard focus to the query field.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.state.update(cx, |state, cx| state.focus(window, cx));
    }

    /// The chats listed now, the best match first: `(id, name)`.
    #[must_use]
    pub fn matches(&self) -> &[(String, SharedString)] {
        &self.matches
    }

    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The messages the query finds, listed under the chats, the newest first.
    #[must_use]
    pub fn hits(&self) -> &[Hit] {
        &self.hits
    }

    /// The commands the query matches ([`COMMAND_NAMES`]), listed last.
    #[must_use]
    pub fn commands(&self) -> Vec<&'static str> {
        COMMAND_NAMES
            .into_iter()
            .filter(|name| fuzzy_score(&self.query, name).is_some())
            .collect()
    }

    /// Whether a search for the query runs.
    #[must_use]
    pub fn searching(&self) -> bool {
        self.searching.is_some()
    }

    fn set_query(&mut self, query: &str, cx: &mut Context<Self>) {
        query.clone_into(&mut self.query);
        self.refilter(cx);
        self.search(cx);
    }

    /// Looks for the query's messages ([`search::find`]); a query too short finds none.
    fn search(&mut self, cx: &mut Context<Self>) {
        if !search::searchable(&self.query) {
            self.searching = None;
            self.hits.clear();
            return;
        }
        let core = Arc::clone(&self.core);
        let chats = search::listed_chats(&self.list, cx);
        let query = self.query.clone();
        self.searching = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            let hits = search::find(&*core, query, chats).await;
            let _ = this.update(cx, |this, cx| {
                this.searching = None;
                this.hits = hits;
                cx.notify();
            });
        }));
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let rows: Vec<(String, SharedString)> = self
            .list
            .read(cx)
            .rows()
            .iter()
            .map(|row| (row.id.clone(), row.name.clone()))
            .collect();
        self.matches = matches(&self.query, &rows);
        cx.notify();
    }

    fn confirm(&mut self, index: IndexPath, cx: &mut Context<Self>) {
        let event = match index.section {
            CHATS => self
                .matches
                .get(index.row)
                .map(|(id, _)| SwitcherEvent::Open(id.clone())),
            MESSAGES => self
                .hits
                .get(index.row)
                .map(|hit| SwitcherEvent::OpenMessage(hit.target.clone())),
            COMMANDS => self.commands().get(index.row).map(|_| SwitcherEvent::CopyDiagnostics),
            _ => None,
        };
        if let Some(event) = event {
            cx.emit(event);
        }
    }
}

impl Render for Switcher {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let on_query = cx.weak_entity();
        let on_confirm = cx.weak_entity();
        let chats = self
            .matches
            .iter()
            .map(|(_, name)| CommandItem::new().label(name.clone()));
        // the sections in order (an empty one is hidden, and keeps its number)
        let command = Command::new(&self.state)
            .filterable(false)
            .placeholder("Go to a chat, or find a message")
            .group(CommandGroup::new().label("Chats").items(chats))
            .group(
                CommandGroup::new()
                    .label("Messages")
                    .items(self.hits.iter().map(search::hit_item)),
            )
            .group(
                CommandGroup::new()
                    .label("Commands")
                    .items(self.commands().into_iter().map(|name| CommandItem::new().label(name))),
            )
            .on_query(move |query, _, cx| {
                // a switcher already closed has nothing to filter
                let _ = on_query.update(cx, |this, cx| this.set_query(query, cx));
            })
            .on_confirm(move |index, _, cx| {
                let _ = on_confirm.update(cx, |this, cx| this.confirm(index, cx));
            })
            .empty(|_, _, _| div().p_2().child("No chat or message matches"));
        div()
            .debug_selector(|| "switcher".to_owned())
            .w(px(WIDTH))
            .rounded_md()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .child(command)
    }
}
