//! The chat switcher (Ctrl+K): the UI kit's command palette over the chat list's rows, matched fuzzily on their names
//! as the user types ([`fuzzy_score`]): Enter opens the chat highlighted, Escape closes the switcher whatever was typed
//! (while the focus is in it: an Escape elsewhere goes where the focus is), and so does Ctrl+K again.
//! It is the live test's way into a chat: the keyboard, no screen coordinates.
//!
//! It reads the chat list's rows (one model for both), so a chat that changes while it is open shows changed. Under
//! the chats come the teams' channels the Teams list shows ("<team> › <channel>", matched the same way): one chosen
//! opens read-only, as from the Teams list. Then the messages the query finds in the store ([`crate::search`]): one
//! chosen opens its chat at the message. Last come the app's commands the query matches ([`COMMANDS`]: "Copy
//! diagnostics", and "Set status: …" for each status the account menu offers, so the account's status is set from the
//! keyboard too), and in a build that shows calls the call commands that fit the moment ([`CallCommand`]: "Call" the
//! chat shown, "Test call", "Join call", "Answer call", "Decline call", "Leave call", "Mute or unmute", M3 Task 17;
//! "Camera on or off", Task 18).

use std::sync::Arc;

use gpui::{
    App, AppContext as _, AsyncApp, Context, Entity, EventEmitter, FocusHandle, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Task, Window, div, px, rgb,
};
use gpui_component::IndexPath;
use gpui_component::command::{Command, CommandGroup, CommandItem, CommandState};

use crate::bridge::Core;
use crate::calls::Calls;
use crate::chat_list::ChatList;
use crate::search::{self, Hit, HitTarget};
use crate::settings::ManualStatus;
use crate::teams_list::TeamsList;
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
    /// Open this team's channel, read-only.
    OpenChannel(String),
    /// Open this chat at this message (a message the query found).
    OpenMessage(HitTarget),
    /// Close the switcher.
    Dismissed,
    /// Put the diagnostics on the clipboard (the "Copy diagnostics" command).
    CopyDiagnostics,
    /// Set the account's status, as the account menu does (a "Set status: …" command).
    SetStatus(ManualStatus),
    /// A call command.
    Call(CallCommand),
}

/// A call command the palette offers (in a build that shows calls), when it fits the moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallCommand {
    /// Call the chat shown.
    CallChat,
    TestCall,
    /// Join the running call of the chat shown.
    Join,
    Answer,
    Decline,
    HangUp,
    ToggleMute,
    ToggleCamera,
}

impl CallCommand {
    /// What the palette calls it, and matches the query on.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::CallChat => "Call",
            Self::TestCall => "Test call",
            Self::Join => "Join call",
            Self::Answer => "Answer call",
            Self::Decline => "Decline call",
            Self::HangUp => "Leave call",
            Self::ToggleMute => "Mute or unmute",
            Self::ToggleCamera => "Turn camera on or off",
        }
    }

    /// The commands that fit `calls` now, `chat` the chat shown.
    #[must_use]
    pub fn fitting(calls: &Calls, chat: Option<&str>) -> Vec<Self> {
        if !calls.shows() {
            return Vec::new();
        }
        if calls.ringing().is_some() {
            return vec![Self::Answer, Self::Decline];
        }
        if calls.call().is_some() {
            let mut up = vec![Self::HangUp, Self::ToggleMute];
            if calls.video().is_some() {
                up.push(Self::ToggleCamera);
            }
            return up;
        }
        let mut fitting = Vec::new();
        if let Some(chat) = chat {
            fitting.push(if calls.joinable(chat) {
                Self::Join
            } else {
                Self::CallChat
            });
        }
        if calls.has_test_call() {
            fitting.push(Self::TestCall);
        }
        fitting
    }
}

/// The palette's sections in order (an empty one is hidden, and keeps its number).
const CHATS: usize = 0;
const CHANNELS: usize = 1;
const MESSAGES: usize = 2;
const COMMAND_SECTION: usize = 3;

/// One of the app's commands the palette offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteCommand {
    CopyDiagnostics,
    SetStatus(ManualStatus),
    Call(CallCommand),
}

impl PaletteCommand {
    /// What the palette calls it, and matches the query on.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::CopyDiagnostics => "Copy diagnostics",
            Self::SetStatus(ManualStatus::Automatic) => "Set status: Automatic",
            Self::SetStatus(ManualStatus::Available) => "Set status: Available",
            Self::SetStatus(ManualStatus::Busy) => "Set status: Busy",
            Self::SetStatus(ManualStatus::DoNotDisturb) => "Set status: Do not disturb",
            Self::SetStatus(ManualStatus::Away) => "Set status: Appear away",
            Self::Call(command) => command.name(),
        }
    }

    fn event(self) -> SwitcherEvent {
        match self {
            Self::CopyDiagnostics => SwitcherEvent::CopyDiagnostics,
            Self::SetStatus(status) => SwitcherEvent::SetStatus(status),
            Self::Call(command) => SwitcherEvent::Call(command),
        }
    }
}

/// The commands the palette offers, in order, matched on their names as the chats are.
pub const COMMANDS: [PaletteCommand; 6] = [
    PaletteCommand::CopyDiagnostics,
    PaletteCommand::SetStatus(ManualStatus::Automatic),
    PaletteCommand::SetStatus(ManualStatus::Available),
    PaletteCommand::SetStatus(ManualStatus::Busy),
    PaletteCommand::SetStatus(ManualStatus::DoNotDisturb),
    PaletteCommand::SetStatus(ManualStatus::Away),
];

