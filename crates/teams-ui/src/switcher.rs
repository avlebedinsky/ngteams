//! The chat switcher (Ctrl+K): the UI kit's command palette over the chat list's rows, matched fuzzily on their names
//! as the user types ([`fuzzy_score`]): Enter opens the chat highlighted, Escape closes the switcher whatever was typed.
//! It is the live test's way into a chat: the keyboard, no screen coordinates.
//!
//! It reads the chat list's rows (one model for both), so a chat that changes while it is open shows changed.

use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, Subscription, Window, div, px, rgb,
};
use gpui_component::IndexPath;
use gpui_component::command::{Command, CommandItem, CommandState};

use crate::chat_list::ChatList;
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
    /// Close the switcher.
    Dismissed,
}

/// The switcher (see the module docs).
pub struct Switcher {
    state: Entity<CommandState>,
    list: Entity<ChatList>,
    query: String,
    matches: Vec<(String, SharedString)>,
    _observing: Subscription,
    /// Escape, before the palette's own binding (which first clears the query).
    _escape: Subscription,
}

impl EventEmitter<SwitcherEvent> for Switcher {}

impl Switcher {
    pub fn new(list: Entity<ChatList>, window: &mut Window, cx: &mut Context<Self>) -> Self {
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
            query: String::new(),
            matches: Vec::new(),
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

    fn set_query(&mut self, query: &str, cx: &mut Context<Self>) {
        query.clone_into(&mut self.query);
        self.refilter(cx);
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
        if let Some((id, _)) = self.matches.get(index.row) {
            cx.emit(SwitcherEvent::Open(id.clone()));
        }
    }
}

impl Render for Switcher {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let on_query = cx.weak_entity();
        let on_confirm = cx.weak_entity();
        let items = self
            .matches
            .iter()
            .map(|(_, name)| CommandItem::new().label(name.clone()));
        let command = Command::new(&self.state)
            .filterable(false)
            .placeholder("Go to a chat")
            .items(items)
            .on_query(move |query, _, cx| {
                // a switcher already closed has nothing to filter
                let _ = on_query.update(cx, |this, cx| this.set_query(query, cx));
            })
            .on_confirm(move |index, _, cx| {
                let _ = on_confirm.update(cx, |this, cx| this.confirm(index, cx));
            })
            .empty(|_, _, _| div().p_2().child("No chat matches"));
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
