//! The "Create a channel" dialog of a team's "..." menu, as Teams has it (`docs/protocol/channels.md`, "Creating a
//! channel"): the channel's name and description, its type, its layout, Cancel and Create.
//!
//! - **The form** ([`name_problem`], [`description_problem`]): Teams' own rules and words. A problem shows under its field
//!   once the name or the description was touched; Create stays off until the name reads, a type is chosen and something
//!   was typed. The team's own channel names are taken, in any case.
//! - **The types**: Standard, and Private where the team lets the account ([`Team::can_add_private_channel`]). A team's
//!   owner choosing Standard may also show the channel in everyone's list.
//! - **Creating** ([`Core::create_channel`], only on Create): "Adding the channel...", then the channel opens; what goes
//!   wrong replaces the form with Teams' words for it, "Try again" where trying again can help ([`problem_words`]).
//! - **A private channel's people** ([`Step::People`]): as in Teams, the form gives way to "Add members to the ... channel":
//!   a box that finds people on the team ([`Core::team_people`]), the people picked, Skip and Add. Then its thread is
//!   waited for ([`Core::channel_thread`]) and the channel opens.
//!
//! Nothing here sends anything but through [`Core`]. Traced: `channel-dialog opened`/`closed`, `created`, with the team or
//! the channel by id and a `kind`, never a name.

use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, AsyncApp, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity, Window, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::spinner::Spinner;
use gpui_component::{Disableable as _, FocusTrapElement as _, Sizable as _};
use teams_core::{ChannelDraft, ChannelMade, ChannelProblem, EngineError};
use teams_proto::{ChannelLayout, NewChannel, NewChannelKind, TeamPerson};
use teams_store::Team;

use crate::bridge::Core;
use crate::icons::{Glyph, icon_button, selected_by};
use crate::keyboard::{self, Handles};
use crate::teams_page::PopupButton;
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};

/// The longest name Teams takes.
pub const NAME_LIMIT: usize = 50;
/// The longest description Teams takes.
pub const DESCRIPTION_LIMIT: usize = 1024;
/// The dialog's width and height, px, as Teams draws it.
const WIDTH: f32 = 600.0;
const HEIGHT: f32 = 700.0;
/// How long the people box waits after a key before it asks.
const PAUSE: Duration = Duration::from_millis(250);
/// The most people suggested.
const MOST: usize = 8;
/// The learn-more link of the channel types' info icon, as Teams opens it.
const TYPES_LINK: &str = "https://learn.microsoft.com/en-us/microsoftteams/teams-channels-overview";
/// The channel type's marks, as Teams colours them.
const STANDARD_MARK: u32 = 0x2d_4f_12;
const PRIVATE_MARK: u32 = 0x8f_50_0d;

/// What the dialog tells the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChannelDialogEvent {
    /// It closed with nothing to open.
    Closed,
    /// It closed on the channel to open.
    Open(String),
}

/// Where the dialog is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// The form.
    Form,
    /// The creation is under way.
    Creating,
    /// The creation failed: what to say, and whether to try again.
    Failed(Words),
    /// A private channel's people, its group kept.
    People { group: String, thread: Option<String> },
    /// The people are being added.
    Adding { group: String, thread: Option<String> },
    /// Some people weren't added: how many, then the channel opens on Close.
    SomeNotAdded {
        failed: usize,
        group: String,
        thread: Option<String>,
    },
    /// A private channel's thread is waited for.
    Waiting,
    /// A word to give before closing: the channel exists but something after it didn't go. Close opens `open`.
    Notice { words: Words, open: Option<String> },
}

/// A problem's words, as Teams writes them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Words {
    pub message: &'static str,
    pub detail: Option<&'static str>,
    pub retry: bool,
}

/// What Teams says for `problem`, and whether it offers "Try again".
#[must_use]
pub fn problem_words(problem: ChannelProblem) -> Words {
    let say = |message, detail, retry| Words { message, detail, retry };
    match problem {
        ChannelProblem::NameTaken => say(
            "The name can't be the same as any channels previously created in this team.",
            None,
            false,
        ),
        ChannelProblem::TooManyPrivateChannels => say(
            "Couldn't create private channel",
            Some(
                "The team is at the limit of 30 private channels. To create a new one, you'd need to delete one first.",
            ),
            false,
        ),
        ChannelProblem::TooManyChannels => say(
            "Couldn't create channel",
            Some(
                "This team has the maximum number of channels. To create a new channel, you'd need to delete one first.",
            ),
            false,
        ),
        ChannelProblem::NoTeamOwner => say(
            "You and other members can't create channels right now because this team has no owner. Ask your IT admin to assign an owner for this team.",
            None,
            false,
        ),
        ChannelProblem::NoPermission => say("Looks like you don't have permission to create a channel.", None, false),
        ChannelProblem::PrivateNotAllowed => say(
            "Looks like your org isn't allowing you to create a private channel in this team.",
            None,
            false,
        ),
        ChannelProblem::TeamArchived => say("This team is archived, so you can't take this action.", None, false),
        ChannelProblem::InvalidUsers => say(
            "Looks like you can't create a channel in this team",
            Some("Please check your role or permissions and try again"),
            false,
        ),
        ChannelProblem::DescriptionTooLong => say(
            "We ran into a problem creating your channel",
            Some("Try a shorter description or deleting any extra space following what you wrote."),
            false,
        ),
        ChannelProblem::Other => say("We ran into a problem creating your channel", None, true),
    }
}

/// Names Teams keeps for itself, as it lists them.
const BLOCKED: [&str; 24] = [
    "forms",
    ".lock",
    "con",
    "prn",
    "aux",
    "nul",
    "_vti_",
    "desktop.ini",
    "com1",
    "com2",
    "com3",
    "com4",
    "com5",
    "com6",
    "com7",
    "com8",
    "lpt1",
    "lpt2",
    "lpt3",
    "lpt4",
    "lpt5",
    "lpt6",
    "lpt7",
    "lpt8",
];

