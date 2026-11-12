//! A person's profile card, as browser Teams shows it (seen 2026-11-11 with lab accounts), from their avatar or their
//! name wherever the app shows one ([`anchor`]). Its data is the engine's ([`Core::profile_card`]: Loki's person,
//! schedule, organization and working-with, their presence and short profile), read as it opens; the name, picture
//! and presence the views already have show at once.
//!
//! - **Two forms.** The compact card ([`Form::Compact`]) opens beside a face or a name the pointer rests on, or a name
//!   clicked: the picture with its presence, the name and job title, the actions, then a quick message, the
//!   availability box, "Profile >" with the email and "Show more". It closes on Escape, a press outside, or the pointer
//!   leaving both it and what it opened from. The full card ([`Form::Full`]) opens centred over the dimmed window from a
//!   click on a face, or from the compact card's "Profile >", "Show more" and org chart: Overview, Profile and
//!   Organization tabs ([`CardTab`]), Close and Escape. A person on the Organization tab opens their card in the same
//!   dialog, "Back to {name}" returning to the one before on the same tab.
//! - **The actions**, as Teams' tooltips word them: Chat ([`ProfileCardEvent::OpenChat`]: the 1:1 chat with them, else
//!   [`ProfileCardEvent::StartChat`], a new one New message makes on its first message), the org chart (the full card's
//!   Organization tab), a video or an audio call ([`ProfileCardEvent::Call`]: their 1:1 chat's call, the camera turned on
//!   once a video call is up), the quick message (sent to the 1:1 chat through the outbox, the chat made first when there
//!   is none, "Quick message sent" after) and the email (`mailto:`).
//! - **A quick message outlives its card.** Its sending (the chat made, then the message posted, each watched until the
//!   outbox settles it) is a task of the app's, not the card's: closing the card, or the card going over to someone
//!   else, never drops a message half way, and only a card still showing its person hears how it went. One that failed
//!   is kept: sending the same text again sends its outbox row again ([`Core::resend`]), after the chat view's warning
//!   when it may have arrived, never as a new message. A press on a card opened by a rest, or typing in it, keeps it
//!   open.
//! - **The read-only mode** offers no quick message and no call; Chat still opens a chat there is. The account's own
//!   card has its calls drawn off, as Teams draws them, no quick message, and "Your local time".
//! - **Nobody without a card**: a face or a name of anyone but a directory user (a bot, a phone number) opens none
//!   ([`teams_core::profile_card::has_card`]).
//!
//! The views ask for a card through [`CardAsks`] (the models' `cards`), and the shell of the window asked from opens
//! it. Traced: `profile-card opened` (the form), `tab` (its key), `chat`, `call`, `quick-sent`, never a name or a text.

pub mod words;

mod draw;

pub use draw::{COMPACT_MOST, COMPACT_WIDTH, FULL_MOST};

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, AsyncWindowContext, BackgroundExecutor, Bounds, Context, ElementId, Entity,
    EventEmitter, FocusHandle, Focusable, InteractiveElement as _, IntoElement, ParentElement as _, Pixels,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity, Window, WindowId, div,
};
use gpui_component::input::{InputEvent, InputState};
use teams_core::{Action, ProfileCard as CardData};
use teams_proto::Mri;
use teams_proto::chat::MessageText;
use teams_store::{OutboxEntry, OutboxState};

use crate::bridge::Core;
use crate::chat_list::{self, ChatList, RowKind};
use crate::keyboard::Handles;
use crate::models::Models;
use crate::new_chat::Recipient;
use crate::people::{Picture, Shown};
use crate::sending::{Again, settled_row_unowned};
use crate::trace::{TraceLine, ViewTrace};

/// The quick message box's hint, as Teams words it.
pub const QUICK_HINT: &str = "Send a quick message";
/// What the card says once a quick message went.
pub const QUICK_SENT: &str = "Quick message sent";
/// What it says when one couldn't go.
pub const QUICK_FAILED: &str = "Your message wasn't sent. Check your connection and try again.";
/// What it says when one isn't known sent but may have arrived: Send sends it again, as the chat view's warning asks.
pub const QUICK_MAY_HAVE_ARRIVED: &str = crate::chat_view::MAY_HAVE_ARRIVED;
/// What it says when the card couldn't be read.
pub const NOT_LOADED: &str = "We couldn't load the profile card. Check your connection and try again.";
pub const CONTACT: &str = "Contact information";
pub const MORE_CONTACT: &str = "Show more contact information";
pub const SHOW_MORE: &str = "Show more";
pub const PROFILE: &str = "Profile";
pub const OUT_OF_OFFICE: &str = "Out of office";
pub const REPORTS_TO: &str = "Reports to";
pub const CLOSE: &str = "Close";
/// The contact rows' labels.
pub const EMAIL: &str = "Email";
pub const CHAT: &str = "Chat";
pub const JOB_TITLE: &str = "Job title";
pub const OFFICE: &str = "Office location";

