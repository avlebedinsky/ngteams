//! The Workflows app in the main window, as browser Teams opens it from the channel's Workflows ("Workflows Home",
//! "See all") or from the rail's More apps: its header (the app's icon and name, the Home, Templates and Chat tabs,
//! and its "..." menu) over the tab shown. Home and Templates are the app's pages ([`WorkflowDialog::new_app`]); Chat
//! is the account's chat with the Workflows bot, the same chat view as any chat. The bot is an app, not a person.
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px, rgb,
};
use gpui_component::button::ButtonVariants as _;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};

use crate::chat_view::ChatView;
use crate::icons::{Glyph, icon_button, selected_by};
use crate::shell::ShellParts;
use crate::theme::{self, Palette};
use crate::workflow_dialog::{AppTab, WorkflowDialog, workflows_icon};

/// The Workflows bot's app id: its chat with the account is `19:<account>_<bot>@unq.gbl.spaces`, as Teams names it.
pub const WORKFLOWS_BOT: &str = "358f0194-6b0e-4dd3-af35-c24fe8a9ec87";

/// The account's chat with the Workflows bot.
#[must_use]
pub fn bot_chat(me: &str) -> Option<String> {
    let account = me.strip_prefix("8:orgid:")?;
    Some(format!("19:{account}_{WORKFLOWS_BOT}@unq.gbl.spaces"))
}

/// The Workflows app's page (see the module docs).
pub struct WorkflowsApp {
    parts: ShellParts,
    pages: Entity<WorkflowDialog>,
    chat: Option<Entity<ChatView>>,
    tab: AppTab,
}

impl WorkflowsApp {
    pub fn new(parts: ShellParts, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let core = Arc::clone(&parts.core);
        let pages = cx.new(|cx| WorkflowDialog::new_app(core, window, cx));
        cx.observe(&pages, |_, _, cx| cx.notify()).detach();
        Self {
            parts,
            pages,
            chat: None,
            tab: AppTab::Home,
        }
    }

    /// The app's pages (Home, Templates, the card editor, the Overview).
    #[must_use]
    pub fn pages(&self) -> &Entity<WorkflowDialog> {
        &self.pages
    }

    /// The bot's chat, once the Chat tab was shown.
    #[must_use]
    pub fn chat(&self) -> Option<&Entity<ChatView>> {
        self.chat.as_ref()
    }

    /// The tab shown.
    #[must_use]
    pub fn tab(&self) -> AppTab {
        self.tab
    }

    /// Shows a tab, as the header's tabs do.
    pub fn show_tab(&mut self, tab: AppTab, window: &mut Window, cx: &mut Context<Self>) {
        // Back from Chat to the tab the pages were on, they show what they showed (an open card editor too).
        let returning = self.tab == AppTab::Chat && self.pages.read(cx).app_tab() == Some(tab);
        self.tab = tab;
        if !returning {
            self.pages.update(cx, |pages, cx| pages.show_tab(tab, cx));
        }
        if tab == AppTab::Chat {
            if self.chat.is_none()
                && let Some(conversation) = bot_chat(self.parts.core.me())
            {
                let parts = self.parts.clone();
                let chat = cx.new(|cx| ChatView::new(parts, conversation, window, cx));
                cx.observe(&chat, |_, _, cx| cx.notify()).detach();
                self.chat = Some(chat);
            }
            if let Some(chat) = &self.chat {
                chat.update(cx, |chat, cx| chat.set_shown(true, cx));
                let compose = chat.read(cx).compose().clone();
                compose.update(cx, |compose, cx| compose.focus(window, cx));
            }
        } else if let Some(chat) = &self.chat {
            chat.update(cx, |chat, cx| chat.set_shown(false, cx));
        }
        cx.notify();
    }

