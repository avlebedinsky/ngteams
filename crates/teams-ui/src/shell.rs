//! The shell: the window's three panes, Teams-like and compact. On the left the rail (Chat, and Settings at its foot),
//! then the list pane (the [`Sidebar`]: the [`ChatList`]'s chats and the [`TeamsList`]'s teams with their channels,
//! M2 Task 16, in sections of one list), resizable by the handle on its right edge, then the conversation pane (the
//! chat opened in a [`ChatView`] under its name, and a 1:1 chat's other member's presence, M2 Task 17; a channel
//! opened in a [`ChannelView`] under its team's and its own name, read-only; or the settings, which the rail's Chat
//! leaves for what they hid).
//!
//! The account menu (M2 Task 20): the rail's status button, above Settings, shows what the account's own presence
//! reports ([`OwnPresence`], handed in by the desktop) and opens a menu of the statuses the user may set (Automatic and
//! the captured manual ones, the same setting the settings view changes) with the last failed report; a status chosen
//! there goes to the own presence too, so choosing again one whose report failed sends it again. The shell's mouse
//! input and every keystroke in its window (the ones a key binding takes included) tell the own presence the window is
//! used.
//!
//! The keyboard: Ctrl+Alt+Down and Ctrl+Alt+Up show the app's next and previous account ([`crate::accounts`]); Ctrl+K
//! opens the [`Switcher`] over the panes (and closes it again); a chat chosen there or clicked in
//! the list opens in the conversation pane and is kept as the selected chat in the settings, so the app opens it again
//! at its next start; a channel chosen there opens read-only, and a "Set status: …" command chosen there sets the
//! account's status as the account menu does. Ctrl+Shift+F opens the [`Search`] of the stored messages the same way (one of the two at a
//! time); a message found there or in the switcher opens its chat at the message ([`ChatView::reveal`]). What the
//! shell shows is traced (`chat opened`, `channel opened`, `settings opened`, `switcher opened`/`closed`, `search
//! opened`/`closed`, `status-menu opened`/`closed`/`chosen` with the status's key, and `header shown`: the chat's
//! header's presence and whether its name and avatar came from people's names and a picture, never the name).
//!
//! Calls (M3 Task 17), in a build that shows them (in one that doesn't, the call keys do nothing here): the chat's
//! header has Call (or Join, when an invitation named a running call in that group chat) while the calling endpoint is
//! registered and no call is up, ringing or being set up; the [`CallBar`] above the panes shows the incoming call's
//! banner (Answer, Decline) and the in-call bar (Mute, Hang up). The keys: Ctrl+Shift+C calls the chat shown,
//! Ctrl+Shift+J joins its running call, Ctrl+Shift+S answers and Ctrl+Shift+D declines the call that rings,
//! Ctrl+Shift+M mutes or unmutes, Ctrl+Shift+O turns the camera on or off (a build that shows video) and Ctrl+Shift+H
//! hangs up; the switcher offers the same as commands, with "Test call".
//! Traced: `call-button shown` (the chat, and `call`, `join` or `none`).

use std::sync::Arc;

use gpui::{
    AnyElement, AnyView, App, AppContext as _, Context, Entity, FocusHandle, Global, InteractiveElement as _,
    IntoElement, KeyBinding, MouseDownEvent, MouseMoveEvent, ParentElement as _, Render, ScrollWheelEvent,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, actions, div,
    prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::tooltip::Tooltip;
use gpui_component::{Icon, ResizableState, Size, h_resizable, resizable_panel};
use teams_core::{Availability, Reporting};
use teams_proto::ManualAvailability;

use crate::accounts::{AccountTile, Accounts};
use crate::bridge::Core;
use crate::call_view::CallBar;
use crate::calls::{self, Ask};
use crate::channel_view::ChannelView;
use crate::chat_list::{ChatList, ChatListEvent};
use crate::chat_view::{self, ChatView};
use crate::first_run::FirstRun;
use crate::models::{Account, Models};
use crate::own_presence::{self, OwnPresence};
use crate::people;
use crate::presence;
use crate::search::{HitTarget, Search, SearchEvent};
use crate::settings::{ManualStatus, SettingsModel};
use crate::settings_view::SettingsView;
use crate::sidebar::Sidebar;
use crate::switcher::{CallCommand, Switcher, SwitcherEvent};
use crate::teams_list::{ChannelChosen, TeamsList, TeamsListEvent};
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};
use crate::view::status_text;

actions!(teams_ui, [ToggleSwitcher, ToggleSearch, NextAccount, PreviousAccount]);
actions!(
    teams_ui,
    [
        CallChat,
        JoinCall,
        AnswerCall,
        DeclineCall,
        HangUpCall,
        ToggleMuteCall,
        ToggleCameraCall
    ]
);

