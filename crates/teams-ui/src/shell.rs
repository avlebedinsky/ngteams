//! The shell: the window's three panes, Teams-like and compact. On the left the rail (Chat, Teams, and Settings at
//! its foot), then the list pane (the [`ChatList`] for Chat; the [`TeamsList`] of teams and their channels for Teams,
//! M2 Task 16), resizable by the handle on its right edge, then the conversation pane (the chat opened in a
//! [`ChatView`] under its name, and a 1:1 chat's other member's presence, M2 Task 17; a channel opened in a
//! [`ChannelView`] under its team's and its own name, read-only; or the settings).
//!
//! The account menu (M2 Task 20): the rail's status button, above Settings, shows what the account's own presence
//! reports ([`OwnPresence`], handed in by the desktop) and opens a menu of the statuses the user may set (Automatic and
//! the captured manual ones, the same setting the settings view changes) with the last failed report; the shell's
//! mouse and key input tells the own presence the window is used.
//!
//! The keyboard: Ctrl+K opens the [`Switcher`] over the panes (and closes it again); a chat chosen there or clicked in
//! the list opens in the conversation pane and is kept as the selected chat in the settings, so the app opens it again
//! at its next start. Ctrl+Shift+F opens the [`Search`] of the stored messages the same way (one of the two at a
//! time); a message found there or in the switcher opens its chat at the message ([`ChatView::reveal`]). What the
//! shell shows is traced (`chat opened`, `channel opened`, `settings opened`, `switcher opened`/`closed`, `search
//! opened`/`closed`, `status-menu opened`/`closed`/`chosen` with the status's key).

use std::sync::Arc;

use gpui::{
    AnyElement, AnyView, App, AppContext as _, Context, Entity, FocusHandle, Global, InteractiveElement as _,
    IntoElement, KeyBinding, KeyDownEvent, MouseDownEvent, MouseMoveEvent, ParentElement as _, Render,
    ScrollWheelEvent, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, actions, div,
    prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ResizableState, h_resizable, resizable_panel};
use teams_core::{Availability, Reporting};
use teams_proto::ManualAvailability;

use crate::bridge::Core;
use crate::channel_view::ChannelView;
use crate::chat_list::{ChatList, ChatListEvent};
use crate::chat_view::{self, ChatView};
use crate::models::Models;
use crate::own_presence::{self, OwnPresence};
use crate::presence;
use crate::search::{HitTarget, Search, SearchEvent};
use crate::settings::{ManualStatus, SettingsModel};
use crate::settings_view::SettingsView;
use crate::switcher::{Switcher, SwitcherEvent};
use crate::teams_list::{ChannelChosen, TeamsList, TeamsListEvent};
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};

actions!(teams_ui, [ToggleSwitcher, ToggleSearch]);

/// The shell's key context.
pub const CONTEXT: &str = "Shell";
/// The list pane's width when the window opens, px.
pub const LIST_WIDTH: f32 = 320.0;
/// The narrowest the list pane gets, px.
pub const LIST_MIN: f32 = 220.0;
/// The widest the list pane gets, px.
pub const LIST_MAX: f32 = 560.0;
/// The rail's width, px.
pub const RAIL_WIDTH: f32 = 76.0;

/// Marks the shell's key bindings as installed.
struct Bound;

impl Global for Bound {}

/// Binds the shell's keys (Ctrl+K: the switcher; Ctrl+Shift+F: the search) and the chat view's, once per app.
pub fn init(cx: &mut App) {
    chat_view::init(cx);
    if cx.has_global::<Bound>() {
        return;
    }
    cx.set_global(Bound);
    cx.bind_keys([
        KeyBinding::new("ctrl-k", ToggleSwitcher, None),
        KeyBinding::new("ctrl-shift-f", ToggleSearch, None),
    ]);
}

/// What the rail has chosen for the list pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RailItem {
    Chats,
    Teams,
}

/// What the conversation pane shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Main {
    Nothing,
    Chat(String),
    /// A team's channel, read-only.
    Channel(String),
    Settings,
}

/// What the shell is made of: the engine's request side, the models, the settings and the view trace.
#[derive(Clone)]
pub struct ShellParts {
    pub core: Arc<dyn Core>,
    pub models: Models,
    pub settings: Entity<SettingsModel>,
    pub trace: Arc<ViewTrace>,
}