/// The characters Teams refuses in a name.
const REFUSED: &str = "~#%&*{}/\\:<>?+|'\"";

/// What's wrong with `name` in a team whose channels are named `taken`, in Teams' words; `None` when it reads.
#[must_use]
pub fn name_problem(name: &str, taken: &[String]) -> Option<String> {
    let name = name.trim();
    let lower = name.to_lowercase();
    if name.is_empty() {
        return Some("Required.".to_owned());
    }
    if name.starts_with(['_', '.']) {
        return Some("Channel name cannot start with an underscore (_) or a period (.)".to_owned());
    }
    if name.ends_with('.') {
        return Some("Channel name cannot end with a period (.)".to_owned());
    }
    if name.contains(|c| REFUSED.contains(c)) || name.contains("..") {
        return Some("We don't support these characters: ~#%&*{}+/\\:<>?|'\"..".to_owned());
    }
    if BLOCKED.contains(&name) || name == "lpt9" {
        return Some(format!("Channel name can't be {name}. Try another name."));
    }
    if lower == "general" {
        return Some("The name General is reserved. Try another name.".to_owned());
    }
    if taken.iter().any(|taken| taken.to_lowercase() == lower) {
        return Some("Channel name is already taken.".to_owned());
    }
    if holds_link(name) {
        return Some("Links are not allowed in the channel name".to_owned());
    }
    if lower == "forms" {
        return Some("The name Forms is reserved. Try another name".to_owned());
    }
    None
}

/// What's wrong with a description, in Teams' words.
#[must_use]
pub fn description_problem(description: &str) -> Option<&'static str> {
    holds_link(description.trim()).then_some("Links are not allowed in the channel description")
}

/// Whether `text` reads as Teams' link check does: it starts `www.<a>.<b>`, or holds `http(s)://<a>.<b>`.
fn holds_link(text: &str) -> bool {
    let two_parts = |rest: &str| {
        let mut parts = rest.split('.');
        let (first, second) = (parts.next().unwrap_or_default(), parts.next());
        !first.is_empty() && second.is_some_and(|second| !second.is_empty())
    };
    if text.strip_prefix("www.").is_some_and(two_parts) {
        return true;
    }
    ["http://", "https://"].iter().any(|scheme| {
        text.match_indices(scheme)
            .any(|(at, _)| two_parts(&text[at + scheme.len()..]))
    })
}

/// The channel type's rows, as Teams shows them.
fn type_words(kind: NewChannelKind) -> (&'static str, &'static str, &'static str) {
    match kind {
        NewChannelKind::Standard => (
            "Standard",
            "Everyone on the team has access.",
            "Standard \u{2014} Everyone on the team has access",
        ),
        NewChannelKind::Private => (
            "Private",
            "Specific people on the team have access.",
            "Private \u{2014} Specific people on the team have access",
        ),
    }
}

/// A selector's key of a channel type.
fn type_key(kind: NewChannelKind) -> &'static str {
    match kind {
        NewChannelKind::Standard => "standard",
        NewChannelKind::Private => "private",
    }
}

/// The dialog (see the module docs).
pub struct ChannelDialog {
    core: Arc<dyn Core>,
    trace: Arc<ViewTrace>,
    team: Team,
    /// The team's channels' names: taken.
    taken: Vec<String>,
    name: Entity<InputState>,
    description: Entity<TextareaState>,
    kind: Option<NewChannelKind>,
    /// The kinds' panel is open.
    type_open: bool,
    /// The kind the panel's keys are on.
    type_highlight: Option<NewChannelKind>,
    type_focus: FocusHandle,
    layout: ChannelLayout,
    /// The layout cards' focus: one radio group, the chosen card its tab stop.
    layouts: Handles,
    show_by_default: bool,
    /// The name or the description was touched: problems show.
    touched: bool,
    step: Step,
    /// The people box, in the people step.
    people_box: Option<Entity<InputState>>,
    picked: Vec<TeamPerson>,
    suggestions: Vec<TeamPerson>,
    /// The people box's text the suggestions are for.
    suggested_for: String,
    suggesting: Option<Task<()>>,
    working: Option<Task<()>>,
    /// The people found: Up and Down move through them.
    found_focus: FocusHandle,
    focus: FocusHandle,
    inputs: Vec<Subscription>,
}

impl EventEmitter<ChannelDialogEvent> for ChannelDialog {}

