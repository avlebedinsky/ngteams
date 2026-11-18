//! "Share this file", as Teams opens it from a file card's Share and from its "More options > Share" (seen with lab
//! accounts on 2026-11-18, `docs/protocol/chat.md`, "Files: shared on"): the file sent on to any chat or channel, with
//! an optional message.
//!
//! - **The dialog**: the title, "You can share to any chat or channel.", Close; "Add recipients" (marked as necessary)
//!   with its box; "Message preview": a box with "Add a message (optional)" over the file's card (its type's icon, its
//!   name, what its link lets people do as the file's site says it, and its picture); then Copy link and Share. Share
//!   stays off until a recipient is picked, and while the file is being shared.
//! - **The recipients** are found as Teams' picker finds them, as the box's text changes: people (the account's 1:1
//!   chats' partners, then the people the directory finds, never the account itself), group and meeting chats by
//!   their names and their people's, and the teams' channels by their names and their team's, each with its own face.
//!   A query with no match says "We couldn't find any matches." A pick is a chip with its face, its name and a remove
//!   mark; Backspace in the empty box takes the last one off.
//! - **Share** sends the file to each recipient as Teams does: one ordinary message to the recipient's chat or channel,
//!   its text the message, its `properties.files` the file's entry again ([`teams_proto::chat::share_entry_any`],
//!   [`MessageText::with_shared`]). A person with no 1:1 chat yet gets one first ([`Action::CreateChat`], as New
//!   message makes one). When all went out the dialog closes and the shell says "You've shared <name>"; a recipient
//!   it couldn't reach stays picked, the others go, and the dialog says so.
//! - **Copy link** puts the file's sharing link on the clipboard and closes the dialog; the shell says so.
//! - **Keys**: the box has the keyboard when the dialog opens; Up and Down move in the suggestions, Enter takes the
//!   one highlighted (else the first); Escape closes the suggestions and clears the box, else closes the dialog; Enter
//!   in the message (Shift+Enter: a new line) shares; Tab goes from the box to the message, then to the buttons.
//!
//! Nothing here writes but through [`Core`]; the read-only app offers no Share. Traced: `share-file opened`,
//! `shared`, `closed`, the conversations by id, never a name or a text.

use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement as _, IntoElement, KeyBinding, KeyDownEvent, ObjectFit, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, StyledImage as _, Subscription, Task, WeakEntity,
    Window, actions, div, img, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{
    Backspace, Escape, Input, InputEvent, InputState, MoveDown, MoveUp, Textarea, TextareaState,
};
use gpui_component::{Disableable as _, FocusTrapElement as _, Size};
use teams_core::{Action, SearchScope};
use teams_proto::Mri;
use teams_proto::chat::{LinkPermission, MessageText, Node, RichText, SharedFile, share_entry_any};
use teams_store::OutboxState;

use crate::bridge::Core;
use crate::chat_list::{self, ChatList, ChatRow, RowKind};
use crate::files::{PICTURE_RATIO, header_colour, kind_icon, no_picture, picture_colour};
use crate::icons::{Glyph, Spinning as _, icon_button, selected_by};
use crate::inline::{PictureSource, PictureState, thumbnail};
use crate::people::{self, Picture};
use crate::sending::settled_row;
use crate::switcher::matches;
use crate::teams_list::TeamsList;
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};

actions!(share_file, [ToMessage]);

/// The recipients' box's key context.
const TO_CONTEXT: &str = "ShareFileTo";

/// Binds the dialog's keys, once per app: Tab in the recipients' box goes to the message, as in Teams; the kit's Tab
/// would stop on the box's own controls first.
pub fn init(cx: &mut gpui::App) {
    cx.bind_keys([KeyBinding::new("tab", ToMessage, Some(TO_CONTEXT))]);
}

/// The dialog's words, as Teams has them.
pub const TITLE: &str = "Share this file";
pub const SUBTITLE: &str = "You can share to any chat or channel.";
pub const RECIPIENTS: &str = "Add recipients";
pub const RECIPIENTS_HINT: &str = "Enter name, email, group or tag";
pub const PREVIEW: &str = "Message preview";
pub const MESSAGE_HINT: &str = "Add a message (optional)";
pub const NO_MATCH: &str = "We couldn't find any matches.";
pub const COPY_LINK: &str = "Copy link";
pub const SHARE: &str = "Share";