/// How long the pointer rests on a face or a name before its card opens, and how long an open card waits for the
/// pointer to come back to it (or to what it opened from) before it closes.
pub const HOVER_OPEN: Duration = Duration::from_millis(800);
pub const HOVER_CLOSE: Duration = Duration::from_millis(400);
/// How long "Quick message sent" shows.
const NOTE_SHOWN: Duration = Duration::from_secs(4);

/// "Start a chat with {name}".
#[must_use]
pub fn chat_tip(name: &str) -> String {
    format!("Start a chat with {name}")
}

/// "Navigate to {name}'s org chart".
#[must_use]
pub fn org_tip(name: &str) -> String {
    format!("Navigate to {name}'s org chart")
}

/// "Start a video call with {name}".
#[must_use]
pub fn video_tip(name: &str) -> String {
    format!("Start a video call with {name}")
}

/// "Start an audio call with {name}".
#[must_use]
pub fn audio_tip(name: &str) -> String {
    format!("Start an audio call with {name}")
}

/// "Back to {name}".
#[must_use]
pub fn back_tip(name: &str) -> String {
    format!("Back to {name}")
}

/// The Organization tab's note when the directory names neither managers nor reports.
#[must_use]
pub fn no_org(name: &str) -> String {
    format!(
        "We don't have the necessary details to show the organization chart for {name}. Organization details are \
         provided by your admin or human resources department."
    )
}

/// "{name} works with".
#[must_use]
pub fn works_with(name: &str) -> String {
    format!("{name} works with")
}

/// "People reporting to {name} ({n})".
#[must_use]
pub fn reporting(name: &str, count: usize) -> String {
    format!("People reporting to {name} ({count})")
}

/// Which card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// The popover beside a face or a name.
    Compact,
    /// The dialog over the window.
    Full,
}

/// The full card's tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardTab {
    Overview,
    Profile,
    Organization,
}

impl CardTab {
    pub const ALL: [Self; 3] = [Self::Overview, Self::Profile, Self::Organization];

    /// Its key in selectors and the trace.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Profile => "profile",
            Self::Organization => "organization",
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Profile => "Profile",
            Self::Organization => "Organization",
        }
    }
}

/// Who a card is of, as the place it opened from shows them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardOf {
    pub mri: String,
    pub name: Option<SharedString>,
    pub picture: Option<Picture>,
}

impl CardOf {
    /// `mri`, named `name` when it is a name.
    #[must_use]
    pub fn new(mri: impl Into<String>, name: Option<SharedString>) -> Self {
        Self {
            mri: mri.into(),
            name,
            picture: None,
        }
    }

    /// With their picture, as the place shows it.
    #[must_use]
    pub fn with_picture(mut self, picture: Option<Picture>) -> Self {
        self.picture = picture;
        self
    }
}

/// A view's ask for a card, told by [`CardAsks`]; the shell of `window` opens it.
#[derive(Clone, Debug)]
pub enum CardAsk {
    /// The pointer came onto a face or a name: the compact card opens if it rests there.
    Hover {
        of: CardOf,
        anchor: Bounds<Pixels>,
        window: WindowId,
    },
    /// The pointer left it.
    Left { mri: String, window: WindowId },
    /// A click: the card opens at once.
    Open {
        of: CardOf,
        anchor: Bounds<Pixels>,
        form: Form,
        window: WindowId,
    },
}

impl CardAsk {
    /// The window asked from.
    #[must_use]
    pub fn window(&self) -> WindowId {
        match self {
            Self::Hover { window, .. } | Self::Left { window, .. } | Self::Open { window, .. } => *window,
        }
    }
}

/// Where the views ask for cards (the models' `cards`): each ask is told to whoever follows it.
#[derive(Debug, Default)]
pub struct CardAsks;