impl Focusable for ChannelDialog {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ChannelDialog {
    /// The dialog on `team`, its name box focused.
    pub fn new(
        core: Arc<dyn Core>,
        trace: Arc<ViewTrace>,
        team: Team,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Letters, numbers, and spaces are allowed")
                .validate(|text, _| text.chars().count() <= NAME_LIMIT)
        });
        let description = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Help others find the right channel by providing a description")
                .soft_wrap(true)
                .auto_grow(2, 2)
        });
        let touched_name = cx.subscribe(&name, |this: &mut Self, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.touched = true;
                cx.notify();
            }
        });
        let touched_description = cx.subscribe(&description, |this: &mut Self, state, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.touched = true;
                let _ = state;
                cx.notify();
            }
        });
        name.update(cx, |name, cx| name.focus(window, cx));
        trace.record(&TraceLine::new("channel-dialog", "opened").conversation(&team.id));
        let taken = team.channels.iter().map(|channel| channel.name.clone()).collect();
        Self {
            core,
            trace,
            team,
            taken,
            name,
            description,
            kind: None,
            type_open: false,
            type_highlight: None,
            type_focus: cx.focus_handle().tab_stop(true),
            layout: ChannelLayout::default(),
            layouts: Handles::default(),
            show_by_default: false,
            touched: false,
            step: Step::Form,
            people_box: None,
            picked: Vec::new(),
            suggestions: Vec::new(),
            suggested_for: String::new(),
            suggesting: None,
            working: None,
            found_focus: cx.focus_handle(),
            focus: cx.focus_handle(),
            inputs: vec![touched_name, touched_description],
        }
    }

    /// The layout chosen.
    #[must_use]
    pub fn layout(&self) -> ChannelLayout {
        self.layout
    }

    /// Where the dialog is.
    #[must_use]
    pub fn step(&self) -> &Step {
        &self.step
    }

    /// What's wrong with the name, shown or not.
    fn name_problem(&self, cx: &gpui::App) -> Option<String> {
        name_problem(&self.name.read(cx).value(), &self.taken)
    }

    /// What's wrong with the description, shown or not.
    fn description_problem(&self, cx: &gpui::App) -> Option<&'static str> {
        description_problem(&self.description.read(cx).value())
    }

    /// Whether Create is on: Teams' rule.
    #[must_use]
    pub fn can_create(&self, cx: &gpui::App) -> bool {
        self.touched && self.kind.is_some() && self.name_problem(cx).is_none() && self.description_problem(cx).is_none()
    }

    /// Puts `name` in the name box.
    pub fn set_name(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.name.update(cx, |input, cx| input.set_value(name, window, cx));
        self.touched = true;
        cx.notify();
    }

    /// Puts `description` in its box.
    pub fn set_description(&mut self, description: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.description
            .update(cx, |input, cx| input.set_value(description, window, cx));
        self.touched = true;
        cx.notify();
    }

    /// Picks the channel's type (the kinds this team offers only).
    pub fn pick_kind(&mut self, kind: NewChannelKind, cx: &mut Context<Self>) {
        self.kind = Some(kind);
        cx.notify();
    }

    /// Picks the channel's layout.
    pub fn pick_layout(&mut self, layout: ChannelLayout, cx: &mut Context<Self>) {
        self.layout = layout;
        cx.notify();
    }

    /// The kinds the team offers the account.
    #[must_use]
    pub fn kinds(&self) -> Vec<NewChannelKind> {
        let mut kinds = vec![NewChannelKind::Standard];
        if self.team.can_add_private_channel {
            kinds.push(NewChannelKind::Private);
        }
        kinds
    }

    /// Closes with nothing to open; only where nothing is under way.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if matches!(self.step, Step::Creating | Step::Adding { .. } | Step::Waiting) {
            return;
        }
        self.close(None, cx);
    }

    /// Closes with nothing to open, whatever step it is on.
    pub fn close_now(&mut self, cx: &mut Context<Self>) {
        self.close(None, cx);
    }

    fn close(&mut self, open: Option<String>, cx: &mut Context<Self>) {
        self.trace.record(
            &TraceLine::new("channel-dialog", "closed").conversation(open.as_deref().unwrap_or(self.team.id.as_str())),
        );
        cx.emit(open.map_or(ChannelDialogEvent::Closed, ChannelDialogEvent::Open));
    }

    /// Create: the channel made, its name trimmed.
    pub fn create(&mut self, cx: &mut Context<Self>) {
        if !self.can_create(cx) {
            return;
        }
        let (Some(kind), Some(group)) = (self.kind, self.team.group_id.clone()) else {
            self.failed(ChannelProblem::Other, cx);
            return;
        };
        let draft = ChannelDraft {
            channel: NewChannel {
                team: self.team.id.clone(),
                group,
                mailbox: self.team.mailbox.clone(),
                name: self.name.read(cx).value().trim().to_owned(),
                description: self.description.read(cx).value().to_string(),
                kind,
                layout: self.layout,
            },
            show_by_default: self.show_by_default && kind == NewChannelKind::Standard,
        };
        self.step = Step::Creating;
        cx.notify();
        let reply = self.core.create_channel(draft);
        self.working = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let made = reply.await;
            let _ = this.update(cx, |this, cx| this.created(made, cx));
        }));
    }

    /// What Create brought.
    fn created(&mut self, made: Result<ChannelMade, EngineError>, cx: &mut Context<Self>) {
        self.working = None;
        match made {
            Ok(made) => {
                let kind = self.kind.map_or("unknown", type_key);
                self.trace.record(
                    &TraceLine::new("channel-dialog", "created")
                        .conversation(made.thread.as_deref().unwrap_or(self.team.id.as_str()))
                        .kind(kind),
                );
                self.after_creation(made, cx);
            }
            Err(EngineError::ChannelNotCreated { why }) => self.failed(why, cx),
            Err(_) => self.failed(ChannelProblem::Other, cx),
        }
    }

    fn after_creation(&mut self, made: ChannelMade, cx: &mut Context<Self>) {
        if self.kind == Some(NewChannelKind::Private)
            && let Some(group) = made.group
        {
            self.people_step(group, made.thread, cx);
            return;
        }
        if made.not_shown_by_default {
            self.step = Step::Notice {
                words: Words {
                    message: "Channel created but we couldn\u{2019}t save it as recommended",
                    detail: Some(
                        "The team is at the limit for recommended channels. To recommend this channel, you'd need to stop recommending another one.",
                    ),
                    retry: false,
                },
                open: made.thread,
            };
            cx.notify();
            return;
        }
        self.close(made.thread, cx);
    }

    fn failed(&mut self, problem: ChannelProblem, cx: &mut Context<Self>) {
        self.step = Step::Failed(problem_words(problem));
        cx.notify();
    }

    /// "Try again": what was typed goes again.
    pub fn try_again(&mut self, cx: &mut Context<Self>) {
        self.step = Step::Form;
        self.create(cx);
    }

    /// The people step begins: the form gives way to the box.
    pub fn people_step(&mut self, group: String, thread: Option<String>, cx: &mut Context<Self>) {
        self.step = Step::People { group, thread };
        self.people_box = None;
        cx.notify();
    }

    /// The people box, made when first drawn (making one needs the window).
    fn people_box(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some(input) = &self.people_box {
            return input.clone();
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Type a name or email"));
        let typed = cx.subscribe(&input, |this: &mut Self, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let text = input.read(cx).value().trim().to_owned();
                this.search_people(text, cx);
            }
        });
        self.inputs.push(typed);
        input.update(cx, |input, cx| input.focus(window, cx));
        self.people_box = Some(input.clone());
        input
    }

    /// Asks for the people on the team `text` finds, a moment after the typing stops.
    pub fn search_people(&mut self, text: String, cx: &mut Context<Self>) {
        if text.is_empty() {
            self.suggestions.clear();
            self.suggesting = None;
            self.suggested_for.clear();
            cx.notify();
            return;
        }
        let core = Arc::clone(&self.core);
        let team = self.team.id.clone();
        self.suggesting = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            cx.background_executor().timer(PAUSE).await;
            let found = core.team_people(team, text.clone()).await.unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                this.suggestions = found.into_iter().take(MOST).collect();
                this.suggested_for = text;
                cx.notify();
            });
        }));
    }

    /// Picks someone found, clearing the box.
    pub fn pick_person(&mut self, person: TeamPerson, window: &mut Window, cx: &mut Context<Self>) {
        if !self.picked.iter().any(|picked| picked.mri == person.mri) {
            self.picked.push(person);
        }
        self.suggestions.clear();
        self.suggested_for.clear();
        if let Some(input) = self.people_box.clone() {
            input.update(cx, |input, cx| {
                input.set_value("", window, cx);
                input.focus(window, cx);
            });
        }
        cx.notify();
    }

    /// Takes someone picked back.
    pub fn unpick_person(&mut self, mri: &str, cx: &mut Context<Self>) {
        self.picked.retain(|person| person.mri != mri);
        cx.notify();
    }

    /// Add: the people picked go in, by the channel's group.
    pub fn add_people(&mut self, cx: &mut Context<Self>) {
        let Step::People { group, thread } = self.step.clone() else {
            return;
        };
        let (Some(team_group), false) = (self.team.group_id.clone(), self.picked.is_empty()) else {
            return;
        };
        let members: Vec<String> = self.picked.iter().map(|person| person.mri.clone()).collect();
        self.step = Step::Adding {
            group: group.clone(),
            thread: thread.clone(),
        };
        cx.notify();
        let reply = self
            .core
            .add_channel_members(team_group, group.clone(), members.clone());
        self.working = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let failed = reply.await.unwrap_or(members).len();
            let _ = this.update(cx, |this, cx| {
                this.working = None;
                if failed == 0 {
                    this.finish(group, thread, cx);
                } else {
                    this.step = Step::SomeNotAdded { failed, group, thread };
                    cx.notify();
                }
            });
        }));
    }

    /// Skip (or the people are in): the channel opens once it has its thread.
    pub fn finish(&mut self, group: String, thread: Option<String>, cx: &mut Context<Self>) {
        if let Some(thread) = thread {
            self.close(Some(thread), cx);
            return;
        }
        let Some(team_group) = self.team.group_id.clone() else {
            self.close(None, cx);
            return;
        };
        self.step = Step::Waiting;
        cx.notify();
        let name = self.name.read(cx).value().trim().to_owned();
        let reply = self.core.channel_thread(self.team.id.clone(), team_group, group, name);
        self.working = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let thread = reply.await.ok().flatten();
            let _ = this.update(cx, |this, cx| {
                this.working = None;
                if thread.is_some() {
                    this.close(thread, cx);
                } else {
                    this.step = Step::Notice {
                        words: Words {
                            message: "We're still setting up your channel",
                            detail: Some("It will show up in your channel list soon."),
                            retry: false,
                        },
                        open: None,
                    };
                    cx.notify();
                }
            });
        }));
    }

    /// The form's keys: Escape cancels. Tab and Shift+Tab go round the dialog's fields and buttons (the kit's root, the
    /// dialog trapping them).
    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            cx.stop_propagation();
            self.cancel(cx);
        }
    }

    /// The layouts' keys, as a radio group's: the arrows choose the other card, the keyboard on it.
    fn layout_key(&mut self, layout: ChannelLayout, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let at = LAYOUTS.iter().position(|shown| *shown == layout).unwrap_or(0);
        let Some(to) = keyboard::step(event, LAYOUTS.len(), at) else {
            return;
        };
        cx.stop_propagation();
        let layout = LAYOUTS[to];
        self.pick_layout(layout, cx);
        self.layouts.get(layout_key(layout), cx).focus(window, cx);
    }
}

