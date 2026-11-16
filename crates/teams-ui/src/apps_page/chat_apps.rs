//! A chat's apps, as Teams' "Manage your apps" in a chat lists them: "Get more apps", Find by name, and a row per app
//! added to the chat ([`Core::chat_apps`]) with Remove, which asks "Remove <app>" / "Are you sure you want to remove
//! <app>?" first, then takes it off the chat (`DELETE beta/chats/<chat>/apps/entitlements/<app>`).

use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, AsyncApp, Context, Entity, EventEmitter, FocusHandle, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, WeakEntity,
    Window, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::FocusTrapElement as _;
use teams_proto::{StoreApp, TeamWrite};

use super::app_picture;
use crate::bridge::Core;
use crate::icons::{Glyph, icon_button, selected_by};
use crate::team_dialogs::{TeamDialogEvent, problem, surface, title, waiting};
use crate::theme::{self, Palette};

/// A chat's apps (see the module docs).
pub struct ChatAppsDialog {
    core: Arc<dyn Core>,
    chat: String,
    find: Entity<InputState>,
    apps: Option<Vec<StoreApp>>,
    removing: Option<StoreApp>,
    failed: bool,
    changed: bool,
    working: Vec<Task<()>>,
    focus: FocusHandle,
    _typed: Subscription,
}

impl EventEmitter<TeamDialogEvent> for ChatAppsDialog {}

impl ChatAppsDialog {
    /// The dialog on `chat`, its apps being read.
    pub fn new(core: Arc<dyn Core>, chat: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let find = cx.new(|cx| InputState::new(window, cx).placeholder("Find by name"));
        let typed = cx.subscribe(&find, |_, _, _: &InputEvent, cx| cx.notify());
        find.update(cx, |find, cx| find.focus(window, cx));
        let reply = core.chat_apps(chat.clone());
        let reading = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let apps = reply.await.unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                this.apps = Some(apps);
                cx.notify();
            });
        });
        Self {
            core,
            chat,
            find,
            apps: None,
            removing: None,
            failed: false,
            changed: false,
            working: vec![reading],
            focus: cx.focus_handle(),
            _typed: typed,
        }
    }

    /// The apps listed: those whose name holds Find by name's words.
    #[must_use]
    pub fn shown(&self, cx: &gpui::App) -> Vec<StoreApp> {
        let query = self.find.read(cx).value().trim().to_lowercase();
        self.apps
            .iter()
            .flatten()
            .filter(|store| query.is_empty() || store.app.name.to_lowercase().contains(&query))
            .cloned()
            .collect()
    }

    /// A row's Remove: asks first.
    pub fn ask_remove(&mut self, store: StoreApp, cx: &mut Context<Self>) {
        self.removing = Some(store);
        self.failed = false;
        cx.notify();
    }

    /// The question's Remove: the app off the chat, gone from the list once the service took it.
    pub fn remove(&mut self, cx: &mut Context<Self>) {
        let Some(store) = self.removing.clone() else {
            return;
        };
        let id = store.app.id.clone();
        let reply = self.core.team_write(
            TeamWrite::RemoveChatApp {
                chat: self.chat.clone(),
                app: id.clone(),
            },
            None,
        );
        self.working.push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = reply.await;
            let _ = this.update(cx, |this, cx| {
                if answer.is_ok() {
                    this.removing = None;
                    this.changed = true;
                    this.apps.iter_mut().for_each(|apps| apps.retain(|listed| listed.app.id != id));
                } else {
                    this.failed = true;
                }
                cx.notify();
            });
        }));
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if self.changed {
            cx.emit(TeamDialogEvent::AppsChanged { words: "App removed" });
        } else {
            cx.emit(TeamDialogEvent::Closed);
        }
    }

    fn row(&self, at: usize, store: &StoreApp, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let removed = store.clone();
        div()
            .debug_selector(move || format!("chat-apps-{at}"))
            .py_2()
            .flex()
            .items_center()
            .gap_3()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(app_picture(store, None, 36.0, palette))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::BOLD)
                            .truncate()
                            .child(store.app.name.clone()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(palette.muted_foreground))
                            .truncate()
                            .child(store.app.developer.clone()),
                    ),
            )
            .when(!self.core.read_only(), |row| {
                row.child(selected_by(
                    format!("chat-apps-{at}-remove"),
                    icon_button(SharedString::from(format!("chat-apps-{at}-remove")), Glyph::TRASH, "Remove")
                        .on_click(cx.listener(move |this, _, _, cx| this.ask_remove(removed.clone(), cx))),
                ))
            })
            .into_any_element()
    }

    fn question(&self, palette: Palette, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let store = self.removing.as_ref()?;
        let name = store.app.name.clone();
        Some(
            div()
                .debug_selector(|| "chat-apps-question".to_owned())
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .rounded_lg()
                .border_1()
                .border_color(rgb(palette.border))
                .child(div().font_weight(gpui::FontWeight::BOLD).child(format!("Remove {name}")))
                .when(self.failed, |question| {
                    question.child(problem(
                        "chat-apps-problem",
                        format!("There was an issue while trying to remove {name}. Please try again."),
                    ))
                })
                .child(format!("Are you sure you want to remove {name}?"))
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(selected_by(
                            "chat-apps-cancel",
                            Button::new("chat-apps-cancel")
                                .outline()
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.removing = None;
                                    cx.notify();
                                })),
                        ))
                        .child(selected_by(
                            "chat-apps-confirm",
                            Button::new("chat-apps-confirm")
                                .primary()
                                .label("Remove")
                                .on_click(cx.listener(|this, _, _, cx| this.remove(cx))),
                        )),
                ),
        )
    }
}

impl Render for ChatAppsDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let rows: Vec<AnyElement> = self
            .shown(cx)
            .iter()
            .enumerate()
            .map(|(at, store)| self.row(at, store, palette, cx))
            .collect();
        let list: AnyElement = match &self.apps {
            None => waiting("chat-apps-loading", "Loading…", palette).into_any_element(),
            Some(apps) if apps.is_empty() => div()
                .debug_selector(|| "chat-apps-none".to_owned())
                .py_8()
                .text_center()
                .text_color(rgb(palette.muted_foreground))
                .child("No apps have been added to this chat yet.")
                .into_any_element(),
            Some(_) if rows.is_empty() => div()
                .debug_selector(|| "chat-apps-none".to_owned())
                .py_8()
                .text_center()
                .text_color(rgb(palette.muted_foreground))
                .child("No apps match your search.")
                .into_any_element(),
            Some(_) => div()
                .id("chat-apps-list")
                .max_h(px(360.0))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .children(rows)
                .into_any_element(),
        };
        surface("chat-apps-dialog", palette, window)
            .key_context("TeamDialog")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.close(cx);
                }
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(div().flex_1().child(title("Apps in this chat")))
                    .child(selected_by(
                        "chat-apps-close",
                        icon_button("chat-apps-close", Glyph::X, "Close")
                            .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(selected_by(
                        "chat-apps-get-more",
                        Button::new("chat-apps-get-more")
                            .outline()
                            .icon(Glyph::PLUS.icon())
                            .label("Get more apps")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(TeamDialogEvent::ShowStore))),
                    ))
                    .child(div().flex_1())
                    .child(div().w(px(220.0)).child(selected_by(
                        "chat-apps-find",
                        Input::new(&self.find).suffix(Glyph::SEARCH.icon().size(px(14.0))),
                    ))),
            )
            .children(self.question(palette, cx))
            .child(list)
            .focus_trap("chat-apps-trap", &self.focus)
    }
}
