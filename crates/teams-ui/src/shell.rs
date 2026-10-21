//! The shell: the window's three panes, Teams-like and compact. On the left the rail (Chat, Teams, and Settings at
//! its foot), then the list pane (the [`ChatList`] for Chat; teams and channels for Teams, Task 16), resizable by the
//! handle on its right edge, then the conversation pane (the chat opened in a [`ChatView`] under its name, or the
//! settings).
//!
//! The keyboard: Ctrl+K opens the [`Switcher`] over the panes (and closes it again); a chat chosen there or clicked in
//! the list opens in the conversation pane and is kept as the selected chat in the settings, so the app opens it again
//! at its next start. Ctrl+Shift+F opens the [`Search`] of the stored messages the same way (one of the two at a
//! time); a message found there or in the switcher opens its chat at the message ([`ChatView::reveal`]). What the
//! shell shows is traced (`chat opened`, `settings opened`, `switcher opened`/`closed`, `search opened`/`closed`).

use std::sync::Arc;

use gpui::{
    AnyElement, AnyView, App, AppContext as _, Context, Entity, FocusHandle, Global, InteractiveElement as _,
    IntoElement, KeyBinding, ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, actions, div,
    px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ResizableState, h_resizable, resizable_panel};

use crate::bridge::Core;
use crate::chat_list::{ChatList, ChatListEvent};
use crate::chat_view::{self, ChatView};
use crate::models::Models;
use crate::search::{HitTarget, Search, SearchEvent};
use crate::settings::SettingsModel;
use crate::settings_view::SettingsView;
use crate::switcher::{Switcher, SwitcherEvent};
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
    /// The chat opened in the conversation pane.
    chat: Option<Entity<ChatView>>,
    settings_view: Entity<SettingsView>,
    switcher: Option<(Entity<Switcher>, Subscription)>,
    search: Option<(Entity<Search>, Subscription)>,
    rail: RailItem,
    main: Main,
    panes: Entity<ResizableState>,
    focus: FocusHandle,
    _subscriptions: [Subscription; 1],
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
        let settings_view = cx.new(|cx| SettingsView::new(settings.clone(), cx));
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
            chat,
            settings_view,
            switcher: None,
            search: None,
            rail: RailItem::Chats,
            main: selected.map_or(Main::Nothing, Main::Chat),
            panes: cx.new(|_| ResizableState::default()),
            focus,
            _subscriptions: [opened],
        }
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

    /// Opens a chat in the conversation pane and keeps it as the selected chat; the switcher closes.
    pub fn open_chat(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
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
        if let Some(chat) = &self.chat {
            chat.update(cx, |chat, cx| chat.reveal(target.arrival_time, target.message, cx));
        }
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

    /// Shows what the rail's `item` lists in the list pane.
    pub fn show_rail(&mut self, item: RailItem, cx: &mut Context<Self>) {
        if self.rail != item {
            self.rail = item;
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
        });
        switcher.update(cx, |switcher, cx| switcher.focus(window, cx));
        self.switcher = Some((switcher, events));
        self.trace.record(&TraceLine::new("switcher", "opened"));
        cx.notify();
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
            .child(Self::rail_button(
                "rail-settings",
                "Settings",
                self.main == Main::Settings,
                cx,
                Self::show_settings,
            ))
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
                .child(
                    div()
                        .debug_selector(|| "teams-list".to_owned())
                        .p_3()
                        .text_sm()
                        .text_color(rgb(palette.muted_foreground))
                        .child("No teams to show"),
                )
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
                pane.child(pane_header(name, palette).debug_selector(|| "chat-header".to_owned()))
                    .child(div().flex_1().min_h_0().children(chat))
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
        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &ToggleSwitcher, window, cx| this.toggle_switcher(window, cx)))
            .on_action(cx.listener(|this, _: &ToggleSearch, window, cx| this.toggle_search(window, cx)))
            .relative()
            .size_full()
            .flex()
            .child(self.rail_view(palette, cx))
            .child(div().flex_1().h_full().min_w_0().child(panes))
            .children(switcher)
            .children(search)
    }
}