/// The shell (see the module docs).
pub struct Shell {
    core: Arc<dyn Core>,
    models: Models,
    settings: Entity<SettingsModel>,
    trace: Arc<ViewTrace>,
    list: Entity<ChatList>,
    teams: Entity<TeamsList>,
    /// The chat opened in the conversation pane.
    chat: Option<Entity<ChatView>>,
    /// The channel opened in the conversation pane, with the names its header shows.
    channel: Option<(Entity<ChannelView>, ChannelChosen)>,
    settings_view: Entity<SettingsView>,
    switcher: Option<(Entity<Switcher>, Subscription)>,
    search: Option<(Entity<Search>, Subscription)>,
    rail: RailItem,
    main: Main,
    panes: Entity<ResizableState>,
    focus: FocusHandle,
    /// The account's own presence, once the desktop handed it in, with the shell's observation of it.
    own_presence: Option<(Entity<OwnPresence>, Subscription)>,
    /// The account menu is open.
    status_menu: bool,
    _subscriptions: [Subscription; 4],
}

impl Shell {
    /// The shell over `parts`, focused; the chat the settings kept is opened again.
    pub fn new(parts: ShellParts, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let ShellParts {
            core,
            models,
            settings,
            trace,
        } = parts;
        let selected = settings.read(cx).settings().selected_chat.clone();
        let list_trace = Arc::clone(&trace);
        let (list_core, list_models) = (Arc::clone(&core), models.clone());
        let list = cx.new(|cx| ChatList::new(list_core, list_models, list_trace, selected.clone(), cx));
        let opened = cx.subscribe_in(&list, window, |this: &mut Self, _, event, window, cx| match event {
            ChatListEvent::Opened(id) => this.open_chat(id.clone(), window, cx),
        });
        let (teams_core, teams_models, teams_trace) = (Arc::clone(&core), models.clone(), Arc::clone(&trace));
        let teams = cx.new(|cx| TeamsList::new(teams_core, teams_models, teams_trace, cx));
        let channel_opened = cx.subscribe_in(&teams, window, |this: &mut Self, _, event, window, cx| match event {
            TeamsListEvent::Opened(chosen) => this.open_channel(chosen.clone(), window, cx),
        });
        // a channel kept as the selected chat (by a build that opened one in a chat view) opens as a channel once the
        // list knows it
        let kept_channel = cx.observe_in(&teams, window, |this: &mut Self, teams, window, cx| {
            let Main::Chat(id) = &this.main else { return };
            if let Some(chosen) = teams.read(cx).chosen(id) {
                this.open_channel(chosen, window, cx);
            }
        });
        let settings_view = cx.new(|cx| SettingsView::new(settings.clone(), cx));
        // a presence the engine told: the chat's header draws its dot again
        let presence = cx.observe(&models.presence, |_: &mut Self, _, cx| cx.notify());
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        if let Some(id) = &selected {
            trace.record(&TraceLine::new("chat", "opened").conversation(id));
        }
        let chat_parts = ShellParts {
            core: Arc::clone(&core),
            models: models.clone(),
            settings: settings.clone(),
            trace: Arc::clone(&trace),
        };
        let chat = selected
            .clone()
            .map(|id| cx.new(|cx| ChatView::new(chat_parts, id, window, cx)));
        if let Some(chat) = &chat {
            let compose = chat.read(cx).compose().clone();
            compose.update(cx, |compose, cx| compose.focus(window, cx));
        }
        Self {
            core,
            models,
            settings,
            trace,
            list,
            teams,
            chat,
            channel: None,
            settings_view,
            switcher: None,
            search: None,
            rail: RailItem::Chats,
            main: selected.map_or(Main::Nothing, Main::Chat),
            panes: cx.new(|_| ResizableState::default()),
            focus,
            own_presence: None,
            status_menu: false,
            _subscriptions: [opened, channel_opened, kept_channel, presence],
        }
    }

    /// Shows the account's own presence in the account menu, and tells it of the window's use from now on.
    pub fn set_own_presence(&mut self, own_presence: Entity<OwnPresence>, cx: &mut Context<Self>) {
        let observing = cx.observe(&own_presence, |_, _, cx| cx.notify());
        self.own_presence = Some((own_presence, observing));
        cx.notify();
    }

    /// The account's own presence, once the desktop handed it in.
    #[must_use]
    pub fn own_presence(&self) -> Option<&Entity<OwnPresence>> {
        self.own_presence.as_ref().map(|(own, _)| own)
    }