/// The dialog's width, px, as Teams draws it.
const WIDTH: f32 = 600.0;
/// The file card's width in the preview, px.
const CARD: f32 = 320.0;
/// A suggestion's face, px.
const FACE: f32 = 32.0;
/// The most people, chats and channels suggested, each.
const MOST: usize = 6;
/// How long typing rests before the directory is asked.
const PAUSE: Duration = Duration::from_millis(200);
/// The suggestions' most height before they scroll, px.
const SUGGESTIONS_MAX: f32 = 300.0;

/// What the dialog tells the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShareFileEvent {
    /// Closed with nothing shared.
    Closed,
    /// The file (by its name) went to every recipient.
    Shared(SharedString),
    /// Copy link: the file's sharing link, for the clipboard.
    CopyLink(String),
}

/// A recipient, picked or suggested.
#[derive(Clone, Debug)]
pub enum Target {
    /// Someone: their 1:1 chat, made when there is none.
    Person {
        mri: String,
        name: SharedString,
        detail: SharedString,
        picture: Option<Picture>,
    },
    /// A group or meeting chat.
    Chat(Box<ChatRow>),
    /// A team's channel: a new post in it.
    Channel {
        id: String,
        name: SharedString,
        team: SharedString,
        team_id: String,
    },
}

impl Target {
    /// What tells it from any other.
    #[must_use]
    pub fn key(&self) -> &str {
        match self {
            Self::Person { mri, .. } => mri,
            Self::Chat(row) => &row.id,
            Self::Channel { id, .. } => id,
        }
    }

    /// Its name, as its chip shows it.
    #[must_use]
    pub fn name(&self) -> SharedString {
        match self {
            Self::Person { name, .. } | Self::Channel { name, .. } => name.clone(),
            Self::Chat(row) => row.name.clone(),
        }
    }
}

/// Where the dialog is.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Step {
    Ready,
    Sharing,
    /// Some recipients weren't reached: what to tell.
    Failed(SharedString),
}