/// The shell's key context.
pub const CONTEXT: &str = "Shell";
/// The list pane's width when the window opens, px.
pub const LIST_WIDTH: f32 = 320.0;
/// The narrowest the list pane gets, px.
pub const LIST_MIN: f32 = 220.0;
/// The widest the list pane gets, px.
pub const LIST_MAX: f32 = 560.0;
/// An account's avatar on the rail, px.
const ACCOUNT_AVATAR: f32 = 36.0;
/// An account's unread badge's colour, 0xRRGGBB (Teams' red).
const UNREAD_BADGE: u32 = 0x00c4_314b;
/// The rail's width, px.
pub const RAIL_WIDTH: f32 = 76.0;
/// A rail item's height, px.
const RAIL_ITEM_HEIGHT: f32 = 52.0;
/// A rail item's icon's size, px.
const RAIL_ICON: f32 = 22.0;

/// A rail item: its icon (Lucide's, ISC: `assets/icons/LICENSE-LUCIDE`) and its caption.
#[derive(Clone, Copy)]
struct RailItem {
    icon: &'static [u8],
    label: &'static str,
}

impl RailItem {
    const CHAT: Self = Self {
        icon: include_bytes!("../assets/icons/chat.svg"),
        label: "Chat",
    };
    const SETTINGS: Self = Self {
        icon: include_bytes!("../assets/icons/settings.svg"),
        label: "Settings",
    };
}

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
        KeyBinding::new("ctrl-alt-down", NextAccount, None),
        KeyBinding::new("ctrl-alt-up", PreviousAccount, None),
        KeyBinding::new("ctrl-shift-c", CallChat, None),
        KeyBinding::new("ctrl-shift-j", JoinCall, None),
        KeyBinding::new("ctrl-shift-s", AnswerCall, None),
        KeyBinding::new("ctrl-shift-d", DeclineCall, None),
        KeyBinding::new("ctrl-shift-h", HangUpCall, None),
        KeyBinding::new("ctrl-shift-m", ToggleMuteCall, None),
        // Teams' own key for the camera
        KeyBinding::new("ctrl-shift-o", ToggleCameraCall, None),
    ]);
}

/// What a button of the shell does.
type ShellAction = fn(&mut Shell, &mut Context<Shell>);

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
    /// The list pane: the chats and the teams.
    sidebar: Entity<Sidebar>,
    /// The chat opened in the conversation pane.
    chat: Option<Entity<ChatView>>,
    /// The channel opened in the conversation pane, with the names its header shows.
    channel: Option<(Entity<ChannelView>, ChannelChosen)>,
    settings_view: Entity<SettingsView>,
    /// The first run's screen, shown over everything while it waits ([`SettingsModel::awaiting_first_run`]).
    first_run: Entity<FirstRun>,
    switcher: Option<(Entity<Switcher>, Subscription)>,
    search: Option<(Entity<Search>, Subscription)>,
    main: Main,
    /// What the settings hid, shown again when the rail's Chat leaves them.
    before_settings: Main,
    panes: Entity<ResizableState>,
    focus: FocusHandle,
    /// The account's own presence, once the desktop handed it in, with the shell's observation of it.
    own_presence: Option<(Entity<OwnPresence>, Subscription)>,
    /// The account menu is open.
    status_menu: bool,
    /// What the chat's header was last traced showing.
    header_traced: Option<HeaderShown>,
    /// The incoming call's banner and the in-call bar.
    call_bar: Entity<CallBar>,
    /// The chat whose call button was last traced, and which it showed.
    call_button_traced: Option<(String, &'static str)>,
    /// The app's accounts, once the desktop handed them in ([`Shell::set_accounts`]): the rail shows their avatars.
    accounts: Option<(Entity<Accounts>, Subscription)>,
    _subscriptions: [Subscription; 8],
}