    /// Whether the account menu is open.
    #[must_use]
    pub fn status_menu_open(&self) -> bool {
        self.status_menu
    }

    /// Opens the account menu, or closes it when it is open.
    pub fn toggle_status_menu(&mut self, cx: &mut Context<Self>) {
        self.status_menu = !self.status_menu;
        let change = if self.status_menu { "opened" } else { "closed" };
        self.trace.record(&TraceLine::new("status-menu", change));
        cx.notify();
    }

    /// The user chose `status` in the account menu: kept in the settings (the own presence sends it), the menu closed.
    pub fn choose_status(&mut self, status: ManualStatus, cx: &mut Context<Self>) {
        self.trace
            .record(&TraceLine::new("status-menu", "chosen").kind(status.key()));
        self.settings.update(cx, |settings, cx| {
            settings.change(|settings| settings.manual_status = status, cx);
        });
        self.status_menu = false;
        cx.notify();
    }

    /// The window was used: the own presence is told (Available again at once after idle).
    fn used(&mut self, cx: &mut Context<Self>) {
        if let Some((own, _)) = &self.own_presence {
            own.update(cx, OwnPresence::active);
        }
    }

    /// The Teams list.
    #[must_use]
    pub fn teams(&self) -> &Entity<TeamsList> {
        &self.teams
    }

    /// The channel view of the channel opened, if one is.
    #[must_use]
    pub fn channel(&self) -> Option<&Entity<ChannelView>> {
        self.channel.as_ref().map(|(view, _)| view)
    }

    /// Opens team channel `id` read-only ([`Shell::open_channel`]), named as the Teams list names it, or as
    /// "Channel" when the list doesn't show it (the teams not read yet, or unreadable).
    pub fn open_channel_id(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = self.teams.read(cx).chosen(&id).unwrap_or(ChannelChosen {
            id,
            name: SharedString::default(),
            team: SharedString::default(),
        });
        self.open_channel(chosen, window, cx);
    }

    /// Opens a team's channel in the conversation pane, read-only; the chat it hides is kept (its draft stays) but
    /// isn't shown, so no read receipt goes for it. A channel is never kept as the selected chat. The switcher and the
    /// search close.
    pub fn open_channel(&mut self, chosen: ChannelChosen, window: &mut Window, cx: &mut Context<Self>) {
        self.trace
            .record(&TraceLine::new("channel", "opened").conversation(&chosen.id));
        let shown = self
            .channel
            .as_ref()
            .is_some_and(|(view, _)| view.read(cx).channel() == chosen.id);
        if shown {
            // opened again: synced again (an earlier sync may have failed, and no catch-up reads a channel)
            if let Some((view, kept)) = &mut self.channel {
                if !chosen.team.is_empty() {
                    *kept = chosen.clone();
                }
                view.update(cx, ChannelView::sync);
            }
        } else {
            let (core, models, trace) = (Arc::clone(&self.core), self.models.clone(), Arc::clone(&self.trace));
            let id = chosen.id.clone();
            let view = cx.new(|cx| ChannelView::new(core, models, trace, id, cx));
            self.channel = Some((view, chosen.clone()));
        }
        self.teams
            .update(cx, |teams, cx| teams.select(Some(chosen.id.clone()), cx));
        // a chat view of the channel itself (kept as the selected chat by an earlier build) is dropped, and the channel
        // isn't the selected chat any more
        if self
            .chat
            .as_ref()
            .is_some_and(|chat| chat.read(cx).conversation() == chosen.id)
        {
            self.chat = None;
        }
        if self.settings.read(cx).settings().selected_chat.as_deref() == Some(chosen.id.as_str()) {
            self.settings.update(cx, |settings, cx| {
                settings.change(|settings| settings.selected_chat = None, cx);
            });
        }
        if let Some(chat) = &self.chat {
            chat.update(cx, |chat, cx| chat.set_shown(false, cx));
        }
        self.rail = RailItem::Teams;
        self.main = Main::Channel(chosen.id);
        self.close_switcher(window, cx);
        self.close_search(window, cx);
        self.focus.focus(window, cx);
        cx.notify();
    }

    #[must_use]
    pub fn list(&self) -> &Entity<ChatList> {
        &self.list
    }

    /// The settings the shell keeps the selected chat in.
    #[must_use]
    pub fn settings(&self) -> &Entity<SettingsModel> {
        &self.settings
    }

    #[must_use]
    pub fn main(&self) -> &Main {
        &self.main
    }