/// "Share this file" (see the module docs).
pub struct ShareFileDialog {
    core: Arc<dyn Core>,
    trace: Arc<ViewTrace>,
    list: Entity<ChatList>,
    teams: Entity<TeamsList>,
    file: SharedFile,
    source: PictureSource,
    permission: Option<LinkPermission>,
    to: Entity<InputState>,
    message: Entity<TextareaState>,
    picked: Vec<Target>,
    suggestions: Vec<Target>,
    suggesting: bool,
    highlighted: Option<usize>,
    /// The directory's people for what the box said when they were asked.
    found_people: Vec<Target>,
    asking: Option<Task<()>>,
    step: Step,
    tasks: Vec<Task<()>>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ShareFileEvent> for ShareFileDialog {}

impl ShareFileDialog {
    pub fn new(
        (core, trace): (Arc<dyn Core>, Arc<ViewTrace>),
        (list, teams): (Entity<ChatList>, Entity<TeamsList>),
        file: SharedFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        trace.record(&TraceLine::new("share-file", "opened"));
        let to = cx.new(|cx| InputState::new(window, cx).placeholder(RECIPIENTS_HINT));
        let message = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(MESSAGE_HINT)
                .soft_wrap(true)
                .auto_grow(1, 6)
                .submit_on_enter(true)
        });
        let subscriptions = vec![
            cx.subscribe_in(&to, window, |this: &mut Self, _, event: &InputEvent, window, cx| {
                this.on_to(event, window, cx);
            }),
            cx.subscribe_in(
                &message,
                window,
                |this: &mut Self, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { shift: false, .. }) {
                        this.share(window, cx);
                    }
                },
            ),
            cx.observe(&list, |this: &mut Self, _, cx| this.refresh_suggestions(cx)),
        ];
        let mut dialog = Self {
            source: PictureSource::new(Arc::clone(&core)),
            core,
            trace,
            list,
            teams,
            file,
            permission: None,
            to,
            message,
            picked: Vec::new(),
            suggestions: Vec::new(),
            suggesting: false,
            highlighted: None,
            found_people: Vec::new(),
            asking: None,
            step: Step::Ready,
            tasks: Vec::new(),
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        dialog.read_permission(cx);
        dialog
    }

    /// Gives the recipients' box the keyboard, as the dialog opens.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.to.update(cx, |to, cx| to.focus(window, cx));
    }

    /// The file it shares.
    #[must_use]
    pub fn file(&self) -> &SharedFile {
        &self.file
    }

    /// The recipients picked, in order.
    #[must_use]
    pub fn picked(&self) -> &[Target] {
        &self.picked
    }

    /// The suggestions shown for what the box says.
    #[must_use]
    pub fn suggestions(&self) -> &[Target] {
        &self.suggestions
    }

    /// What the link lets people do, once it was read.
    #[must_use]
    pub fn permission(&self) -> Option<&LinkPermission> {
        self.permission.as_ref()
    }

    /// Whether Share is on: a recipient picked, nothing being shared.
    #[must_use]
    pub fn can_share(&self) -> bool {
        !self.picked.is_empty() && self.step != Step::Sharing
    }

    /// What the dialog says went wrong, while it says it.
    #[must_use]
    pub fn problem(&self) -> Option<&SharedString> {
        match &self.step {
            Step::Failed(words) => Some(words),
            _ => None,
        }
    }

    /// Reads what the file's link lets people do, for its card's line and its entry's scope.
    fn read_permission(&mut self, cx: &mut Context<Self>) {
        let (Some(object), Some(share)) = (self.file.object_url.clone(), self.file.share_url.clone()) else {
            return;
        };
        let reply = self.core.link_permission(object, share);
        self.tasks.push(cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let permission = reply.await.ok().flatten();
            let _ = this.update(cx, |this, cx| {
                this.permission = permission;
                cx.notify();
            });
        }));
    }

    // --- the recipients ---

    fn on_to(&mut self, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            InputEvent::Change => {
                self.highlighted = None;
                self.suggesting = true;
                self.ask_directory(cx);
                self.refresh_suggestions(cx);
            }
            InputEvent::Focus => {
                self.suggesting = true;
                self.refresh_suggestions(cx);
            }
            InputEvent::Blur => {
                self.suggesting = false;
                cx.notify();
            }
            InputEvent::PressEnter { .. } => {
                if self.query(cx).is_empty() {
                    self.share(window, cx);
                } else {
                    self.choose(self.highlighted.unwrap_or(0), window, cx);
                }
            }
        }
    }

    /// What the box says, trimmed.
    fn query(&self, cx: &gpui::App) -> String {
        self.to.read(cx).value().trim().to_owned()
    }

    /// Puts `text` in the box as if it were typed, its suggestions open.
    pub fn set_query(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.to.update(cx, |to, cx| to.set_value(text.to_owned(), window, cx));
        self.highlighted = None;
        self.suggesting = true;
        self.ask_directory(cx);
        self.refresh_suggestions(cx);
    }

    /// Puts `text` in the message box.
    pub fn set_message(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.message
            .update(cx, |message, cx| message.set_value(text.to_owned(), window, cx));
    }

    /// Asks the directory for the people the box names, a moment after the typing stops; an answer to what the box
    /// no longer says is dropped.
    fn ask_directory(&mut self, cx: &mut Context<Self>) {
        let query = self.query(cx);
        self.found_people.clear();
        if query.is_empty() {
            self.asking = None;
            return;
        }
        let core = Arc::clone(&self.core);
        self.asking = Some(cx.spawn(async move |this: WeakEntity<Self>, cx| {
            cx.background_executor().timer(PAUSE).await;
            let answer = core
                .search_service(query.clone(), SearchScope::Suggestions)
                .await
                .unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                if this.query(cx) != query {
                    return;
                }
                this.asking = None;
                this.found_people = answer
                    .people
                    .into_iter()
                    .filter_map(|person| {
                        let mri = person.mri.filter(|mri| mri.starts_with("8:orgid:"))?;
                        let detail = person.job_title.or(person.email).unwrap_or_default();
                        Some(Target::Person {
                            mri,
                            name: person.name.into(),
                            detail: detail.into(),
                            picture: None,
                        })
                    })
                    .collect();
                this.refresh_suggestions(cx);
            });
        }));
    }

    /// The suggestions again from what the box says (see the module docs).
    fn refresh_suggestions(&mut self, cx: &mut Context<Self>) {
        let query = self.query(cx);
        self.suggestions = if query.is_empty() {
            Vec::new()
        } else {
            let mut found = self.people(&query, cx);
            found.extend(self.chats(&query, cx));
            found.extend(self.channels(&query, cx));
            found
        };
        if self.highlighted.is_some_and(|at| at >= self.suggestions.len()) {
            self.highlighted = None;
        }
        cx.notify();
    }

    /// Whether `key` is already picked.
    fn is_picked(&self, key: &str) -> bool {
        self.picked.iter().any(|target| target.key() == key)
    }

    /// The people `query` finds: the 1:1 chats' partners, then the directory's, never the account or one picked.
    fn people(&self, query: &str, cx: &gpui::App) -> Vec<Target> {
        let me = self.core.me();
        let list = self.list.read(cx);
        let partners: Vec<&ChatRow> = list
            .findable_rows()
            .filter(|row| row.kind == RowKind::OneToOne && row.named)
            .filter(|row| row.partner.as_deref().is_some_and(|mri| mri != me))
            .collect();
        let named: Vec<(String, SharedString)> =
            partners.iter().map(|row| (row.id.clone(), row.name.clone())).collect();
        let mut people: Vec<Target> = matches(query, &named)
            .into_iter()
            .filter_map(|(id, _)| partners.iter().find(|row| row.id == id))
            .map(|row| Target::Person {
                mri: row.partner.clone().unwrap_or_default(),
                name: row.name.clone(),
                detail: SharedString::default(),
                picture: row.picture.clone(),
            })
            .collect();
        for person in &self.found_people {
            if person.key() != me && !people.iter().any(|known| known.key() == person.key()) {
                people.push(person.clone());
            }
        }
        people.retain(|person| !self.is_picked(person.key()));
        people.truncate(MOST);
        people
    }

    /// The group and meeting chats `query` finds by their names and their people's.
    fn chats(&self, query: &str, cx: &gpui::App) -> Vec<Target> {
        let list = self.list.read(cx);
        let chats: Vec<&ChatRow> = list
            .findable_rows()
            .filter(|row| matches!(row.kind, RowKind::Group | RowKind::Meeting))
            .filter(|row| !self.is_picked(&row.id))
            .collect();
        let named: Vec<(String, SharedString)> =
            chats.iter().map(|row| (row.id.clone(), searchable_name(row))).collect();
        matches(query, &named)
            .into_iter()
            .filter_map(|(id, _)| chats.iter().find(|row| row.id == id))
            .take(MOST)
            .map(|row| Target::Chat(Box::new((*row).clone())))
            .collect()
    }

    /// The channels `query` finds by their names and their team's.
    fn channels(&self, query: &str, cx: &gpui::App) -> Vec<Target> {
        let teams = self.teams.read(cx).teams().unwrap_or_default();
        let channels: Vec<Target> = teams
            .iter()
            .filter(|team| !team.archived)
            .flat_map(|team| {
                team.channels.iter().map(|channel| Target::Channel {
                    id: channel.id.clone(),
                    name: channel.name.clone().into(),
                    team: team.name.clone().into(),
                    team_id: team.id.clone(),
                })
            })
            .filter(|channel| !self.is_picked(channel.key()))
            .collect();
        let named: Vec<(String, SharedString)> = channels
            .iter()
            .map(|channel| match channel {
                Target::Channel { id, name, team, .. } => (id.clone(), format!("{name} {team}").into()),
                other => (other.key().to_owned(), other.name()),
            })
            .collect();
        matches(query, &named)
            .into_iter()
            .filter_map(|(id, _)| channels.iter().find(|channel| channel.key() == id))
            .take(MOST)
            .cloned()
            .collect()
    }

    /// Moves the highlight `by` rows, wrapping.
    fn move_highlight(&mut self, by: isize, cx: &mut Context<Self>) {
        let count = self.suggestions.len();
        if count == 0 {
            return;
        }
        let next = match self.highlighted {
            None if by > 0 => 0,
            None => count - 1,
            Some(at) => (at.cast_signed() + by).rem_euclid(count.cast_signed()).cast_unsigned(),
        };
        self.highlighted = Some(next);
        cx.notify();
    }

    /// Picks the suggestion at `at`; nothing while the file is being shared.
    pub fn choose(&mut self, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.step == Step::Sharing {
            return;
        }
        let Some(chosen) = self.suggestions.get(at).cloned() else {
            return;
        };
        self.picked.push(chosen);
        self.to.update(cx, |to, cx| to.set_value("", window, cx));
        self.found_people.clear();
        self.highlighted = None;
        self.refresh_suggestions(cx);
    }

    /// Takes the recipient at `at` off; nobody while the file is being shared.
    pub fn remove(&mut self, at: usize, cx: &mut Context<Self>) {
        if at < self.picked.len() && self.step != Step::Sharing {
            self.picked.remove(at);
            self.refresh_suggestions(cx);
        }
    }

    /// Backspace in the empty box: the last recipient comes off.
    fn backspace(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.to.read(cx).value().is_empty() || self.picked.is_empty() {
            return false;
        }
        self.remove(self.picked.len() - 1, cx);
        true
    }

    /// Escape: the suggestions close and the box is cleared, else the dialog closes.
    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.suggesting && !self.to.read(cx).value().is_empty() {
            self.to.update(cx, |to, cx| to.set_value("", window, cx));
            self.suggesting = false;
            self.refresh_suggestions(cx);
            return;
        }
        self.close(cx);
    }

    /// Closes the dialog with nothing more shared.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.trace.record(&TraceLine::new("share-file", "closed"));
        cx.emit(ShareFileEvent::Closed);
    }

    /// Copy link: the file's sharing link, for the shell to put on the clipboard.
    pub fn copy_link(&mut self, cx: &mut Context<Self>) {
        if let Some(link) = self.file.open_link() {
            cx.emit(ShareFileEvent::CopyLink(link.to_owned()));
        }
    }

    // --- sharing ---

    /// The message as it is sent: each line a paragraph, nothing when it says nothing.
    fn text(&self, cx: &gpui::App) -> Option<MessageText> {
        let typed = self.message.read(cx).value().to_string();
        let nodes = typed
            .trim()
            .lines()
            .map(|line| Node::Paragraph(RichText::plain(line).nodes))
            .collect();
        let text = MessageText::new(
            RichText {
                nodes,
                truncated: false,
            },
            [],
        )
        .ok()?;
        let entry = share_entry_any(
            &self.file,
            self.permission.as_ref().map(|permission| permission.scope.as_str()),
        )?;
        Some(text.with_shared(vec![entry]))
    }

    /// Share: the file sent to each recipient (see the module docs).
    pub fn share(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if !self.can_share() || self.core.read_only() {
            return;
        }
        let Some(text) = self.text(cx) else {
            self.step = Step::Failed("This file can't be shared from here.".into());
            cx.notify();
            return;
        };
        self.step = Step::Sharing;
        self.suggesting = false;
        let targets = self.picked.clone();
        let core = Arc::clone(&self.core);
        let chats: Vec<(String, String)> = self
            .list
            .read(cx)
            .findable_rows()
            .filter(|row| row.kind == RowKind::OneToOne)
            .filter_map(|row| Some((row.partner.clone()?, row.id.clone())))
            .collect();
        self.tasks
            .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let mut missed = Vec::new();
                for target in targets {
                    if !send_to(&core, &target, &chats, text.clone(), cx).await {
                        missed.push(target);
                    }
                }
                let _ = this.update(cx, |this, cx| this.shared(missed, cx));
            }));
        cx.notify();
    }

    /// The share's end: every recipient reached closes the dialog, else those missed stay picked.
    fn shared(&mut self, missed: Vec<Target>, cx: &mut Context<Self>) {
        if missed.is_empty() {
            self.step = Step::Ready;
            self.trace.record(&TraceLine::new("share-file", "shared"));
            cx.emit(ShareFileEvent::Shared(self.file.name.clone().into()));
            return;
        }
        let names: Vec<String> = missed.iter().map(|target| target.name().to_string()).collect();
        self.step = Step::Failed(format!("We couldn't share the file with {}. Try again.", names.join(", ")).into());
        self.picked = missed;
        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            cx.stop_propagation();
            self.escape(window, cx);
        }
    }
}