/// The switcher (see the module docs).
pub struct Switcher {
    state: Entity<CommandState>,
    list: Entity<ChatList>,
    teams: Entity<TeamsList>,
    core: Arc<dyn Core>,
    query: String,
    matches: Vec<(String, SharedString)>,
    /// The channels the query matches, the best first: `(id, "<team> › <channel>")`.
    channels: Vec<(String, SharedString)>,
    /// The messages the query finds, the newest first.
    hits: Vec<Hit>,
    /// The search for the latest query: a later query drops it.
    searching: Option<Task<()>>,
    /// The call commands that fit the moment, first of the commands.
    call_commands: Vec<CallCommand>,
    _observing: [Subscription; 3],
    /// Tracked on the dialog's root: the focus is in it while its query field or the palette's list has it.
    focus: FocusHandle,
    /// Escape, before the palette's own binding (which first clears the query).
    _escape: Subscription,
}

impl EventEmitter<SwitcherEvent> for Switcher {}

impl Switcher {
    /// The switcher over the chat list's rows and the Teams list's channels; with the call commands that fit `calls`
    /// now, `chat` the chat shown.
    pub fn new(
        list: Entity<ChatList>,
        teams: Entity<TeamsList>,
        core: Arc<dyn Core>,
        (calls, chat): (Entity<Calls>, Option<String>),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let state = cx.new(|cx| CommandState::new(window, cx));
        let fitting = move |calls: &Calls| CallCommand::fitting(calls, chat.as_deref());
        let call_commands = fitting(calls.read(cx));
        let observing = [
            cx.observe(&list, |this: &mut Self, _, cx| this.refilter(cx)),
            cx.observe(&teams, |this: &mut Self, _, cx| this.refilter(cx)),
            cx.observe(&calls, move |this: &mut Self, calls, cx| {
                this.call_commands = fitting(calls.read(cx));
                this.refilter(cx);
            }),
        ];
        let this = cx.weak_entity();
        // the interceptor sees every keystroke of the app: an Escape is the switcher's only while the focus is in it (a
        // switcher left open in a shell not shown, another account's, or with the focus moved out of it, never takes
        // the Escape meant for where the focus is)
        let escape = cx.intercept_keystrokes(move |event, window, cx| {
            let keystroke = &event.keystroke;
            if keystroke.key != "escape" || keystroke.modifiers.number_of_modifiers() != 0 {
                return;
            }
            let dismissed = this.update(cx, |this, cx| {
                let held = this.holds_focus(window, cx);
                if held {
                    cx.emit(SwitcherEvent::Dismissed);
                }
                held
            });
            if matches!(dismissed, Ok(true)) {
                cx.stop_propagation();
            }
        });
        let mut switcher = Self {
            state,
            list,
            teams,
            core,
            query: String::new(),
            matches: Vec::new(),
            channels: Vec::new(),
            hits: Vec::new(),
            searching: None,
            call_commands,
            _observing: observing,
            focus: cx.focus_handle(),
            _escape: escape,
        };
        switcher.refilter(cx);
        switcher
    }

    /// Moves the keyboard focus to the query field.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.state.update(cx, |state, cx| state.focus(window, cx));
    }

    /// Whether the focus is in the switcher (its query field, or its list once clicked): the focus may have moved
    /// elsewhere since it opened. Read from the last frame, which GPUI draws before it dispatches a key; one not drawn
    /// (in a shell not shown) never holds it.
    #[must_use]
    pub fn holds_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus.contains_focused(window, cx)
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

    /// The channels listed now, the best match first: `(id, "<team> › <channel>")`.
    #[must_use]
    pub fn channels(&self) -> &[(String, SharedString)] {
        &self.channels
    }

    /// The names of the commands the query matches ([`COMMANDS`]), listed last.
    #[must_use]
    pub fn commands(&self) -> Vec<&'static str> {
        self.matching_commands().into_iter().map(PaletteCommand::name).collect()
    }

    fn matching_commands(&self) -> Vec<PaletteCommand> {
        self.call_commands
            .iter()
            .copied()
            .map(PaletteCommand::Call)
            .chain(COMMANDS)
            .filter(|command| fuzzy_score(&self.query, command.name()).is_some())
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
            .all_rows()
            .map(|row| (row.id.clone(), row.name.clone()))
            .collect();
        self.matches = matches(&self.query, &rows);
        let channels: Vec<(String, SharedString)> = self
            .teams
            .read(cx)
            .teams()
            .unwrap_or_default()
            .iter()
            .flat_map(|team| {
                team.channels
                    .iter()
                    .map(move |channel| (channel.id.clone(), format!("{} › {}", team.name, channel.name).into()))
            })
            .collect();
        self.channels = matches(&self.query, &channels);
        cx.notify();
    }

    fn confirm(&mut self, index: IndexPath, cx: &mut Context<Self>) {
        let event = match index.section {
            CHATS => self
                .matches
                .get(index.row)
                .map(|(id, _)| SwitcherEvent::Open(id.clone())),
            CHANNELS => self
                .channels
                .get(index.row)
                .map(|(id, _)| SwitcherEvent::OpenChannel(id.clone())),
            MESSAGES => self
                .hits
                .get(index.row)
                .map(|hit| SwitcherEvent::OpenMessage(hit.target.clone())),
            COMMAND_SECTION => self.matching_commands().get(index.row).map(|command| command.event()),
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
            .placeholder("Go to a chat or a channel, find a message, or run a command")
            .group(CommandGroup::new().label("Chats").items(chats))
            .group(
                CommandGroup::new().label("Channels").items(
                    self.channels
                        .iter()
                        .map(|(_, name)| CommandItem::new().label(name.clone())),
                ),
            )
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
            .empty(|_, _, _| div().p_2().child("No chat, channel, message or command matches"));
        div()
            .debug_selector(|| "switcher".to_owned())
            .track_focus(&self.focus)
            .w(px(WIDTH))
            .rounded_md()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .child(command)
    }
}