impl EventEmitter<CardAsk> for CardAsks {}

/// What a click on an [`anchor`] opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Click {
    /// A face: the full card.
    Full,
    /// A name: the compact card.
    Compact,
    /// Nothing: the place's own click stays (a row that opens a chat); the card opens on hover alone.
    Hover,
}

/// `element` (a face or a name) as one a card opens from: the compact card when the pointer rests on it, the card
/// `click` names on a click (which goes no further: the row under it doesn't take it). Its test selector is
/// `selector`. Anyone without a card gets `element` as it is.
pub fn anchor(
    selector: impl Into<String>,
    of: CardOf,
    click: Click,
    asks: &Entity<CardAsks>,
    element: impl IntoElement,
) -> AnyElement {
    if !teams_core::profile_card::has_card(&of.mri) {
        return element.into_any_element();
    }
    let selector = selector.into();
    let placed: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::default();
    let (hover_at, click_at) = (Rc::clone(&placed), Rc::clone(&placed));
    let (hover_asks, click_asks) = (asks.clone(), asks.clone());
    let (hover_of, click_of) = (of.clone(), of);
    let id = selector.clone();
    let inner = div()
        .id(ElementId::Name(id.into()))
        .debug_selector(move || selector.clone())
        .flex_none()
        .cursor_pointer()
        .on_hover(move |hovered, window, cx| {
            let window = window.window_handle().window_id();
            let ask = if *hovered {
                CardAsk::Hover {
                    of: hover_of.clone(),
                    anchor: hover_at.get().unwrap_or_default(),
                    window,
                }
            } else {
                CardAsk::Left {
                    mri: hover_of.mri.clone(),
                    window,
                }
            };
            hover_asks.update(cx, |_, cx| cx.emit(ask));
        })
        .when(click != Click::Hover, |inner| {
            inner.on_click(move |_, window, cx| {
                cx.stop_propagation();
                let form = if click == Click::Full {
                    Form::Full
                } else {
                    Form::Compact
                };
                let ask = CardAsk::Open {
                    of: click_of.clone(),
                    anchor: click_at.get().unwrap_or_default(),
                    form,
                    window: window.window_handle().window_id(),
                };
                click_asks.update(cx, |_, cx| cx.emit(ask));
            })
        })
        .child(element);
    div()
        .flex_none()
        .on_children_prepainted(move |children, _, _| placed.set(children.first().copied()))
        .child(inner)
        .into_any_element()
}

/// The chat the account has with `mri` alone, as the list knows it: their 1:1 chat (the account's own notes for
/// itself).
#[must_use]
pub fn chat_with(list: &ChatList, me: &str, mri: &str) -> Option<String> {
    if mri == me {
        return list.row(chat_list::NOTES).map(|row| row.id.clone());
    }
    list.all_rows()
        .find(|row| row.kind == RowKind::OneToOne && row.partner.as_deref() == Some(mri))
        .map(|row| row.id.clone())
        .or_else(|| {
            let id = teams_proto::one_to_one_id(me, mri)?;
            list.row(&id).map(|row| row.id.clone())
        })
}

/// What a card tells its shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfileCardEvent {
    /// Closed: Escape, Close, a press outside.
    Close,
    /// Chat: the chat with them, opened in its place.
    OpenChat(String),
    /// Chat with someone the account has no 1:1 chat with: a new one, made by its first message.
    StartChat(Recipient),
    /// A call to their 1:1 chat (`chat`), the camera on once it is up when `video`.
    Call { chat: String, video: bool },
    /// The compact card's "Profile >", "Show more" or org chart: the full card on that tab.
    Expand(CardTab),
    /// The pointer came onto the card (`true`) or left it.
    Hovered(bool),
    /// The card was pressed, or its quick message box typed in: a card opened by a rest stays open, as one opened by a
    /// click does.
    Engaged,
}

/// The card's read, as it stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Loaded {
    Loading,
    Read(Box<CardData>),
    Failed,
}

/// What the quick message box says under it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuickNote {
    Sent,
    Failed,
    /// It isn't known sent but may have arrived: Send sends it again.
    MayHaveArrived,
}

/// A quick message that didn't go, kept while its person's card shows the same text: Send sends its outbox row again.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Kept {
    row: i64,
    text: String,
    /// Its message may have arrived: the card says so before it is sent again.
    warn: bool,
}