    #[must_use]
    pub fn rail(&self) -> RailItem {
        self.rail
    }

    /// The panes' sizes (the list pane's first).
    #[must_use]
    pub fn panes(&self) -> &Entity<ResizableState> {
        &self.panes
    }

    /// The switcher, while it is open.
    #[must_use]
    pub fn switcher(&self) -> Option<&Entity<Switcher>> {
        self.switcher.as_ref().map(|(switcher, _)| switcher)
    }

    /// The search, while it is open.
    #[must_use]
    pub fn search(&self) -> Option<&Entity<Search>> {
        self.search.as_ref().map(|(search, _)| search)
    }

    /// The chat view of the chat opened, if one is.
    #[must_use]
    pub fn chat(&self) -> Option<&Entity<ChatView>> {
        self.chat.as_ref()
    }

    /// Opens a chat in the conversation pane and keeps it as the selected chat; the switcher closes. A team's channel
    /// the Teams list shows opens read-only instead ([`Shell::open_channel`]), never in a chat view.
    pub fn open_chat(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(chosen) = self.teams.read(cx).chosen(&id) {
            self.open_channel(chosen, window, cx);
            return;
        }
        self.trace.record(&TraceLine::new("chat", "opened").conversation(&id));
        let shown = self.chat.as_ref().map(|chat| chat.read(cx).conversation() == id);
        if shown != Some(true) {
            let parts = self.parts();
            let conversation = id.clone();
            self.chat = Some(cx.new(|cx| ChatView::new(parts, conversation, window, cx)));
        }
        self.list.update(cx, |list, cx| list.select(Some(id.clone()), cx));
        let kept = id.clone();
        self.settings.update(cx, |settings, cx| {
            settings.change(|settings| settings.selected_chat = Some(kept), cx);
        });
        self.rail = RailItem::Chats;
        self.main = Main::Chat(id);
        self.close_switcher(window, cx);
        self.close_search(window, cx);
        // the chat opened takes what is typed next, and is shown (read receipts may go)
        if let Some(chat) = &self.chat {
            chat.update(cx, |chat, cx| chat.set_shown(true, cx));
            let compose = chat.read(cx).compose().clone();
            compose.update(cx, |compose, cx| compose.focus(window, cx));
        }
        cx.notify();
    }

    /// Opens a message's chat ([`Shell::open_chat`]) at the message: the chat view reads back to it, shows it and
    /// selects it ([`ChatView::reveal`]).
    pub fn open_message(&mut self, target: HitTarget, window: &mut Window, cx: &mut Context<Self>) {
        self.open_chat(target.conversation.clone(), window, cx);
        if self.main != Main::Chat(target.conversation.clone()) {
            return;
        }
        if let Some(chat) = &self.chat {
            chat.update(cx, |chat, cx| chat.reveal(target.arrival_time, target.message, cx));
        }
    }

    /// The presence the chat's header shows: the chat opened's other member's, when it is a 1:1 chat (the list's row
    /// for it names them), as the presence model holds it.
    #[must_use]
    pub fn header_presence(&self, cx: &App) -> Option<Availability> {
        let Main::Chat(id) = &self.main else {
            return None;
        };
        let list = self.list.read(cx);
        let row = list.row(id)?;
        list.presence_of(row, cx)
    }

    /// What a chat view is made of: the shell's own.
    fn parts(&self) -> ShellParts {
        ShellParts {
            core: Arc::clone(&self.core),
            models: self.models.clone(),
            settings: self.settings.clone(),
            trace: Arc::clone(&self.trace),
        }
    }

    /// Shows the settings in the conversation pane; the chat they hide is kept (its draft stays), but isn't shown, so
    /// no read receipt goes for it.
    pub fn show_settings(&mut self, cx: &mut Context<Self>) {
        if self.main != Main::Settings {
            self.trace.record(&TraceLine::new("settings", "opened"));
            self.main = Main::Settings;
            if let Some(chat) = &self.chat {
                chat.update(cx, |chat, cx| chat.set_shown(false, cx));
            }
            cx.notify();
        }
    }

    /// Shows what the rail's `item` lists in the list pane; the Teams list asks for the teams again (the engine reads
    /// them when they are old).
    pub fn show_rail(&mut self, item: RailItem, cx: &mut Context<Self>) {
        if self.rail != item {
            self.rail = item;
            if item == RailItem::Teams {
                self.teams.update(cx, TeamsList::reload);
            }
            self.trace.record(&TraceLine::new(
                "rail",
                match item {
                    RailItem::Chats => "chats",
                    RailItem::Teams => "teams",
                },
            ));
            cx.notify();
        }
    }