/// A field's label: its words, and Teams' red mark when it is required.
fn label(words: &'static str, required: bool, palette: Palette) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_1()
        .text_sm()
        .text_color(rgb(palette.foreground))
        .child(words)
        .when(required, |label| {
            label.child(div().text_color(rgb(theme::DANGER)).child("*"))
        })
}

/// A problem's line under its field.
fn problem_line(selector: &'static str, problem: impl Into<SharedString>) -> gpui::Div {
    div()
        .debug_selector(move || selector.to_owned())
        .pt_1()
        .text_xs()
        .text_color(rgb(theme::DANGER))
        .child(problem.into())
}

/// A channel type's square mark, as Teams draws it.
fn type_mark(kind: NewChannelKind) -> gpui::Div {
    let (colour, glyph) = match kind {
        NewChannelKind::Standard => (STANDARD_MARK, Glyph::USERS),
        NewChannelKind::Private => (PRIVATE_MARK, Glyph::LOCK),
    };
    div()
        .size(px(32.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .bg(rgb(colour))
        .text_color(rgb(0xff_ff_ff))
        .child(glyph.icon().size(px(18.0)))
}

/// A layout card's picture: what the layout looks like, in the theme's colours.
fn layout_art(layout: ChannelLayout, palette: Palette) -> gpui::Div {
    let bar = |width: f32, colour: u32| div().h(px(8.0)).w(px(width)).rounded_sm().bg(rgb(colour));
    let grey = palette.muted_foreground;
    let dot = || div().size(px(5.0)).rounded_full().bg(rgb(palette.muted_foreground));
    let frame = div()
        .h(px(92.0))
        .w_full()
        .flex()
        .overflow_hidden()
        .rounded_sm()
        .border_2()
        .border_color(rgb(palette.muted_foreground))
        .bg(rgb(palette.background));
    match layout {
        ChannelLayout::Threads => frame
            .child(
                div()
                    .flex_1()
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().flex().items_center().gap_1().child(dot()).child(bar(56.0, grey)))
                    .child(div().pl(px(9.0)).child(bar(38.0, grey)))
                    .child(div().flex().justify_end().child(bar(44.0, palette.brand)))
                    .child(div().flex().items_center().gap_1().child(dot()).child(bar(46.0, grey))),
            )
            .child(
                div()
                    .w(px(44.0))
                    .h_full()
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .items_end()
                    .bg(rgb(palette.hover))
                    .child(bar(24.0, palette.brand))
                    .child(bar(24.0, grey)),
            ),
        ChannelLayout::Posts => {
            let band = |colour| {
                div()
                    .h(px(13.0))
                    .w_full()
                    .flex()
                    .items_center()
                    .pl_1()
                    .rounded_sm()
                    .bg(rgb(colour))
                    .child(div().size(px(5.0)).rounded_full().bg(rgb(palette.on_brand)))
            };
            frame.child(
                div()
                    .flex_1()
                    .px_6()
                    .py_1()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(band(palette.brand))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .px_1()
                            .child(div().flex().items_center().gap_1().child(dot()).child(bar(40.0, grey)))
                            .child(div().flex().items_center().gap_1().child(dot()).child(bar(40.0, grey))),
                    )
                    .child(band(palette.brand)),
            )
        }
    }
}