/// How a quick message goes.
enum Quick {
    /// A new message to the 1:1 chat with `mri` (`chat`, else one made first).
    New {
        chat: Option<String>,
        mri: String,
        text: Box<MessageText>,
    },
    /// A failed one's outbox row, sent again.
    Again(i64),
}

/// How a quick message's sending ended.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Settled {
    Sent,
    /// Not sent; its row kept when it can be sent again.
    Failed(Option<Kept>),
}

/// What a card is made with.
#[derive(Clone)]
pub struct ProfileCardParts {
    pub core: Arc<dyn Core>,
    pub models: Models,
    pub trace: Arc<ViewTrace>,
    pub list: Entity<ChatList>,
}

/// A person's card (see the module docs).
pub struct ProfileCard {
    core: Arc<dyn Core>,
    models: Models,
    trace: Arc<ViewTrace>,
    list: Entity<ChatList>,
    form: Form,
    tab: CardTab,
    of: CardOf,
    /// The cards opened before this one in the dialog (the Organization tab's people), the last the one Back returns to.
    back: Vec<CardOf>,
    loaded: Loaded,
    /// The people its Organization tab shows, as the engine names and pictures them.
    faces: HashMap<String, Shown>,
    quick: Entity<InputState>,
    note: Option<QuickNote>,
    /// Whom a quick message is going out to (their sending is the app's: see the module docs).
    sending: HashSet<String>,
    /// The shown person's quick message that failed.
    failed: Option<Kept>,
    loading: Option<Task<()>>,
    note_shown: Option<Task<()>>,
    tabs: Handles,
    focus: FocusHandle,
    /// The Chat action's: the keyboard's first stop, as in Teams.
    chat_focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ProfileCardEvent> for ProfileCard {}

impl ProfileCard {
    /// The card of `of` in `form`, on `tab` (the full card's), read now.
    pub fn new(
        parts: ProfileCardParts,
        of: CardOf,
        form: Form,
        tab: CardTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let quick = cx.new(|cx| InputState::new(window, cx).placeholder(QUICK_HINT));
        let subscriptions = vec![
            cx.subscribe_in(
                &quick,
                window,
                |this: &mut Self, _, event: &InputEvent, window, cx| match event {
                    InputEvent::PressEnter { .. } => this.send_quick(window, cx),
                    // typed in (a press on the card is the card's own)
                    InputEvent::Change => cx.emit(ProfileCardEvent::Engaged),
                    InputEvent::Focus | InputEvent::Blur => {}
                },
            ),
            cx.observe(&parts.models.presence, |_, _, cx| cx.notify()),
            cx.observe(&parts.list, |_, _, cx| cx.notify()),
        ];
        let kind = if form == Form::Full { "full" } else { "compact" };
        parts.trace.record(&TraceLine::new("profile-card", "opened").kind(kind));
        let mut this = Self {
            core: parts.core,
            models: parts.models,
            trace: parts.trace,
            list: parts.list,
            form,
            tab,
            of,
            back: Vec::new(),
            loaded: Loaded::Loading,
            faces: HashMap::new(),
            quick,
            note: None,
            sending: HashSet::new(),
            failed: None,
            loading: None,
            note_shown: None,
            tabs: Handles::default(),
            focus: cx.focus_handle(),
            chat_focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        this.load(cx);
        this
    }

    /// Reads the card of the person shown (and their name and picture when the place gave none), and asks their
    /// presence; an answer for someone no longer shown is dropped.
    fn load(&mut self, cx: &mut Context<Self>) {
        self.loaded = Loaded::Loading;
        self.faces.clear();
        let core = Arc::clone(&self.core);
        let mri = self.of.mri.clone();
        crate::presence::watch(core.as_ref(), [&mri]);
        self.loading = Some(cx.spawn(async move |this: WeakEntity<Self>, cx| {
            if let Ok(person) = core.person(mri.clone()).await {
                let shown = Shown::of(person);
                let _ = this.update(cx, |this, cx| this.take_person(&mri, shown, cx));
            }
            let read = core.profile_card(mri.clone()).await;
            let faces = match &read {
                Ok(card) => faces_of(&core, card).await,
                Err(_) => HashMap::new(),
            };
            let _ = this.update(cx, |this, cx| {
                if this.of.mri != mri {
                    return;
                }
                this.loading = None;
                this.faces = faces;
                this.loaded = read.map_or(Loaded::Failed, |card| Loaded::Read(Box::new(card)));
                cx.notify();
            });
        }));
    }

    /// The engine's name and picture of `mri`, while they are shown: a place that gave none gets them.
    fn take_person(&mut self, mri: &str, shown: Shown, cx: &mut Context<Self>) {
        if self.of.mri != mri {
            return;
        }
        if self.of.name.is_none() {
            self.of.name = shown.name;
        }
        if self.of.picture.is_none() {
            self.of.picture = shown.picture;
        }
        cx.notify();
    }

    // --- what it shows ---

    /// Who it is of.
    #[must_use]
    pub fn of(&self) -> &CardOf {
        &self.of
    }

    #[must_use]
    pub fn form(&self) -> Form {
        self.form
    }

    #[must_use]
    pub fn tab(&self) -> CardTab {
        self.tab
    }

    /// The card's read.
    #[must_use]
    pub fn loaded(&self) -> &Loaded {
        &self.loaded
    }

    /// What the quick message box says under it.
    #[must_use]
    pub fn note(&self) -> Option<&QuickNote> {
        self.note.as_ref()
    }

    /// Whether a quick message to the person shown is going out.
    #[must_use]
    pub fn is_sending(&self) -> bool {
        self.sending.contains(&self.of.mri)
    }

    /// The quick message box's text.
    #[must_use]
    pub fn quick_text(&self, cx: &gpui::App) -> SharedString {
        self.quick.read(cx).value()
    }

    /// Whom Back returns to, the last first.
    #[must_use]
    pub fn back(&self) -> &[CardOf] {
        &self.back
    }

    /// The card's read, when it came.
    fn card(&self) -> Option<&CardData> {
        match &self.loaded {
            Loaded::Read(card) => Some(card),
            Loaded::Loading | Loaded::Failed => None,
        }
    }

    /// Their name as the card shows it: the place's, else the card's, else what the list calls someone unknown.
    #[must_use]
    pub fn name(&self) -> SharedString {
        self.of
            .name
            .clone()
            .or_else(|| self.card().and_then(|card| card.name.clone()).map(SharedString::from))
            .unwrap_or_else(|| SharedString::from(chat_list::unnamed(&self.of.mri)))
    }

    /// Whether it is the account's own card.
    #[must_use]
    pub fn own(&self) -> bool {
        self.of.mri == self.core.me()
    }

    /// Whether the card offers a quick message and calls: not on the account's own, nor in the read-only mode.
    #[must_use]
    pub fn writes(&self) -> bool {
        !self.own() && !self.core.read_only()
    }

    /// The chat the account has with them, as the list knows it.
    fn existing_chat(&self, cx: &gpui::App) -> Option<String> {
        chat_with(self.list.read(cx), self.core.me(), &self.of.mri)
    }

    /// Whether Chat goes anywhere: a chat there is, or a new 1:1 chat (none in the read-only mode, nor for the account
    /// without its notes).
    #[must_use]
    pub fn can_chat(&self, cx: &gpui::App) -> bool {
        self.existing_chat(cx).is_some() || self.writes()
    }

    /// The keyboard goes to the card: its Chat action, as Teams puts it there.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.chat_focus, cx);
    }