    /// Opens the switcher, or closes it when it is open.
    pub fn toggle_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.switcher.is_some() {
            self.close_switcher(window, cx);
            return;
        }
        self.close_search(window, cx);
        let list = self.list.clone();
        let core = Arc::clone(&self.core);
        let switcher = cx.new(|cx| Switcher::new(list, core, window, cx));
        let events = cx.subscribe_in(&switcher, window, |this: &mut Self, _, event, window, cx| match event {
            SwitcherEvent::Open(id) => this.open_chat(id.clone(), window, cx),
            SwitcherEvent::OpenMessage(target) => this.open_message(target.clone(), window, cx),
            SwitcherEvent::Dismissed => this.close_switcher(window, cx),
            SwitcherEvent::CopyDiagnostics => this.copy_diagnostics(window, cx),
        });
        switcher.update(cx, |switcher, cx| switcher.focus(window, cx));
        self.switcher = Some((switcher, events));
        self.trace.record(&TraceLine::new("switcher", "opened"));
        cx.notify();
    }

    /// The palette's "Copy diagnostics": the text on the clipboard, the switcher closed, and the settings shown, where
    /// what was copied is told (said plainly when it holds full requests and responses).
    fn copy_diagnostics(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.update(cx, |settings, cx| {
            settings.copy_diagnostics(cx);
        });
        self.trace.record(&TraceLine::new("diagnostics", "copied"));
        self.close_switcher(window, cx);
        self.show_settings(cx);
    }

    fn close_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.switcher.take().is_some() {
            self.trace.record(&TraceLine::new("switcher", "closed"));
            self.focus.focus(window, cx);
            cx.notify();
        }
    }

    /// Opens the search of the stored messages (the switcher closes), or closes it when it is open.
    pub fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.is_some() {
            self.close_search(window, cx);
            return;
        }
        self.close_switcher(window, cx);
        let (core, list, trace) = (Arc::clone(&self.core), self.list.clone(), Arc::clone(&self.trace));
        let search = cx.new(|cx| Search::new(core, list, trace, window, cx));
        let events = cx.subscribe_in(&search, window, |this: &mut Self, _, event, window, cx| match event {
            SearchEvent::Open(target) => this.open_message(target.clone(), window, cx),
            SearchEvent::Dismissed => this.close_search(window, cx),
        });
        search.update(cx, |search, cx| search.focus(window, cx));
        self.search = Some((search, events));
        self.trace.record(&TraceLine::new("search", "opened"));
        cx.notify();
    }

    fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.take().is_some() {
            self.trace.record(&TraceLine::new("search", "closed"));
            self.focus.focus(window, cx);
            cx.notify();
        }
    }

    fn rail_button(
        id: &'static str,
        label: &'static str,
        chosen: bool,
        cx: &mut Context<Self>,
        on: fn(&mut Self, &mut Context<Self>),
    ) -> AnyElement {
        let button = Button::new(id)
            .label(label)
            .compact()
            .on_click(cx.listener(move |this, _, _, cx| on(this, cx)));
        let button = if chosen { button.primary() } else { button.ghost() };
        div()
            .debug_selector(move || id.to_owned())
            .w_full()
            .px_1()
            .child(button.w_full())
            .into_any_element()
    }

    fn rail_view(&self, palette: Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let chats = self.rail == RailItem::Chats && self.main != Main::Settings;
        let teams = self.rail == RailItem::Teams && self.main != Main::Settings;
        div()
            .debug_selector(|| "rail".to_owned())
            .w(px(RAIL_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .gap_1()
            .py_2()
            .bg(rgb(palette.rail))
            .border_r_1()
            .border_color(rgb(palette.border))
            .child(Self::rail_button("rail-chats", "Chat", chats, cx, |this, cx| {
                this.show_rail(RailItem::Chats, cx);
            }))
            .child(Self::rail_button("rail-teams", "Teams", teams, cx, |this, cx| {
                this.show_rail(RailItem::Teams, cx);
            }))
            .child(div().flex_1())
            .child(self.status_button(palette, cx))
            .child(Self::rail_button(
                "rail-settings",
                "Settings",
                self.main == Main::Settings,
                cx,
                Self::show_settings,
            ))
    }

    /// What the account's own presence reports now, as the account menu shows it; `None` before the desktop handed it
    /// in.
    #[must_use]
    pub fn own_reporting(&self, cx: &App) -> Option<Reporting> {
        self.own_presence().map(|own| own.read(cx).reporting(cx))
    }

    /// The rail's status button: a dot for what the own presence reports, opening the account menu.
    fn status_button(&self, palette: Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let reporting = self.own_reporting(cx);
        let failed = self.own_presence().is_some_and(|own| own.read(cx).failure().is_some());
        // the account's own dot, with a selector of its own: the `presence-…` ones are others' (the views' dots)
        let dot = reporting.map(|reporting| {
            let shown = shown_as(reporting);
            let dot = div()
                .debug_selector(|| "rail-status-dot".to_owned())
                .size(px(presence::DOT))
                .flex_none()
                .rounded_full()
                .border_1()
                .border_color(rgb(palette.background));
            match presence::fill(&shown) {
                Some(color) => dot.bg(rgb(color)),
                None => dot
                    .border_color(rgb(palette.muted_foreground))
                    .bg(rgb(palette.background)),
            }
        });
        div()
            .debug_selector(|| "rail-status".to_owned())
            .id("rail-status")
            .mx_1()
            .px_1()
            .py_1()
            .flex()
            .items_center()
            .justify_center()
            .gap_1()
            .rounded_md()
            .cursor_pointer()
            .text_xs()
            .when(self.status_menu, |button| button.bg(rgb(palette.border)))
            .children(dot)
            .child(if failed { "Status !" } else { "Status" })
            .on_click(cx.listener(|this, _, _, cx| this.toggle_status_menu(cx)))
    }

    /// The account menu: the statuses the user may set (the one set marked), what is reported, and the last failed
    /// report.
    fn status_menu_view(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let chosen = self.settings.read(cx).settings().manual_status;
        let entries = ManualStatus::ALL.map(|status| {
            let id: SharedString = format!("status-menu-{}", status.key()).into();
            let selector = id.to_string();
            let button = Button::new(id)
                .label(status.label())
                .compact()
                .on_click(cx.listener(move |this, _, _, cx| this.choose_status(status, cx)));
            let button = if status == chosen {
                button.primary()
            } else {
                button.ghost()
            };
            div()
                .debug_selector(move || selector.clone())
                .w_full()
                .child(button.w_full())
        });
        let reported = self.own_reporting(cx).map(|reporting| {
            div()
                .debug_selector(|| "status-menu-reported".to_owned())
                .text_xs()
                .text_color(rgb(palette.muted_foreground))
                .child(SharedString::from(format!(
                    "Reported: {}",
                    own_presence::reporting_label(reporting)
                )))
        });
        let failure = self
            .own_presence()
            .and_then(|own| own.read(cx).failure().cloned())
            .map(|failure| {
                div()
                    .debug_selector(|| "status-menu-failure".to_owned())
                    .text_xs()
                    .text_color(rgb(palette.muted_foreground))
                    .child(SharedString::from(format!(
                        "Not reported ({}): {}",
                        own_presence::kind(failure.status),
                        failure.why
                    )))
            });
        div()
            .debug_selector(|| "status-menu".to_owned())
            .absolute()
            .left(px(RAIL_WIDTH + 4.0))
            .bottom(px(48.0))
            .w(px(220.0))
            .p_2()
            .flex()
            .flex_col()
            .gap_1()
            .bg(rgb(palette.surface))
            .border_1()
            .border_color(rgb(palette.border))
            .rounded_md()
            .shadow_md()
            .children(entries)
            .children(reported)
            .children(failure)
            .into_any_element()
    }

    fn list_pane(&self, palette: Palette) -> AnyElement {
        let pane = div().debug_selector(|| "list-pane".to_owned()).size_full();
        match self.rail {
            RailItem::Chats => pane.child(self.list.clone()).into_any_element(),
            RailItem::Teams => pane
                .flex()
                .flex_col()
                .bg(rgb(palette.surface))
                .child(pane_header("Teams", palette))
                .child(div().flex_1().min_h_0().child(self.teams.clone()))
                .into_any_element(),
        }
    }

    fn main_pane(&self, palette: Palette, cx: &App) -> AnyElement {
        let pane = div()
            .debug_selector(|| "conversation-pane".to_owned())
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(palette.background));
        match &self.main {
            Main::Settings => pane.child(self.settings_view.clone()).into_any_element(),
            Main::Chat(id) => {
                let name = self
                    .list
                    .read(cx)
                    .row(id)
                    .map_or_else(|| SharedString::from("Chat"), |row| row.name.clone());
                let chat = self
                    .chat
                    .clone()
                    .filter(|chat| chat.read(cx).conversation() == id.as_str());
                let presence = self.header_presence(cx).map(|availability| {
                    presence::labelled(&availability, palette).debug_selector(|| "chat-header-presence".to_owned())
                });
                let header = pane_header(name, palette)
                    .gap_2()
                    .children(presence)
                    .debug_selector(|| "chat-header".to_owned());
                pane.child(header)
                    .child(div().flex_1().min_h_0().children(chat))
                    .into_any_element()
            }
            Main::Channel(id) => {
                let shown = self
                    .channel
                    .as_ref()
                    .filter(|(view, _)| view.read(cx).channel() == id.as_str());
                let title = shown.map_or_else(
                    || SharedString::from("Channel"),
                    |(_, chosen)| {
                        if chosen.team.is_empty() {
                            SharedString::from("Channel")
                        } else {
                            format!("{} › {}", chosen.team, chosen.name).into()
                        }
                    },
                );
                pane.child(pane_header(title, palette).debug_selector(|| "channel-header".to_owned()))
                    .child(div().flex_1().min_h_0().children(shown.map(|(view, _)| view.clone())))
                    .into_any_element()
            }
            Main::Nothing => pane
                .items_center()
                .justify_center()
                .text_color(rgb(palette.muted_foreground))
                .child("Choose a chat, or press Ctrl+K")
                .into_any_element(),
        }
    }
}