/// Sends `text` to `target`'s conversation, making a person's 1:1 chat first when `chats` (partner, chat) has none;
/// whether the outbox sent it.
async fn send_to(
    core: &Arc<dyn Core>,
    target: &Target,
    chats: &[(String, String)],
    text: MessageText,
    cx: &mut gpui::AsyncApp,
) -> bool {
    let conversation = match target {
        Target::Chat(row) => row.id.clone(),
        Target::Channel { id, .. } => id.clone(),
        Target::Person { mri, .. } => {
            if let Some((_, chat)) = chats.iter().find(|(partner, _)| partner == mri) {
                chat.clone()
            } else {
                let reply = core.write(Action::CreateChat {
                    others: vec![Mri::new(mri)],
                    topic: None,
                });
                let Some(row) = settled_row(core.as_ref(), reply, cx.background_executor()).await else {
                    return false;
                };
                match row.message_id.filter(|_| row.state == OutboxState::Sent) {
                    Some(chat) => chat,
                    None => return false,
                }
            }
        }
    };
    let reply = core.write(Action::Send { conversation, text });
    settled_row(core.as_ref(), reply, cx.background_executor())
        .await
        .is_some_and(|row| row.state == OutboxState::Sent)
}

/// What a group chat is found by: its name, and the names of the members its avatar shows.
fn searchable_name(row: &ChatRow) -> SharedString {
    let mut name = row.name.to_string();
    for face in &row.faces {
        if let Some(person) = &face.name {
            name.push(' ');
            name.push_str(person);
        }
    }
    name.into()
}