/// What the chat's header shows, as the trace tells it: the chat, its other member's presence (a 1:1 chat's), and
/// whether its name came from people's names (or its topic) and its avatar from a picture. Never the name itself.
#[derive(Clone, Debug, PartialEq, Eq)]
struct HeaderShown {
    chat: String,
    presence: &'static str,
    named: bool,
    picture: bool,
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
        let selected = settings
            .read(cx)
            .settings()
            .selected_chat_of(core.me())
            .map(str::to_owned);
        let list_trace = Arc::clone(&trace);
        let (list_core, list_models) = (Arc::clone(&core), models.clone());
        let list = cx.new(|cx| ChatList::new(list_core, list_models, list_trace, selected.clone(), cx));
        let opened = cx.subscribe_in(&list, window, |this: &mut Self, _, event, window, cx| match event {
            ChatListEvent::Opened(id) => this.open_chat(id.clone(), window, cx),
        });
        let (teams, [channel_opened, kept_channel]) = Self::teams_list(&core, &models, &trace, window, cx);
        let sidebar_parts = (list.clone(), teams.clone(), models.clone(), Arc::clone(&trace));
        let sidebar = cx.new(|cx| {
            let (list, teams, models, trace) = sidebar_parts;
            Sidebar::new(list, teams, models, trace, cx)
        });
        let account = core.me().to_owned();
        let settings_view = cx.new(|cx| SettingsView::new(settings.clone(), cx).for_account(account));
        let first_run = cx.new(|cx| FirstRun::new(settings.clone(), cx));
        // the first run's screen comes and goes with the settings
        let settled = cx.observe(&settings, |_: &mut Self, _, cx| cx.notify());
        // a presence the engine told: the chat's header draws its dot again
        let presence = cx.observe(&models.presence, |this: &mut Self, _, cx| {
            this.trace_header(cx);
            cx.notify();
        });
        // the list's rows built again (a name or a picture read): the header shows what its row shows
        let renamed = cx.observe(&list, |this: &mut Self, _, cx| this.trace_header(cx));
        let typed = Self::keystrokes_are_use(window, cx);
        let (call_bar, calling) = Self::calls(&core, &models, &list, &trace, cx);
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let chat_parts = ShellParts {
            core: Arc::clone(&core),
            models: models.clone(),
            settings: settings.clone(),
            trace: Arc::clone(&trace),
        };
        let chat = Self::kept_chat(chat_parts, selected.clone(), window, cx);
        Self {
            core,
            models,
            settings,
            trace,
            list,
            teams,
            sidebar,
            chat,
            channel: None,
            settings_view,
            first_run,
            switcher: None,
            search: None,
            main: selected.map_or(Main::Nothing, Main::Chat),
            before_settings: Main::Nothing,
            panes: cx.new(|_| ResizableState::default()),
            focus,
            own_presence: None,
            status_menu: false,
            header_traced: None,
            call_bar,
            call_button_traced: None,
            accounts: None,
            _subscriptions: [
                opened,
                channel_opened,
                kept_channel,
                presence,
                typed,
                renamed,
                settled,
                calling,
            ],
        }
    }

    /// The Teams list, with the subscriptions that open a channel it chose, and a channel kept as the selected chat.
    fn teams_list(
        core: &Arc<dyn Core>,
        models: &Models,
        trace: &Arc<ViewTrace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<TeamsList>, [Subscription; 2]) {
        let (core, models, trace) = (Arc::clone(core), models.clone(), Arc::clone(trace));
        let teams = cx.new(|cx| TeamsList::new(core, models, trace, cx));
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
        (teams, [channel_opened, kept_channel])
    }

    /// The chat view of `selected`, the chat the settings kept, opened again and taking what is typed.
    fn kept_chat(
        parts: ShellParts,
        selected: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<ChatView>> {
        let id = selected?;
        parts.trace.record(&TraceLine::new("chat", "opened").conversation(&id));
        let chat = cx.new(|cx| ChatView::new(parts, id, window, cx));
        let compose = chat.read(cx).compose().clone();
        compose.update(cx, |compose, cx| compose.focus(window, cx));
        Some(chat)
    }

    /// The call bar over `models`' calls, and the observation that keeps the header's call button up to date.
    fn calls(
        core: &Arc<dyn Core>,
        models: &Models,
        list: &Entity<ChatList>,
        trace: &Arc<ViewTrace>,
        cx: &mut Context<Self>,
    ) -> (Entity<CallBar>, Subscription) {
        let (core, list, trace, calls) = (Arc::clone(core), list.clone(), Arc::clone(trace), models.calls.clone());
        let call_bar = cx.new(|cx| CallBar::new(core, calls, list, trace, cx));
        // a call up, rung or ended: the header's call button follows
        let calling = cx.observe(&models.calls, |this: &mut Self, _, cx| {
            this.trace_call_button(cx);
            cx.notify();
        });
        (call_bar, calling)
    }

    /// The incoming call's banner and the in-call bar.
    #[must_use]
    pub fn call_bar(&self) -> &Entity<CallBar> {
        &self.call_bar
    }

    /// The call button the chat shown has: `call`, `join` (a running call there, from an invitation), or `none` (no
    /// chat shown, calls not shown, the calling endpoint not registered, or a call up or ringing).
    #[must_use]
    pub fn call_button(&self, cx: &App) -> &'static str {
        let Main::Chat(id) = &self.main else {
            return "none";
        };
        let calls = self.models.calls.read(cx);
        if !calls.idle() || !calls.registered() || self.busy_elsewhere(cx) {
            "none"
        } else if calls.joinable(id) {
            "join"
        } else {
            "call"
        }
    }

    /// Traces the chat's call button (`call-button shown`) when it changed; nothing while calls aren't shown.
    fn trace_call_button(&mut self, cx: &App) {
        let Main::Chat(id) = &self.main else {
            return;
        };
        if !self.models.calls.read(cx).shows() {
            return;
        }
        let shown = (id.clone(), self.call_button(cx));
        if self.call_button_traced.as_ref() == Some(&shown) {
            return;
        }
        self.trace.record(
            &TraceLine::new("call-button", "shown")
                .conversation(&shown.0)
                .kind(shown.1),
        );
        self.call_button_traced = Some(shown);
    }

    /// The user asks `ask` of the calls (a key, a button, a command).
    pub fn ask_call(&mut self, ask: Ask, cx: &mut Context<Self>) {
        let enters = matches!(ask, Ask::Call { .. } | Ask::Join { .. } | Ask::TestCall | Ask::Answer);
        if enters && self.busy_elsewhere(cx) {
            calls::refuse_here(&self.models.calls, &self.trace, &ask, "another-account", cx);
            return;
        }
        calls::ask(&self.models.calls, &self.core, &self.trace, ask, cx);
    }

    /// The account's models the shell shows.
    #[must_use]
    pub fn models(&self) -> &Models {
        &self.models
    }

    /// The account the shell is: its MRI.
    #[must_use]
    pub fn account(&self) -> &str {
        self.core.me()
    }

    /// Hands in the app's accounts: the rail shows their avatars in place of the status button, and no call is offered
    /// while another account has one ([`Accounts::busy_elsewhere`]).
    pub fn set_accounts(&mut self, accounts: Entity<Accounts>, cx: &mut Context<Self>) {
        let observing = cx.observe(&accounts, |this: &mut Self, _, cx| {
            this.trace_call_button(cx);
            cx.notify();
        });
        self.accounts = Some((accounts, observing));
        cx.notify();
    }

    /// Takes the window's keyboard: the chat's compose box when a chat is shown, else the shell (an account switched to).
    pub fn take_focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        match (&self.main, &self.chat) {
            (Main::Chat(_), Some(chat)) => {
                let compose = chat.read(cx).compose().clone();
                compose.update(cx, |compose, cx| compose.focus(window, cx));
            }
            _ => self.focus.focus(window, cx),
        }
    }

    /// Shows the next account of the app (`forward`) or the one before, round: Ctrl+Alt+Down and Ctrl+Alt+Up.
    pub fn cycle_account(&mut self, forward: bool, cx: &mut Context<Self>) {
        if let Some((accounts, _)) = &self.accounts {
            accounts.update(cx, |accounts, cx| accounts.cycle(forward, cx));
        }
    }

    /// Whether another account of the app has a call up, ringing or being set up.
    fn busy_elsewhere(&self, cx: &App) -> bool {
        self.accounts
            .as_ref()
            .is_some_and(|(accounts, _)| accounts.read(cx).busy_elsewhere(self.core.me(), cx))
    }

    /// Calls the chat shown (a 1:1 chat's other member, a group chat's members).
    pub fn call_chat(&mut self, cx: &mut Context<Self>) {
        if let Main::Chat(id) = &self.main {
            let chat = id.clone();
            self.ask_call(Ask::Call { chat }, cx);
        } else {
            let ask = Ask::Call { chat: String::new() };
            calls::refuse_here(&self.models.calls, &self.trace, &ask, "no-chat", cx);
        }
    }

    /// Joins the running call of the chat shown.
    pub fn join_call(&mut self, cx: &mut Context<Self>) {
        if let Main::Chat(id) = &self.main {
            let chat = id.clone();
            self.ask_call(Ask::Join { chat }, cx);
        } else {
            let ask = Ask::Join { chat: String::new() };
            calls::refuse_here(&self.models.calls, &self.trace, &ask, "no-chat", cx);
        }
    }

    /// What a call command of the switcher asks.
    fn call_command(&mut self, command: CallCommand, cx: &mut Context<Self>) {
        match command {
            CallCommand::CallChat => self.call_chat(cx),
            CallCommand::Join => self.join_call(cx),
            CallCommand::TestCall => self.ask_call(Ask::TestCall, cx),
            CallCommand::Answer => self.ask_call(Ask::Answer, cx),
            CallCommand::Decline => self.ask_call(Ask::Decline, cx),
            CallCommand::HangUp => self.ask_call(Ask::HangUp, cx),
            CallCommand::ToggleMute => self.ask_call(Ask::ToggleMute, cx),
            CallCommand::ToggleCamera => self.ask_call(Ask::ToggleCamera, cx),
        }
    }

    /// The chat header's call button (Call, or Join), when it has one.
    fn call_header_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (id, label, on): (&'static str, &'static str, ShellAction) = match self.call_button(cx) {
            "call" => ("chat-call", "Call", Self::call_chat),
            "join" => ("chat-join", "Join", Self::join_call),
            _ => return None,
        };
        let button = Button::new(id)
            .label(label)
            .compact()
            .primary()
            .on_click(cx.listener(move |this, _, _, cx| on(this, cx)));
        Some(
            div()
                .debug_selector(move || id.to_owned())
                .child(button)
                .into_any_element(),
        )
    }

    /// Traces what the chat's header shows (`header shown`: the chat; its other member's presence, `none` for a chat
    /// that isn't 1:1, then `named` or `unnamed`, then `picture` or `no-picture`), when it changed. Nothing while no
    /// chat is shown, or before the list has its row.
    fn trace_header(&mut self, cx: &App) {
        let Main::Chat(id) = &self.main else {
            return;
        };
        let list = self.list.read(cx);
        let Some(row) = list.row(id) else {
            return;
        };
        let shown = HeaderShown {
            chat: id.clone(),
            presence: list
                .presence_of(row, cx)
                .map_or("none", |availability| presence::key(&availability)),
            named: row.named,
            picture: row.picture.is_some(),
        };
        if self.header_traced.as_ref() == Some(&shown) {
            return;
        }
        self.trace.record(
            &TraceLine::new("header", "shown")
                .conversation(&shown.chat)
                .kind(shown.presence)
                .kind(if shown.named { "named" } else { "unnamed" })
                .kind(if shown.picture { "picture" } else { "no-picture" }),
        );
        self.header_traced = Some(shown);
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

    /// The user chose `status` in the account menu: handed to the own presence (which sends it, or sends it again when
    /// it is the status already chosen and its report failed) and kept in the settings, the menu closed.
    pub fn choose_status(&mut self, status: ManualStatus, cx: &mut Context<Self>) {
        self.trace
            .record(&TraceLine::new("status-menu", "chosen").kind(status.key()));
        if let Some((own, _)) = &self.own_presence {
            own.update(cx, |own, cx| own.choose(status.choice(), cx));
        }
        // unchanged (the same status chosen again), the settings tell nobody: the own presence was told above
        let account = self.core.me().to_owned();
        self.settings.update(cx, |settings, cx| {
            settings.change(|settings| settings.set_manual_status(&account, status), cx);
        });
        self.status_menu = false;
        cx.notify();
    }

    /// The account menu's "Sign out and remove": asked of the app (the menu closed).
    fn ask_remove_account(&mut self, cx: &mut Context<Self>) {
        self.status_menu = false;
        if let Some((accounts, _)) = &self.accounts {
            let key = self.core.me().to_owned();
            accounts.update(cx, |accounts, cx| accounts.ask_remove(&key, cx));
        }
        cx.notify();
    }

    /// Every keystroke in the shell's window counts as its use. Seen before any key binding (an interceptor): a key
    /// listener never hears a key a binding took (Ctrl+K, the chat view's keys), and keyboard-only use would go unseen.
    fn keystrokes_are_use(window: &Window, cx: &mut Context<Self>) -> Subscription {
        let shells_window = window.window_handle();
        let shell = cx.weak_entity();
        cx.intercept_keystrokes(move |_, window, cx| {
            if window.window_handle() == shells_window {
                // the shell gone: its window is going too
                let _ = shell.update(cx, Self::used);
            }
        })
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

    /// The list pane: the chats and the teams.
    #[must_use]
    pub fn sidebar(&self) -> &Entity<Sidebar> {
        &self.sidebar
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
        self.list.update(cx, |list, cx| list.select(None, cx));
        // a chat view of the channel itself (kept as the selected chat by an earlier build) is dropped, and the channel
        // isn't the selected chat any more
        if self
            .chat
            .as_ref()
            .is_some_and(|chat| chat.read(cx).conversation() == chosen.id)
        {
            self.chat = None;
        }
        let account = self.core.me().to_owned();
        if self.settings.read(cx).settings().selected_chat_of(&account) == Some(chosen.id.as_str()) {
            self.settings.update(cx, |settings, cx| {
                settings.change(|settings| settings.set_selected_chat(&account, None), cx);
            });
        }
        if let Some(chat) = &self.chat {
            chat.update(cx, |chat, cx| chat.set_shown(false, cx));
        }
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
        self.teams.update(cx, |teams, cx| teams.select(None, cx));
        let (account, kept) = (self.core.me().to_owned(), id.clone());
        self.settings.update(cx, |settings, cx| {
            settings.change(|settings| settings.set_selected_chat(&account, Some(kept)), cx);
        });
        self.main = Main::Chat(id);
        self.trace_header(cx);
        self.trace_call_button(cx);
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
            self.before_settings = std::mem::replace(&mut self.main, Main::Settings);
            if let Some(chat) = &self.chat {
                chat.update(cx, |chat, cx| chat.set_shown(false, cx));
            }
            cx.notify();
        }
    }

    /// The rail's Chat: leaves the settings for what they hid (the chat or the channel shown before them, opened
    /// again), or nothing; elsewhere it does nothing.
    pub fn show_chats(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.main != Main::Settings {
            return;
        }
        match std::mem::replace(&mut self.before_settings, Main::Nothing) {
            Main::Chat(id) => self.open_chat(id, window, cx),
            Main::Channel(id) => self.open_channel_id(id, window, cx),
            Main::Nothing | Main::Settings => {
                self.main = Main::Nothing;
                cx.notify();
            }
        }
    }

    /// Opens the switcher, or closes it when it is open.
    pub fn toggle_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.switcher.is_some() {
            self.close_switcher(window, cx);
            return;
        }
        self.close_search(window, cx);
        let (list, teams) = (self.list.clone(), self.teams.clone());
        let core = Arc::clone(&self.core);
        let shown = match &self.main {
            Main::Chat(id) => Some(id.clone()),
            _ => None,
        };
        let calls = (self.models.calls.clone(), shown);
        let switcher = cx.new(|cx| Switcher::new(list, teams, core, calls, window, cx));
        let events = cx.subscribe_in(&switcher, window, |this: &mut Self, _, event, window, cx| match event {
            SwitcherEvent::Open(id) => this.open_chat(id.clone(), window, cx),
            SwitcherEvent::OpenChannel(id) => this.open_channel_id(id.clone(), window, cx),
            SwitcherEvent::OpenMessage(target) => this.open_message(target.clone(), window, cx),
            SwitcherEvent::Dismissed => this.close_switcher(window, cx),
            SwitcherEvent::CopyDiagnostics => this.copy_diagnostics(window, cx),
            SwitcherEvent::SetStatus(status) => {
                this.choose_status(*status, cx);
                this.close_switcher(window, cx);
            }
            SwitcherEvent::Call(command) => {
                this.close_switcher(window, cx);
                this.call_command(*command, cx);
            }
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

    /// A rail item as Teams draws it: its icon over a small caption, the chosen one in the brand colour with a mark on
    /// its left edge.
    fn rail_button(
        id: &'static str,
        item: RailItem,
        chosen: bool,
        palette: Palette,
        cx: &mut Context<Self>,
        on: fn(&mut Self, &mut Window, &mut Context<Self>),
    ) -> AnyElement {
        let color = if chosen {
            palette.brand
        } else {
            palette.muted_foreground
        };
        let mark = div()
            .absolute()
            .left_0()
            .top(px(RAIL_ITEM_HEIGHT / 4.0))
            .w(px(3.0))
            .h(px(RAIL_ITEM_HEIGHT / 2.0))
            .rounded_r_sm()
            .when(chosen, |mark| mark.bg(rgb(palette.brand)));
        div()
            .debug_selector(move || id.to_owned())
            .id(id)
            .relative()
            .w_full()
            .h(px(RAIL_ITEM_HEIGHT))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_0p5()
            .cursor_pointer()
            .text_color(rgb(color))
            .hover(move |style| style.bg(rgb(palette.hover)).text_color(rgb(palette.brand)))
            .on_click(cx.listener(move |this, _, window, cx| on(this, window, cx)))
            .child(mark)
            .child(Icon::default().data(item.icon).size(px(RAIL_ICON)))
            .child(div().text_size(px(10.0)).child(item.label))
            .into_any_element()
    }

    fn rail_view(&self, palette: Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let chats = self.main != Main::Settings;
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
            .child(Self::rail_button(
                "rail-chats",
                RailItem::CHAT,
                chats,
                palette,
                cx,
                Self::show_chats,
            ))
            .child(div().flex_1())
            .children(self.account_rail(palette, cx))
            .children(self.accounts.is_none().then(|| self.status_button(palette, cx)))
            .child(Self::rail_button(
                "rail-settings",
                RailItem::SETTINGS,
                self.main == Main::Settings,
                palette,
                cx,
                |this, _, cx| this.show_settings(cx),
            ))
    }

    /// What the account's own presence reports now, as the account menu shows it; `None` before the desktop handed it
    /// in.
    #[must_use]
    pub fn own_reporting(&self, cx: &App) -> Option<Reporting> {
        self.own_presence().map(|own| own.read(cx).reporting(cx))
    }

    /// The rail's account avatars (see [`crate::accounts`]) and its "+", once the accounts were handed in; nothing
    /// before.
    fn account_rail(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (accounts, _) = self.accounts.as_ref()?;
        let accounts = accounts.read(cx);
        let me = self.core.me();
        let faces: Vec<AccountFace> = accounts
            .tiles()
            .iter()
            .enumerate()
            .map(|(index, tile)| AccountFace::of(index, tile, tile.key == me, cx))
            .collect();
        let can_add = accounts.can_add();
        let avatars: Vec<AnyElement> = faces
            .into_iter()
            .map(|face| self.account_avatar(face, palette, cx))
            .collect();
        let add = can_add.then(|| {
            let accounts = self.accounts.as_ref().map(|(accounts, _)| accounts.clone());
            div()
                .debug_selector(|| "rail-add-account".to_owned())
                .id("rail-add-account")
                .size(px(ACCOUNT_AVATAR))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_1()
                .border_color(rgb(palette.border))
                .text_color(rgb(palette.muted_foreground))
                .cursor_pointer()
                .hover(move |style| style.bg(rgb(palette.hover)).text_color(rgb(palette.brand)))
                .tooltip(|window, cx| Tooltip::new("Add an account").build(window, cx))
                .on_click(move |_, _, cx| {
                    if let Some(accounts) = &accounts {
                        accounts.update(cx, Accounts::ask_add);
                    }
                })
                .child("+")
        });
        Some(
            div()
                .debug_selector(|| "rail-accounts".to_owned())
                .w_full()
                .flex()
                .flex_col()
                .items_center()
                .gap_2()
                .pb_2()
                .children(avatars)
                .children(add)
                .into_any_element(),
        )
    }

    /// One account's avatar on the rail: its picture or initials, its own presence's dot, the account shown marked
    /// (a click opens its account menu: selector `rail-status`), another with its unread total (a click shows it:
    /// selector `rail-account-<n>`), a signed-out one faded.
    fn account_avatar(&self, account: AccountFace, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let AccountFace {
            index,
            key,
            name,
            detail,
            picture,
            reporting,
            unread,
            signed_out,
            shown,
        } = account;
        let selector: SharedString = if shown {
            "rail-status".into()
        } else {
            format!("rail-account-{index}").into()
        };
        let face = people::avatar(Some(&name), picture.as_ref(), Size::Medium);
        let dot = reporting.map(|reporting| {
            let dot = presence::dot(&shown_as(reporting), palette)
                .absolute()
                .right(px(-2.0))
                .bottom(px(-2.0));
            if shown {
                dot.debug_selector(|| "rail-status-dot".to_owned())
            } else {
                dot
            }
        });
        let badge = (!shown && unread > 0).then(|| {
            div()
                .debug_selector(move || format!("rail-account-{index}-unread"))
                .absolute()
                .top(px(-4.0))
                .right(px(-6.0))
                .min_w(px(16.0))
                .h(px(16.0))
                .px_1()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(rgb(UNREAD_BADGE))
                .text_color(rgb(0x00ff_ffff))
                .text_size(px(10.0))
                .child(SharedString::from(if unread > 99 {
                    "99+".to_owned()
                } else {
                    unread.to_string()
                }))
        });
        let tip: SharedString = format!("{name}\n{detail}").into();
        div()
            .debug_selector({
                let selector = selector.clone();
                move || selector.to_string()
            })
            .id(selector)
            .relative()
            .flex_none()
            .p(px(2.0))
            .rounded_full()
            .border_2()
            .border_color(rgb(if shown { palette.brand } else { palette.rail }))
            .cursor_pointer()
            .when(signed_out, |avatar| avatar.opacity(0.45))
            .when(shown && self.status_menu, |avatar| avatar.bg(rgb(palette.border)))
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .on_click(cx.listener(move |this, _, _, cx| {
                if this.core.me() == key {
                    this.toggle_status_menu(cx);
                } else if let Some((accounts, _)) = &this.accounts {
                    accounts.update(cx, |accounts, cx| accounts.switch(&key, cx));
                }
            }))
            .child(face)
            .children(dot)
            .children(badge)
            .into_any_element()
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

    /// The account menu's account: its name and what tells it apart above the statuses, and its "Sign out and remove"
    /// below them in an app that adds and removes accounts; neither before the accounts were handed in.
    fn account_menu_parts(&self, palette: Palette, cx: &mut Context<Self>) -> (Option<gpui::Div>, Option<gpui::Div>) {
        let account = self.accounts.as_ref().and_then(|(accounts, _)| {
            let accounts = accounts.read(cx);
            let tile = accounts.tile(self.core.me())?;
            Some((tile.name.clone(), tile.detail.clone(), accounts.can_add()))
        });
        let header = account.as_ref().map(|(name, detail, _)| {
            div()
                .debug_selector(|| "status-menu-account".to_owned())
                .pb_1()
                .mb_1()
                .border_b_1()
                .border_color(rgb(palette.border))
                .child(div().font_weight(gpui::FontWeight::BOLD).child(name.clone()))
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(palette.muted_foreground))
                        .child(detail.clone()),
                )
        });
        let remove = account.filter(|(_, _, can_add)| *can_add).map(|_| {
            div()
                .debug_selector(|| "status-menu-remove-account".to_owned())
                .w_full()
                .mt_1()
                .pt_1()
                .border_t_1()
                .border_color(rgb(palette.border))
                .child(
                    Button::new("status-menu-remove-account")
                        .label("Sign out and remove")
                        .compact()
                        .ghost()
                        .w_full()
                        .on_click(cx.listener(|this, _, _, cx| this.ask_remove_account(cx))),
                )
        });
        (header, remove)
    }

    /// The account menu: the statuses the user may set (the one set marked), what is reported, and the last failed
    /// report.
    fn status_menu_view(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let chosen = self.settings.read(cx).settings().manual_status_of(self.core.me());
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
        let (header, remove) = self.account_menu_parts(palette, cx);
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
            .children(header)
            .children(entries)
            .children(reported)
            .children(failure)
            .children(remove)
            .into_any_element()
    }

    fn list_pane(&self) -> AnyElement {
        div()
            .debug_selector(|| "list-pane".to_owned())
            .size_full()
            .child(self.sidebar.clone())
            .into_any_element()
    }

    fn main_pane(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
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
                let call = self.call_header_button(cx);
                let header = pane_header(name, palette)
                    .gap_2()
                    .children(presence)
                    .child(div().flex_1())
                    .children(call)
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
                    .child(self.list_pane()),
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
        let first_run = self.settings.read(cx).awaiting_first_run().then(|| {
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::rgba(0x0000_0080))
                .child(self.first_run.clone())
        });
        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            // the window's use, for the account's own presence (its keystrokes: `keystrokes_are_use`)
            .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, _, cx| this.used(cx)))
            .capture_any_mouse_down(cx.listener(|this, _: &MouseDownEvent, _, cx| this.used(cx)))
            .on_scroll_wheel(cx.listener(|this, _: &ScrollWheelEvent, _, cx| this.used(cx)))
            .on_action(cx.listener(|this, _: &ToggleSwitcher, window, cx| this.toggle_switcher(window, cx)))
            .on_action(cx.listener(|this, _: &ToggleSearch, window, cx| this.toggle_search(window, cx)))
            .on_action(cx.listener(|this, _: &NextAccount, _, cx| this.cycle_account(true, cx)))
            .on_action(cx.listener(|this, _: &PreviousAccount, _, cx| this.cycle_account(false, cx)))
            // the call keys only in a build that shows calls: elsewhere they reach whatever else takes them
            .when(self.models.calls.read(cx).shows(), |shell| {
                shell
                    .on_action(cx.listener(|this, _: &CallChat, _, cx| this.call_chat(cx)))
                    .on_action(cx.listener(|this, _: &JoinCall, _, cx| this.join_call(cx)))
                    .on_action(cx.listener(|this, _: &AnswerCall, _, cx| this.ask_call(Ask::Answer, cx)))
                    .on_action(cx.listener(|this, _: &DeclineCall, _, cx| this.ask_call(Ask::Decline, cx)))
                    .on_action(cx.listener(|this, _: &HangUpCall, _, cx| this.ask_call(Ask::HangUp, cx)))
                    .on_action(cx.listener(|this, _: &ToggleMuteCall, _, cx| this.ask_call(Ask::ToggleMute, cx)))
                    .on_action(cx.listener(|this, _: &ToggleCameraCall, _, cx| {
                        this.ask_call(Ask::ToggleCamera, cx);
                    }))
            })
            .relative()
            .size_full()
            .flex()
            .child(self.rail_view(palette, cx))
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(self.call_bar.clone())
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .child(self.panes_or_signed_out(panes, palette, cx)),
                    ),
            )
            .children(switcher)
            .children(search)
            .children(status_menu)
            .children(first_run)
    }
}