    /// The app's Home itself, whatever the pages showed (the channel's "Workflows Home").
    pub fn show_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_tab(AppTab::Home, window, cx);
        self.pages.update(cx, |pages, cx| pages.show_tab(AppTab::Home, cx));
    }

    /// Home's "See all": "Your workflows".
    pub fn show_all_workflows(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_home(window, cx);
        self.pages.update(cx, WorkflowDialog::show_all_workflows);
    }

    /// The app is shown again (the rail, More apps, history): it shows what it showed when it was left. The bot's
    /// chat is shown again; a list is read again, as changes made elsewhere may have left it behind.
    pub fn resume(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab == AppTab::Chat {
            self.show_tab(AppTab::Chat, window, cx);
        } else {
            self.pages.update(cx, WorkflowDialog::reread_workflows);
        }
    }

    /// The list the pages show is read again (after the channel's Workflows asked for the app).
    pub fn reread_workflows(&mut self, cx: &mut Context<Self>) {
        self.pages.update(cx, WorkflowDialog::reread_workflows);
    }

    /// The page is left (another place of the rail is shown): the bot's chat is hidden, so no read receipt goes.
    pub fn hide(&mut self, cx: &mut Context<Self>) {
        if let Some(chat) = &self.chat {
            chat.update(cx, |chat, cx| chat.set_shown(false, cx));
        }
    }

    /// The tab the header underlines: Chat, or the pages' own.
    fn shown_tab(&self, cx: &gpui::App) -> AppTab {
        if self.tab == AppTab::Chat {
            AppTab::Chat
        } else {
            self.pages.read(cx).app_tab().unwrap_or(AppTab::Home)
        }
    }

    fn header(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        div()
            .debug_selector(|| "workflows-header".to_owned())
            .flex_none()
            .h(px(60.0))
            .flex()
            .items_center()
            .gap_6()
            .px_5()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(Self::title())
            .child(self.tabs(palette, cx))
            .child(div().flex_1())
            .child(Self::more(cx))
            .into_any_element()
    }

    /// The app's icon on its white tile and its name.
    fn title() -> AnyElement {
        div()
            .flex()
            .items_center()
            .gap_4()
            .child(
                div()
                    .size(px(32.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(0x00e0_e0e0))
                    .bg(rgb(0x00ff_ffff))
                    .child(gpui::img(workflows_icon()).size(px(22.0))),
            )
            .child(div().text_xl().font_weight(gpui::FontWeight::BOLD).child("Workflows"))
            .into_any_element()
    }

    /// Home, Templates and Chat, the one shown bold and underlined.
    fn tabs(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let shown = self.shown_tab(cx);
        let tabs = [
            (AppTab::Home, "Home", "workflows-tab-home"),
            (AppTab::Templates, "Templates", "workflows-tab-templates"),
            (AppTab::Chat, "Chat", "workflows-tab-chat"),
        ];
        div()
            .h_full()
            .flex()
            .items_center()
            .gap_5()
            .children(tabs.map(|(tab, label, id)| {
                let chosen = shown == tab;
                div()
                    .id(id)
                    .debug_selector(move || id.to_owned())
                    .h_full()
                    .relative()
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .when(chosen, |face| face.font_weight(gpui::FontWeight::BOLD))
                    .when(!chosen, |face| {
                        face.hover(move |face| face.text_color(rgb(palette.brand)))
                    })
                    .child(label)
                    .child(
                        div()
                            .absolute()
                            .bottom_0()
                            .left_0()
                            .right_0()
                            .h(px(3.0))
                            .rounded_t_sm()
                            .when(chosen, |mark| mark.bg(rgb(palette.brand))),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| this.show_tab(tab, window, cx)))
            }))
            .into_any_element()
    }

    /// The app's "..." menu: Reload app.
    fn more(cx: &mut Context<Self>) -> AnyElement {
        let weak = cx.entity().downgrade();
        div()
            .child(selected_by(
                "workflows-more",
                icon_button("workflows-more", Glyph::ELLIPSIS, "More options")
                    .ghost()
                    .dropdown_menu(move |menu, _, _| {
                        let weak = weak.clone();
                        menu.item(
                            PopupMenuItem::element(|_, _| {
                                div()
                                    .debug_selector(|| "workflows-reload".to_owned())
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(Glyph::REFRESH.icon().size(px(14.0)))
                                    .child("Reload app")
                            })
                            .on_click(move |_, _, cx| {
                                let _ = weak.update(cx, WorkflowsApp::reload);
                            }),
                        )
                    }),
            ))
            .into_any_element()
    }

    /// "Reload app": the app's pages read everything again.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.pages.update(cx, WorkflowDialog::reload);
    }

    /// Teams' Chat tab before the first message: "You're starting a new conversation".
    fn new_conversation(palette: Palette) -> AnyElement {
        div()
            .debug_selector(|| "workflows-chat-new".to_owned())
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .bottom(px(140.0))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_4()
            .child(gpui::img(workflows_icon()).size(px(120.0)))
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("You're starting a new conversation"),
            )
            .child(
                div()
                    .text_color(rgb(palette.foreground))
                    .child("Type your first message below."),
            )
            .into_any_element()
    }
}

impl Render for WorkflowsApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let body: AnyElement = match (self.tab, &self.chat) {
            (AppTab::Chat, Some(chat)) => {
                let empty = chat.read(cx).is_empty();
                div()
                    .relative()
                    .size_full()
                    .child(chat.clone())
                    .when(empty, |body| body.child(Self::new_conversation(palette)))
                    .into_any_element()
            }
            (AppTab::Chat, None) => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child("This account's chat with Workflows can't be opened.")
                .into_any_element(),
            _ => self.pages.clone().into_any_element(),
        };
        div()
            .id(SharedString::from("workflows-app"))
            .debug_selector(|| "workflows-app".to_owned())
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(palette.background))
            .text_color(rgb(palette.foreground))
            .child(self.header(palette, cx))
            .child(div().flex_1().min_h_0().child(body))
    }
}