/// The layouts, in the order their cards show.
const LAYOUTS: [ChannelLayout; 2] = [ChannelLayout::Threads, ChannelLayout::Posts];

/// A layout's key, in its card's selector.
fn layout_key(layout: ChannelLayout) -> &'static str {
    match layout {
        ChannelLayout::Threads => "threads",
        ChannelLayout::Posts => "posts",
    }
}

/// A radio's mark.
fn radio(chosen: bool, palette: Palette) -> gpui::Div {
    div()
        .size(px(16.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .border_color(rgb(if chosen {
            palette.brand
        } else {
            palette.muted_foreground
        }))
        .when(chosen, |mark| {
            mark.child(div().size(px(10.0)).rounded_full().bg(rgb(palette.brand)))
        })
}

impl ChannelDialog {
    fn name_field(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let problem = self.name_problem(cx).filter(|_| self.touched);
        let reads = self.touched && problem.is_none();
        let mark = if problem.is_some() {
            Some(
                Glyph::CIRCLE_ALERT
                    .icon()
                    .size(px(16.0))
                    .text_color(rgb(theme::DANGER))
                    .into_any_element(),
            )
        } else if reads {
            Some(
                Glyph::CIRCLE_CHECK
                    .icon()
                    .size(px(16.0))
                    .text_color(rgb(palette.brand))
                    .into_any_element(),
            )
        } else {
            None
        };
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(label("Channel name", true, palette))
            .child(
                div()
                    .debug_selector(|| "channel-name".to_owned())
                    .child(Input::new(&self.name).suffix(div().flex_none().children(mark))),
            )
            .children(problem.map(|problem| problem_line("channel-name-problem", problem)))
    }

    fn description_field(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let problem = self.description_problem(cx).filter(|_| self.touched);
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(label("Description", false, palette))
            .child(
                div()
                    .debug_selector(|| "channel-description".to_owned())
                    .h(px(70.0))
                    .child(Textarea::new(&self.description).h_full()),
            )
            .children(problem.map(|problem| problem_line("channel-description-problem", problem)))
    }

    /// The channel type's label with its info icon, and its dropdown: a box saying what is chosen, the kinds in a panel
    /// over what's below it while it is open, as Teams draws them. Up and Down move through them and Enter picks one.
    fn type_field(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let info = selected_by(
            "channel-type-info",
            icon_button("channel-type-info", Glyph::INFO, "Learn more about channel types")
                .xsmall()
                .on_click(|_, _, cx| cx.open_url(TYPES_LINK)),
        );
        let (face_text, placeholder) = match self.kind {
            Some(kind) => (type_words(kind).2, false),
            None => ("Select", true),
        };
        let open = self.type_open;
        let face = div()
            .debug_selector(|| "channel-type".to_owned())
            .id("channel-type")
            .track_focus(&self.type_focus)
            .h(px(36.0))
            .w_full()
            .px_3()
            .flex()
            .items_center()
            .justify_between()
            .rounded_md()
            .border_1()
            .border_color(rgb(if open { palette.brand } else { palette.border }))
            .cursor_pointer()
            // a mouse click; its keys (Enter, Space, Down) are `on_type_key`'s, on their press
            .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                if !matches!(event, ClickEvent::Keyboard(_)) {
                    this.toggle_types(window, cx);
                }
            }))
            .on_key_down(cx.listener(Self::on_type_key))
            .child(
                div()
                    .text_color(rgb(if placeholder {
                        palette.muted_foreground
                    } else {
                        palette.foreground
                    }))
                    .child(face_text),
            )
            .child(
                Glyph::CHEVRON_DOWN
                    .icon()
                    .size(px(16.0))
                    .text_color(rgb(palette.muted_foreground)),
            );
        let pressed = PopupButton::default();
        // a zero-height place right under the box: an anchored element starts at its parent's origin
        let panel = open.then(|| div().relative().w_full().child(self.types_panel(&pressed, palette, cx)));
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(label("Choose a channel type", true, palette).child(info))
            .child(pressed.hold(face, panel).flex().flex_col().gap_1())
    }

    /// The kinds, in a panel over what's below the box (`pressed`).
    fn types_panel(&self, pressed: &PopupButton, palette: Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.kinds().into_iter().map(|kind| {
            let (title, description, _) = type_words(kind);
            let highlighted = self.type_highlight == Some(kind);
            let key = type_key(kind);
            div()
                .debug_selector(move || format!("channel-type-{key}"))
                .id(SharedString::from(format!("channel-type-{key}")))
                .w_full()
                .px_3()
                .py_1p5()
                .flex()
                .items_center()
                .gap_3()
                .rounded_md()
                .cursor_pointer()
                .when(highlighted, |row| row.bg(rgb(palette.hover)))
                .hover(move |row| row.bg(rgb(palette.hover)))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.pick_kind(kind, cx);
                    this.close_types(window, cx);
                }))
                .child(type_mark(kind))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(div().child(title))
                        .child(div().text_sm().child(description)),
                )
        });
        gpui::deferred(
            gpui::anchored().snap_to_window_with_margin(px(8.0)).child(
                div()
                    .debug_selector(|| "channel-types".to_owned())
                    .occlude()
                    .w(px(WIDTH - 48.0))
                    .mt_1()
                    .p_1()
                    .flex()
                    .flex_col()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.surface))
                    .shadow_lg()
                    .on_mouse_down_out(pressed.outside(cx.listener(|this, _, window, cx| this.close_types(window, cx))))
                    .children(rows),
            ),
        )
        .with_priority(1)
    }

    /// Opens the kinds' panel, or closes it.
    fn toggle_types(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.type_open {
            self.close_types(window, cx);
        } else {
            self.show_types(window, cx);
        }
    }

    /// Opens the kinds' panel, the chosen one (or the first) highlighted.
    pub fn show_types(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.type_open = true;
        self.type_highlight = self.kind.or(Some(NewChannelKind::Standard));
        self.type_focus.focus(window, cx);
        cx.notify();
    }

    /// Closes the kinds' panel, the focus back on its box.
    pub fn close_types(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.type_open = false;
        self.type_highlight = None;
        self.type_focus.focus(window, cx);
        cx.notify();
    }

    /// The box's keys: Enter, Space and Down open the kinds; in them Up and Down move, Enter and Space pick, Escape closes.
    fn on_type_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        if !self.type_open {
            if matches!(key, "enter" | "space" | "down") {
                cx.stop_propagation();
                self.show_types(window, cx);
            }
            return;
        }
        let kinds = self.kinds();
        let at = self
            .type_highlight
            .and_then(|highlight| kinds.iter().position(|kind| *kind == highlight))
            .unwrap_or(0);
        match key {
            "escape" => self.close_types(window, cx),
            "down" => self.type_highlight = kinds.get((at + 1).min(kinds.len() - 1)).copied(),
            "up" => self.type_highlight = kinds.get(at.saturating_sub(1)).copied(),
            "enter" | "space" => {
                if let Some(kind) = self.type_highlight {
                    self.pick_kind(kind, cx);
                }
                self.close_types(window, cx);
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// The owner's choice for a standard channel, under the type.
    fn show_by_default_row(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.kind != Some(NewChannelKind::Standard) || !self.team.owner {
            return None;
        }
        let checked = self.show_by_default;
        Some(
            selected_by(
                "channel-show-by-default",
                Checkbox::new("channel-show-by-default")
                    .label("Automatically show this channel in everyone\u{2019}s channel list")
                    .checked(checked)
                    .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                        this.show_by_default = *checked;
                        cx.notify();
                    })),
            )
            .into_any_element(),
        )
    }

    /// One layout's card: a radio of the layouts' group, Tab stopping on the chosen one.
    fn layout_card(&self, layout: ChannelLayout, palette: Palette, cx: &mut Context<Self>) -> keyboard::Stop {
        let chosen = self.layout == layout;
        let (key, title, description) = match layout {
            ChannelLayout::Threads => (
                "threads",
                "Threads",
                "Looks like chat with replies on the side in threads. Good for back-and-forth discussions.",
            ),
            ChannelLayout::Posts => (
                "posts",
                "Posts",
                "Posts reorder by most recent reply. Good for forums and announcements.",
            ),
        };
        let selector = format!("channel-layout-{key}");
        let card = div()
            .debug_selector(move || selector.clone())
            .id(SharedString::from(format!("channel-layout-{key}")))
            .w(px(198.0))
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .rounded_md()
            .border_1()
            .border_color(rgb(if chosen {
                palette.muted_foreground
            } else {
                palette.border
            }))
            .cursor_pointer()
            .hover(move |card| card.bg(rgb(palette.hover)))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.pick_layout(layout, cx)))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                this.layout_key(layout, event, window, cx);
            }))
            .child(layout_art(layout, palette))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_color(rgb(if chosen {
                                palette.foreground
                            } else {
                                palette.muted_foreground
                            }))
                            .child(title),
                    )
                    .child(radio(chosen, palette)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(if chosen {
                        palette.foreground
                    } else {
                        palette.muted_foreground
                    }))
                    .child(description),
            );
        keyboard::stop(SharedString::from(format!("channel-layout-{key}")), card, palette)
            .edge()
            .handle(&self.layouts.get(layout_key(layout), cx))
            .tab_stop(chosen)
    }

    /// The form: the fields, Cancel and Create.
    fn form(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let can_create = self.can_create(cx);
        let layouts = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).child("Layout"))
            .child(div().text_sm().child("Channel owners can change this at any time"))
            .child(
                div()
                    .pt_1()
                    .flex()
                    .gap_3()
                    .child(self.layout_card(ChannelLayout::Threads, palette, cx))
                    .child(self.layout_card(ChannelLayout::Posts, palette, cx)),
            );
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .text_xl()
                    .font_weight(gpui::FontWeight::BOLD)
                    .pb_3()
                    .child("Create a channel"),
            )
            .child(
                div()
                    .id("channel-form")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(self.name_field(palette, cx))
                    .child(self.description_field(palette, cx))
                    .child(self.type_field(palette, cx))
                    .children(self.show_by_default_row(cx))
                    .child(layouts),
            )
            .child(
                div()
                    .pt_3()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(selected_by(
                        "channel-cancel",
                        Button::new("channel-cancel")
                            .outline()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                    ))
                    .child(selected_by(
                        "channel-create",
                        Button::new("channel-create")
                            .primary()
                            .label("Create")
                            .disabled(!can_create)
                            // Tab passes it while it is off, as Teams' does
                            .tab_stop(can_create)
                            .on_click(cx.listener(|this, _, _, cx| this.create(cx))),
                    )),
            )
    }

    /// A step that waits: a spinner over its words.
    fn waiting(selector: &'static str, words: &'static str, palette: Palette) -> gpui::Div {
        div()
            .debug_selector(move || selector.to_owned())
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .text_color(rgb(palette.foreground))
            .child(Spinner::new())
            .child(words)
    }

    /// What went wrong, in Teams' words: a mark in a tinted circle, the words, "Try again" where it can help; Close at
    /// the top.
    fn failure(words: &Words, open: Option<&str>, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let retry = words.retry;
        let opens = open.map(str::to_owned);
        div()
            .debug_selector(|| "channel-failure".to_owned())
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .child(div().absolute().top_0().right_0().child(selected_by(
                "channel-failure-close",
                icon_button("channel-failure-close", Glyph::X, "Close").on_click(cx.listener(move |this, _, _, cx| {
                    this.close(opens.clone(), cx);
                })),
            )))
            .child(
                div()
                    .size(px(96.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(rgb(palette.selected))
                    .text_color(rgb(palette.brand))
                    .child(Glyph::CIRCLE_ALERT.icon().size(px(44.0))),
            )
            .child(
                div()
                    .debug_selector(|| "channel-failure-words".to_owned())
                    .max_w(px(460.0))
                    .text_center()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(words.message),
            )
            .children(words.detail.map(|detail| {
                div()
                    .max_w(px(460.0))
                    .text_center()
                    .text_sm()
                    .text_color(rgb(palette.muted_foreground))
                    .child(detail)
            }))
            .when(retry, |view| {
                view.child(selected_by(
                    "channel-try-again",
                    Button::new("channel-try-again")
                        .primary()
                        .label("Try again")
                        .on_click(cx.listener(|this, _, _, cx| this.try_again(cx))),
                ))
            })
    }

    /// A private channel's people: the box, those picked, those found, Skip and Add.
    fn people(
        &mut self,
        group: &str,
        thread: Option<&String>,
        palette: Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let input = self.people_box(window, cx);
        let name = self.name.read(cx).value().trim().to_owned();
        let found: Vec<_> = self
            .suggestions
            .iter()
            .cloned()
            .enumerate()
            .map(|(at, person)| Self::person_row(at, person, palette, cx))
            .collect();
        let picked: Vec<_> = self
            .picked
            .iter()
            .enumerate()
            .map(|(at, person)| Self::picked_chip(at, person, palette, cx))
            .collect();
        let none_found = !self.suggested_for.is_empty() && self.suggestions.is_empty();
        div()
            .debug_selector(|| "channel-people".to_owned())
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_xl()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(format!("Add members to the {name} channel")),
            )
            .child(
                div()
                    .text_sm()
                    .child("Start typing a name of a person you want to add to this private channel."),
            )
            .child(
                div()
                    .debug_selector(|| "channel-people-box".to_owned())
                    .child(Input::new(&input)),
            )
            .child(div().flex().flex_wrap().gap_1().children(picked))
            .child(
                div()
                    .id("channel-people-found")
                    .track_focus(&self.found_focus)
                    .on_key_down(keyboard::list_keys(self.found_focus.clone()))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(found)
                    .when(none_found, |list| {
                        list.child(
                            div()
                                .debug_selector(|| "channel-people-none".to_owned())
                                .text_sm()
                                .text_color(rgb(palette.muted_foreground))
                                .child("We didn't find any matches"),
                        )
                    }),
            )
            .child(self.people_buttons(group, thread, cx))
    }

    /// Someone found, picked by a click.
    fn person_row(at: usize, person: TeamPerson, palette: Palette, cx: &mut Context<Self>) -> keyboard::Stop {
        let shown = person.clone();
        let row = div()
            .debug_selector(move || format!("channel-person-{at}"))
            .id(SharedString::from(format!("channel-person-{at}")))
            .px_2()
            .py_1()
            .flex()
            .flex_col()
            .rounded_md()
            .cursor_pointer()
            .hover(move |row| row.bg(rgb(palette.hover)))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.pick_person(person.clone(), window, cx);
            }))
            .child(shown.name)
            .children(
                shown
                    .email
                    .map(|email| div().text_xs().text_color(rgb(palette.muted_foreground)).child(email)),
            );
        keyboard::stop(SharedString::from(format!("channel-person-{at}")), row, palette)
    }

    /// Someone picked, with the button that takes them back.
    fn picked_chip(at: usize, person: &TeamPerson, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let mri = person.mri.clone();
        div()
            .debug_selector(move || format!("channel-picked-{at}"))
            .pl_2()
            .flex()
            .items_center()
            .gap_1()
            .rounded_md()
            .bg(rgb(palette.hover))
            .child(person.name.clone())
            .child(
                icon_button(
                    SharedString::from(format!("channel-unpick-{at}")),
                    Glyph::X,
                    format!("Remove {}", person.name),
                )
                .xsmall()
                .on_click(cx.listener(move |this, _, _, cx| this.unpick_person(&mri, cx))),
            )
    }

    /// Skip, which goes on to the channel, and Add, once someone is picked.
    fn people_buttons(&self, group: &str, thread: Option<&String>, cx: &mut Context<Self>) -> gpui::Div {
        let (group, thread) = (group.to_owned(), thread.cloned());
        div()
            .flex()
            .justify_end()
            .gap_2()
            .child(selected_by(
                "channel-people-skip",
                Button::new("channel-people-skip")
                    .outline()
                    .label("Skip")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.finish(group.clone(), thread.clone(), cx);
                    })),
            ))
            .child(selected_by(
                "channel-people-add",
                Button::new("channel-people-add")
                    .primary()
                    .label("Add")
                    .disabled(self.picked.is_empty())
                    .on_click(cx.listener(|this, _, _, cx| this.add_people(cx))),
            ))
    }

    /// Some people weren't added: Teams' count, and Close, which goes on to the channel.
    fn some_not_added(
        failed: usize,
        group: &str,
        thread: Option<&String>,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let (group, thread) = (group.to_owned(), thread.cloned());
        let words = if failed == 1 {
            "1 person couldn't be added".to_owned()
        } else {
            format!("{failed} people couldn't be added")
        };
        div()
            .debug_selector(|| "channel-people-failed".to_owned())
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.foreground))
                    .child(words),
            )
            .child(selected_by(
                "channel-people-close",
                Button::new("channel-people-close")
                    .outline()
                    .label("Close")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.finish(group.clone(), thread.clone(), cx);
                    })),
            ))
    }
}