/// What the rail shows of one account, read before it is drawn.
struct AccountFace {
    index: usize,
    key: String,
    name: SharedString,
    detail: SharedString,
    picture: Option<people::Picture>,
    reporting: Option<Reporting>,
    unread: u64,
    signed_out: bool,
    /// It is the account this shell is.
    shown: bool,
}

impl AccountFace {
    fn of(index: usize, tile: &AccountTile, shown: bool, cx: &App) -> Self {
        Self {
            index,
            key: tile.key.clone(),
            name: tile.name.clone(),
            detail: tile.detail.clone(),
            picture: tile.picture.clone(),
            reporting: tile.own_presence.as_ref().map(|own| own.read(cx).reporting(cx)),
            unread: tile.unread(cx),
            signed_out: tile.signed_out(cx),
            shown,
        }
    }
}

impl Shell {
    /// The panes; in place of them, once the account is signed out in an app of accounts, what to do (the rail stays,
    /// to switch to another account or remove this one).
    fn panes_or_signed_out(&self, panes: impl IntoElement, palette: Palette, cx: &App) -> AnyElement {
        let status = self.models.status.read(cx);
        if self.accounts.is_none() || !matches!(status.account, Account::SignedOut { .. }) {
            return div().size_full().child(panes).into_any_element();
        }
        div()
            .debug_selector(|| "account-signed-out".to_owned())
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(rgb(palette.muted_foreground))
            .child(status_text(status))
            .into_any_element()
    }
}

/// The dot the rail's status button shows for what is reported: a manual status's own, Available's green, grey while
/// nothing is reported, none (an empty ring) while it isn't known.
#[must_use]
pub fn shown_as(reporting: Reporting) -> Availability {
    match reporting {
        Reporting::Available | Reporting::Manual(ManualAvailability::Available) => Availability::Available,
        Reporting::Manual(ManualAvailability::Busy) => Availability::Busy,
        Reporting::Manual(ManualAvailability::DoNotDisturb) => Availability::DoNotDisturb,
        Reporting::Manual(ManualAvailability::Away) => Availability::Away,
        Reporting::Inactive | Reporting::Halted | Reporting::Quit => Availability::Offline,
        // a manual status (or reset) that failed: what the account shows isn't known
        Reporting::Connecting | Reporting::NotSet(_) => Availability::Unknown,
    }
}