    // --- the actions ---

    /// Chat (see the module docs).
    pub fn chat(&mut self, cx: &mut Context<Self>) {
        if let Some(chat) = self.existing_chat(cx) {
            self.trace
                .record(&TraceLine::new("profile-card", "chat").conversation(&chat));
            cx.emit(ProfileCardEvent::OpenChat(chat));
        } else if self.writes() {
            self.trace.record(&TraceLine::new("profile-card", "chat").kind("new"));
            cx.emit(ProfileCardEvent::StartChat(self.recipient()));
        }
    }

    fn recipient(&self) -> Recipient {
        Recipient {
            mri: self.of.mri.clone(),
            name: self.name(),
            picture: self.of.picture.clone(),
        }
    }

    /// A call to them (`video`: with the camera on once it is up); nothing on the account's own card or in the
    /// read-only mode.
    pub fn call(&mut self, video: bool, cx: &mut Context<Self>) {
        if !self.writes() {
            return;
        }
        let Some(chat) = teams_proto::one_to_one_id(self.core.me(), &self.of.mri) else {
            return;
        };
        let kind = if video { "video" } else { "audio" };
        self.trace
            .record(&TraceLine::new("profile-card", "call").kind(kind).conversation(&chat));
        cx.emit(ProfileCardEvent::Call { chat, video });
    }