/// A pane's header line.
fn pane_header(title: impl Into<SharedString>, palette: Palette) -> gpui::Div {
    div()
        .h(px(48.0))
        .flex_none()
        .px_3()
        .flex()
        .items_center()
        .border_b_1()
        .border_color(rgb(palette.border))
        .font_weight(gpui::FontWeight::BOLD)
        .child(title.into())
}

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let panes = h_resizable("panes")
            .with_state(&self.panes)
            .child(
                resizable_panel()
                    .size(px(LIST_WIDTH))
                    .size_range(px(LIST_MIN)..px(LIST_MAX))
                    .child(self.list_pane(palette)),
            )
            .child(resizable_panel().child(self.main_pane(palette, cx)));
        let overlay = |view: AnyView| {
            div()
                .absolute()
                .top(px(56.0))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(view)
        };
        let switcher = self.switcher().map(|switcher| overlay(switcher.clone().into()));
        let search = self.search().map(|search| overlay(search.clone().into()));
        let status_menu = self.status_menu.then(|| self.status_menu_view(palette, cx));
        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            // the window's use, for the account's own presence
            .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, _, cx| this.used(cx)))
            .capture_any_mouse_down(cx.listener(|this, _: &MouseDownEvent, _, cx| this.used(cx)))
            .capture_key_down(cx.listener(|this, _: &KeyDownEvent, _, cx| this.used(cx)))
            .on_scroll_wheel(cx.listener(|this, _: &ScrollWheelEvent, _, cx| this.used(cx)))
            .on_action(cx.listener(|this, _: &ToggleSwitcher, window, cx| this.toggle_switcher(window, cx)))
            .on_action(cx.listener(|this, _: &ToggleSearch, window, cx| this.toggle_search(window, cx)))
            .relative()
            .size_full()
            .flex()
            .child(self.rail_view(palette, cx))
            .child(div().flex_1().h_full().min_w_0().child(panes))
            .children(switcher)
            .children(search)
            .children(status_menu)
    }
}

/// The dot the account menu shows for what is reported: a manual status's own, Available's green, grey while nothing
/// is reported.
fn shown_as(reporting: Reporting) -> Availability {
    match reporting {
        Reporting::Available | Reporting::Manual(ManualAvailability::Available) => Availability::Available,
        Reporting::Manual(ManualAvailability::Busy) => Availability::Busy,
        Reporting::Manual(ManualAvailability::DoNotDisturb) => Availability::DoNotDisturb,
        Reporting::Manual(ManualAvailability::Away) => Availability::Away,
        Reporting::Inactive | Reporting::Halted | Reporting::Quit => Availability::Offline,
        Reporting::Connecting => Availability::Unknown,
    }
}