// --- drawing ---

impl ShareFileDialog {
    fn header(palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let close = selected_by(
            "share-file-close",
            icon_button("share-file-close", Glyph::X, "Close").on_click(cx.listener(|this, _, _, cx| this.close(cx))),
        );
        div()
            .flex()
            .items_start()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .debug_selector(|| "share-file-title".to_owned())
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(TITLE),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(palette.muted_foreground))
                            .child(SUBTITLE),
                    ),
            )
            .child(close)
            .into_any_element()
    }

    /// A field's label: its words, and Teams' red mark when it is necessary.
    fn label(words: &'static str, necessary: bool, palette: Palette) -> AnyElement {
        div()
            .flex()
            .items_center()
            .gap_1()
            .text_sm()
            .text_color(rgb(palette.foreground))
            .child(words)
            .when(necessary, |label| {
                label.child(div().text_color(rgb(theme::DANGER)).child("*"))
            })
            .into_any_element()
    }

    /// A target's face, `size` across: a person's picture or initials, a chat's avatar, a channel's team's.
    fn face(&self, target: &Target, size: f32, palette: Palette, cx: &gpui::App) -> AnyElement {
        match target {
            Target::Person { name, picture, .. } => {
                people::avatar(Some(name), picture.as_ref(), Size::Size(px(size))).into_any_element()
            }
            Target::Chat(row) => chat_list::chat_avatar(row, palette, Size::Size(px(size)), "share-file-face"),
            Target::Channel { team_id, team, .. } => {
                let teams = self.teams.read(cx);
                match teams
                    .teams()
                    .unwrap_or_default()
                    .iter()
                    .find(|known| known.id == *team_id)
                {
                    Some(known) => teams.avatar(known, size, false, palette),
                    None => people::avatar(Some(team), None, Size::Size(px(size))).into_any_element(),
                }
            }
        }
    }

    fn chip(&self, at: usize, target: &Target, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let remove = div()
            .id(SharedString::from(format!("share-file-chip-remove-{at}")))
            .debug_selector(move || format!("share-file-chip-remove-{at}"))
            .flex_none()
            .p_0p5()
            .rounded_sm()
            .cursor_pointer()
            .text_color(rgb(palette.muted_foreground))
            .hover(move |button| button.bg(rgb(palette.hover)))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.remove(at, cx)))
            .child(Glyph::X.icon().size(px(12.0)));
        div()
            .debug_selector(move || format!("share-file-chip-{at}"))
            .flex_none()
            .h(px(26.0))
            .pl_0p5()
            .pr_1p5()
            .flex()
            .items_center()
            .gap_1p5()
            .rounded_full()
            .bg(rgb(palette.surface))
            .border_1()
            .border_color(rgb(palette.border))
            .child(match target {
                // the kit's small avatar, as New message's chips draw a person: its initials fit
                Target::Person { name, picture, .. } => {
                    people::avatar(Some(name), picture.as_ref(), Size::Small).into_any_element()
                }
                _ => self.face(target, 20.0, palette, cx),
            })
            .child(div().text_sm().child(target.name()))
            .child(remove)
            .into_any_element()
    }

    /// The recipients' box: the chips, then the box typed in, Teams' tinted field with the brand's line while it has
    /// the keyboard.
    fn recipients(&self, window: &Window, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let chips: Vec<AnyElement> = self
            .picked
            .iter()
            .enumerate()
            .map(|(at, target)| self.chip(at, target, palette, cx))
            .collect();
        let focused = self.to.read(cx).focus_handle(cx).is_focused(window);
        div()
            .id("share-file-to")
            .debug_selector(|| "share-file-to".to_owned())
            .key_context(TO_CONTEXT)
            .on_action(cx.listener(|this, _: &ToMessage, window, cx| {
                this.suggesting = false;
                this.message.update(cx, |message, cx| message.focus(window, cx));
                cx.notify();
            }))
            .min_h(px(34.0))
            .px_3()
            .py_1()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1p5()
            .rounded_md()
            .bg(rgb(field(palette)))
            .border_b_2()
            .border_color(rgb(if focused { palette.brand } else { field(palette) }))
            .capture_action(cx.listener(|this, _: &MoveUp, _, cx| {
                if this.suggesting {
                    this.move_highlight(-1, cx);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &MoveDown, _, cx| {
                if this.suggesting {
                    this.move_highlight(1, cx);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                cx.stop_propagation();
                this.escape(window, cx);
            }))
            .capture_action(cx.listener(|this, _: &Backspace, _, cx| {
                if this.backspace(cx) {
                    cx.stop_propagation();
                }
            }))
            .children(chips)
            .child(
                div()
                    .debug_selector(|| "share-file-to-input".to_owned())
                    .flex_1()
                    .min_w(px(160.0))
                    .child(Input::new(&self.to).appearance(false).aria_label(RECIPIENTS)),
            )
            .into_any_element()
    }

    fn suggestion(&self, at: usize, target: &Target, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let highlighted = self.highlighted == Some(at);
        let (name, detail, bold_detail) = match target {
            Target::Person { name, detail, .. } => (name.clone(), detail.clone(), false),
            Target::Chat(row) => (row.name.clone(), SharedString::default(), false),
            Target::Channel { name, team, .. } => (name.clone(), team.clone(), true),
        };
        div()
            .id(SharedString::from(format!("share-file-suggestion-{at}")))
            .debug_selector(move || format!("share-file-suggestion-{at}"))
            .w_full()
            .min_h(px(52.0))
            .px_4()
            .py_1p5()
            .flex()
            .items_center()
            .gap_3()
            .rounded_md()
            .cursor_pointer()
            .when(highlighted, |row| row.bg(rgb(palette.selected)))
            .hover(move |row| row.bg(rgb(palette.hover)))
            // a press would take the keyboard from the box, closing the suggestions before the click lands
            .on_mouse_down(gpui::MouseButton::Left, |_, window, _| window.prevent_default())
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.choose(at, window, cx)))
            .child(self.face(target, FACE, palette, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().text_sm().truncate().child(name))
                    .when(!detail.is_empty(), |lines| {
                        lines.child(
                            div()
                                .text_xs()
                                .text_color(rgb(palette.muted_foreground))
                                .when(bold_detail, |line| line.font_weight(FontWeight::SEMIBOLD))
                                .truncate()
                                .child(detail),
                        )
                    }),
            )
            .into_any_element()
    }

    /// The suggestions under the box while it has the keyboard and says something: "We couldn't find any matches."
    /// once nothing matches and the directory answered.
    fn suggestions_panel(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.suggesting || self.query(cx).is_empty() {
            return None;
        }
        let rows: Vec<AnyElement> = if self.suggestions.is_empty() {
            if self.asking.is_some() {
                return None;
            }
            vec![
                div()
                    .debug_selector(|| "share-file-no-match".to_owned())
                    .px_3()
                    .py_2()
                    .text_sm()
                    .child(NO_MATCH)
                    .into_any_element(),
            ]
        } else {
            self.suggestions
                .iter()
                .enumerate()
                .map(|(at, target)| self.suggestion(at, target, palette, cx))
                .collect()
        };
        Some(
            div()
                .id("share-file-suggestions")
                .debug_selector(|| "share-file-suggestions".to_owned())
                .absolute()
                .occlude()
                .top(px(2.0))
                .left_0()
                .right_0()
                .max_h(px(SUGGESTIONS_MAX))
                .overflow_y_scroll()
                .p_1()
                .bg(rgb(palette.surface))
                .border_1()
                .border_color(rgb(palette.border))
                .rounded_lg()
                .shadow_lg()
                .children(rows)
                .into_any_element(),
        )
    }

    /// The file's card in the preview: its type's icon, its name and what its link lets people do, over its picture.
    fn file_card(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let kind = self.file.kind();
        let permission = self
            .permission
            .as_ref()
            .and_then(|permission| permission.description.clone());
        let state = match self.file.share_url.as_deref() {
            Some(share_url) if kind.has_thumbnail() => thumbnail(share_url, &self.source, cx),
            _ => PictureState::Missing,
        };
        let picture = match state {
            PictureState::Shown(image, _) => img(image).size_full().object_fit(ObjectFit::Cover).into_any_element(),
            PictureState::Reading => div().size_full().into_any_element(),
            PictureState::Missing => no_picture(kind, "share-file-picture-icon"),
        };
        div()
            .debug_selector(|| "share-file-card".to_owned())
            .w(px(CARD))
            .max_w_full()
            .flex()
            .flex_col()
            .rounded_md()
            .overflow_hidden()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(header_colour(palette)))
            .child(
                div()
                    .px_3()
                    .py_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(kind_icon(kind, 28.0, "share-file-icon".to_owned()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_sm()
                                    .truncate()
                                    .child(self.file.name.clone()),
                            )
                            .children(permission.map(|words| {
                                div()
                                    .debug_selector(|| "share-file-permission".to_owned())
                                    .text_xs()
                                    .truncate()
                                    .child(words)
                            })),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .aspect_ratio(PICTURE_RATIO)
                    .bg(rgb(picture_colour(palette)))
                    .child(picture),
            )
            .into_any_element()
    }

    /// "Message preview": the message's box over the file's card.
    fn preview(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        div()
            .debug_selector(|| "share-file-preview".to_owned())
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .rounded_md()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(field(palette)))
            .child(
                div()
                    .debug_selector(|| "share-file-message".to_owned())
                    .child(Textarea::new(&self.message).appearance(false)),
            )
            .child(self.file_card(palette, cx))
            .into_any_element()
    }

    fn buttons(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let sharing = self.step == Step::Sharing;
        let copy = selected_by(
            "share-file-copy-link",
            Button::new("share-file-copy-link")
                .outline()
                .label(COPY_LINK)
                .disabled(sharing || self.file.open_link().is_none())
                .on_click(cx.listener(|this, _, _, cx| this.copy_link(cx))),
        );
        let share = selected_by(
            "share-file-share",
            Button::new("share-file-share")
                .primary()
                .label(SHARE)
                .spinning(sharing)
                .disabled(!self.can_share())
                .on_click(cx.listener(|this, _, window, cx| this.share(window, cx))),
        );
        let problem = self.problem().cloned().map(|words| {
            div()
                .debug_selector(|| "share-file-problem".to_owned())
                .flex_1()
                .min_w_0()
                .text_sm()
                .text_color(rgb(theme::DANGER))
                .child(words)
        });
        div()
            .flex()
            .items_center()
            .justify_end()
            .gap_2()
            .text_color(rgb(palette.foreground))
            .children(problem)
            .child(copy)
            .child(share)
            .into_any_element()
    }
}

/// The colour of the dialog's fields: Teams' tint of the surface toward the text.
fn field(palette: Palette) -> u32 {
    theme::mix(palette.surface, palette.foreground, 0.06)
}

impl Focusable for ShareFileDialog {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ShareFileDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let suggestions = self.suggestions_panel(palette, cx);
        div()
            .debug_selector(|| "share-file-dialog".to_owned())
            .id("share-file-dialog")
            .key_context("ShareFileDialog")
            .track_focus(&self.focus)
            .occlude()
            .w(px(WIDTH))
            .max_w_full()
            .max_h(window.viewport_size().height - px(48.0))
            .p_8()
            .flex()
            .flex_col()
            .gap_5()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .text_color(rgb(palette.foreground))
            .shadow_lg()
            .on_click(|_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(Self::on_key))
            .child(Self::header(palette, cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(Self::label(RECIPIENTS, true, palette))
                    .child(self.recipients(window, palette, cx))
                    .child(div().relative().children(suggestions)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(Self::label(PREVIEW, false, palette))
                    .child(self.preview(palette, cx)),
            )
            .child(self.buttons(palette, cx))
            .focus_trap("share-file-trap", &self.focus)
    }
}