impl Render for ChannelDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let step = self.step.clone();
        let content = match &step {
            Step::Form => self.form(palette, cx),
            Step::Creating => Self::waiting("channel-creating", "Adding the channel...", palette),
            Step::Failed(words) => Self::failure(words, None, palette, cx),
            Step::People { group, thread } => self.people(group, thread.as_ref(), palette, window, cx),
            Step::Adding { .. } => Self::waiting("channel-adding", "Adding members...", palette),
            Step::SomeNotAdded { failed, group, thread } => {
                Self::some_not_added(*failed, group, thread.as_ref(), palette, cx)
            }
            Step::Waiting => Self::waiting("channel-waiting", "Adding the channel...", palette),
            Step::Notice { words, open } => Self::failure(words, open.as_deref(), palette, cx),
        };
        div()
            .debug_selector(|| "channel-dialog".to_owned())
            .id("channel-dialog")
            .key_context("ChannelDialog")
            .track_focus(&self.focus)
            .occlude()
            .w(px(WIDTH))
            .max_w_full()
            .h(px(HEIGHT))
            .max_h(window.viewport_size().height - px(48.0))
            .p_6()
            .flex()
            .flex_col()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .text_color(rgb(palette.foreground))
            .shadow_lg()
            .on_click(|_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(Self::on_key))
            .child(content)
            .focus_trap("channel-dialog-trap", &self.focus)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn taken() -> Vec<String> {
        vec!["General".to_owned(), "Prod Alerts".to_owned()]
    }

    #[test]
    fn names_are_checked_by_teams_rules_in_teams_words() {
        for (name, says) in [
            ("", "Required."),
            ("   ", "Required."),
            (
                "_hidden",
                "Channel name cannot start with an underscore (_) or a period (.)",
            ),
            (
                ".dot",
                "Channel name cannot start with an underscore (_) or a period (.)",
            ),
            ("dot.", "Channel name cannot end with a period (.)"),
            ("a#b", "We don't support these characters: ~#%&*{}+/\\:<>?|'\".."),
            ("a..b", "We don't support these characters: ~#%&*{}+/\\:<>?|'\".."),
            ("con", "Channel name can't be con. Try another name."),
            ("lpt9", "Channel name can't be lpt9. Try another name."),
            ("general", "The name General is reserved. Try another name."),
            ("GENERAL", "The name General is reserved. Try another name."),
            ("prod alerts", "Channel name is already taken."),
            ("www.example.com", "Links are not allowed in the channel name"),
            (
                "see https://example.com now",
                "We don't support these characters: ~#%&*{}+/\\:<>?|'\"..",
            ),
            ("Forms", "The name Forms is reserved. Try another name"),
        ] {
            assert_eq!(name_problem(name, &taken()).as_deref(), Some(says), "{name}");
        }
        for name in [
            "Security Alerts",
            "AI Discussion",
            "Mixed Reality (MR)",
            "a.b",
            "  spaced  ",
        ] {
            assert_eq!(name_problem(name, &taken()), None, "{name}");
        }
    }

    #[test]
    fn a_description_may_hold_anything_but_a_link() {
        assert_eq!(description_problem("what we do here: 100% of it"), None);
        assert_eq!(
            description_problem("see https://example.com"),
            Some("Links are not allowed in the channel description")
        );
    }

    #[test]
    fn only_a_problem_trying_again_can_help_offers_it() {
        assert!(problem_words(ChannelProblem::Other).retry);
        for problem in [
            ChannelProblem::NameTaken,
            ChannelProblem::TooManyPrivateChannels,
            ChannelProblem::TooManyChannels,
            ChannelProblem::NoTeamOwner,
            ChannelProblem::NoPermission,
            ChannelProblem::PrivateNotAllowed,
            ChannelProblem::TeamArchived,
            ChannelProblem::InvalidUsers,
            ChannelProblem::DescriptionTooLong,
        ] {
            assert!(!problem_words(problem).retry, "{problem:?}");
        }
    }
}