    /// The org chart: the Organization tab (the compact card asks for the full one on it).
    pub fn org_chart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_tab(CardTab::Organization, window, cx);
    }

    /// Shows `tab`: the full card's own, the compact card's full one asked for.
    pub fn show_tab(&mut self, tab: CardTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.form == Form::Compact {
            cx.emit(ProfileCardEvent::Expand(tab));
            return;
        }
        self.tab = tab;
        self.trace
            .record(&TraceLine::new("profile-card", "tab").kind(tab.key()));
        self.tabs.get(tab.key(), cx).focus(window, cx);
        cx.notify();
    }

    /// Left, Right, Up, Down, Home and End on a tab.
    fn tab_key(&mut self, tab: CardTab, event: &gpui::KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let at = CardTab::ALL.iter().position(|shown| *shown == tab).unwrap_or(0);
        if let Some(to) = crate::keyboard::step(event, CardTab::ALL.len(), at) {
            cx.stop_propagation();
            self.show_tab(CardTab::ALL[to], window, cx);
        }
    }

    /// Opens `person`'s card in its place (the Organization tab's people), this one kept for Back.
    pub fn open_person(&mut self, person: CardOf, window: &mut Window, cx: &mut Context<Self>) {
        if person.mri == self.of.mri {
            return;
        }
        let shown = std::mem::replace(&mut self.of, person);
        self.back.push(shown);
        self.person_changed(window, cx);
    }

    /// Back to the card before, on the same tab.
    pub fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(before) = self.back.pop() {
            self.of = before;
            self.person_changed(window, cx);
        }
    }

    /// Another person shown: their card read, and the quick message box emptied, so that nothing typed to one goes to
    /// the other.
    fn person_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.quick.update(cx, |quick, cx| quick.set_value("", window, cx));
        self.note = None;
        self.failed = None;
        self.load(cx);
        cx.notify();
    }

    /// Their email, opened in the mail app.
    pub fn email(address: &str, cx: &mut gpui::App) {
        cx.open_url(&format!("mailto:{address}"));
    }

    /// Escape: the card closes.
    pub fn escape(&mut self, cx: &mut Context<Self>) {
        cx.emit(ProfileCardEvent::Close);
    }

    // --- the quick message ---

    /// Puts `text` in the quick message box as if it were typed.
    pub fn set_quick(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.quick
            .update(cx, |quick, cx| quick.set_value(text.to_owned(), window, cx));
    }

    /// Sends the box's text to the 1:1 chat with them through the outbox (the chat made first when the account has
    /// none), then clears the box and says so; what went wrong otherwise, the text kept. The same text after a failure
    /// sends the failed row again, once the card said it may have arrived when it may have.
    pub fn send_quick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.quick.read(cx).value().trim().to_owned();
        if typed.is_empty() || !self.writes() || self.is_sending() {
            return;
        }
        let quick = match self.failed.clone().filter(|kept| kept.text == typed) {
            Some(kept) if kept.warn && self.note != Some(QuickNote::MayHaveArrived) => {
                self.note = Some(QuickNote::MayHaveArrived);
                cx.notify();
                return;
            }
            Some(kept) => Quick::Again(kept.row),
            None => {
                let Ok(text) = MessageText::plain(&typed) else {
                    self.note = Some(QuickNote::Failed);
                    cx.notify();
                    return;
                };
                Quick::New {
                    chat: self.existing_chat(cx),
                    mri: self.of.mri.clone(),
                    text: Box::new(text),
                }
            }
        };
        let (core, trace) = (Arc::clone(&self.core), Arc::clone(&self.trace));
        let executor = cx.background_executor().clone();
        let mri = self.of.mri.clone();
        self.sending.insert(mri.clone());
        self.note = None;
        self.failed = None;
        // the app's task, not the card's: a card closed (or showing someone else) meanwhile drops nothing
        cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
            let settled = send(&core, &executor, quick, &typed).await;
            if settled == Settled::Sent {
                trace.record(&TraceLine::new("profile-card", "quick-sent"));
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.quick_settled(&mri, &typed, settled, window, cx);
            });
        })
        .detach();
        cx.notify();
    }

    /// The quick message `typed` to `mri` settled: told only while the card still shows them, the box cleared only
    /// while it still holds what went.
    fn quick_settled(&mut self, mri: &str, typed: &str, settled: Settled, window: &mut Window, cx: &mut Context<Self>) {
        self.sending.remove(mri);
        if self.of.mri == mri {
            match settled {
                Settled::Sent => {
                    if self.quick.read(cx).value().trim() == typed {
                        self.quick.update(cx, |quick, cx| quick.set_value("", window, cx));
                    }
                    self.note = Some(QuickNote::Sent);
                    self.note_shown = Some(cx.spawn(async move |this: WeakEntity<Self>, cx| {
                        cx.background_executor().timer(NOTE_SHOWN).await;
                        let _ = this.update(cx, Self::sent_note_shown);
                    }));
                }
                Settled::Failed(kept) => {
                    let warn = kept.as_ref().is_some_and(|kept| kept.warn);
                    self.note = Some(if warn {
                        QuickNote::MayHaveArrived
                    } else {
                        QuickNote::Failed
                    });
                    self.failed = kept;
                }
            }
        }
        cx.notify();
    }
}

impl ProfileCard {
    /// "Quick message sent" was shown long enough.
    fn sent_note_shown(&mut self, cx: &mut Context<Self>) {
        if self.note == Some(QuickNote::Sent) {
            self.note = None;
            cx.notify();
        }
    }
}

/// The names and pictures of everyone `card`'s Organization tab shows, as the engine gives them; their presence asked.
async fn faces_of(core: &Arc<dyn Core>, card: &CardData) -> HashMap<String, Shown> {
    let mut faces = HashMap::new();
    for person in people_of(card) {
        if let Ok(found) = core.person(person.clone()).await {
            faces.insert(person, Shown::of(found));
        }
    }
    crate::presence::watch(core.as_ref(), faces.keys());
    faces
}

/// Everyone the card's Organization tab shows: the managers, the reports and the people they work with.
fn people_of(card: &CardData) -> Vec<String> {
    let mut people: Vec<String> = Vec::new();
    let organization = card
        .organization
        .iter()
        .flat_map(|org| org.managers.iter().chain(&org.directs));
    for person in organization.chain(card.working_with.iter().flatten()) {
        if !people.contains(&person.mri) {
            people.push(person.mri.clone());
        }
    }
    people
}

/// Sends a quick message (`typed`, as the box held it): a new one to `chat`, or to a new 1:1 chat made first
/// ([`Action::CreateChat`], as New message's first message makes one), or a failed one's row again; each write watched
/// until the outbox settles it.
async fn send(core: &Arc<dyn Core>, executor: &BackgroundExecutor, quick: Quick, typed: &str) -> Settled {
    let reply = match quick {
        Quick::New { chat, mri, text } => {
            let chat = match chat {
                Some(chat) => Some(chat),
                None => made_chat(core, &mri, executor).await,
            };
            // a 1:1 chat that couldn't be made is made anew by the next try: Teams' 1:1 chat with someone is one thread
            let Some(chat) = chat else {
                return Settled::Failed(None);
            };
            core.write(Action::Send {
                conversation: chat,
                text: *text,
            })
        }
        Quick::Again(row) => core.resend(row),
    };
    settled(settled_row_unowned(core.as_ref(), reply, executor).await, typed)
}

/// What a quick message's settled row (none: refused) comes to: a failed row that may be sent again is kept.
fn settled(row: Option<OutboxEntry>, typed: &str) -> Settled {
    let Some(row) = row else {
        return Settled::Failed(None);
    };
    if row.state == OutboxState::Sent {
        return Settled::Sent;
    }
    let kept = match Again::of(&row, false) {
        Again::No => None,
        again => Some(Kept {
            row: row.id,
            text: typed.to_owned(),
            warn: again == Again::AfterWarning,
        }),
    };
    Settled::Failed(kept)
}

/// A new 1:1 chat with `mri`, made through the outbox; its id once the outbox made it.
async fn made_chat(core: &Arc<dyn Core>, mri: &str, executor: &BackgroundExecutor) -> Option<String> {
    let made = core.write(Action::CreateChat {
        others: vec![Mri::new(mri)],
        topic: None,
    });
    let row = settled_row_unowned(core.as_ref(), made, executor).await?;
    row.message_id.filter(|_| row.state == OutboxState::Sent)
}

impl Focusable for ProfileCard {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}
