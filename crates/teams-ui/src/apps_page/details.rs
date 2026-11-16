//! An app's details, as Teams' store dialog shows them: its picture, name and maker; Add (for the account itself),
//! or Open once added when its bot's chat is listed; "Add to a chat" and "Add to a team" when its definition allows
//! them; its rating; what it is; "Capabilities in Teams"; "Permissions overview"; its details; and "By using <app>,
//! you agree to the privacy policy, terms of use, and permissions." with its links.

use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AsyncApp, Context, EventEmitter, FocusHandle, InteractiveElement as _, IntoElement, KeyDownEvent,
    ParentElement as _, Render, RenderImage, SharedString, StatefulInteractiveElement as _, Styled as _, Task,
    WeakEntity, Window, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Disableable as _, FocusTrapElement as _};
use teams_proto::{AppCapability, Rating, StoreApp, TeamWrite};

use super::{app_picture, ratings_count, stars};
use crate::bridge::Core;
use crate::icons::{Glyph, icon_button, selected_by};
use crate::team_dialogs::{TeamDialogEvent, problem};
use crate::theme::{self, Palette};

/// What the dialog knows of the app beyond its definition.
#[derive(Clone, Debug, Default)]
pub struct DetailsContext {
    /// The store's rating of it.
    pub rating: Option<Rating>,
    /// Whether the account added it for itself.
    pub added: bool,
    /// The chat with its bot, when the chat list has one: Open opens it.
    pub bot_chat: Option<String>,
    /// Its picture, once drawable.
    pub picture: Option<Arc<RenderImage>>,
}

/// An app's details (see the module docs).
pub struct AppDetailsDialog {
    core: Arc<dyn Core>,
    store: StoreApp,
    known: DetailsContext,
    adding: bool,
    /// Whether an Add went through while the dialog was open.
    changed: bool,
    failed: bool,
    working: Option<Task<()>>,
    focus: FocusHandle,
}

impl EventEmitter<TeamDialogEvent> for AppDetailsDialog {}

impl AppDetailsDialog {
    /// The dialog on `store`.
    pub fn new(
        core: Arc<dyn Core>,
        store: StoreApp,
        known: DetailsContext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        Self {
            core,
            store,
            known,
            adding: false,
            changed: false,
            failed: false,
            working: None,
            focus,
        }
    }

    /// Whether the account has the app now.
    #[must_use]
    pub fn added(&self) -> bool {
        self.known.added
    }

    /// Add: the app for the account itself.
    pub fn add(&mut self, cx: &mut Context<Self>) {
        if self.core.read_only() || self.adding {
            return;
        }
        self.adding = true;
        self.failed = false;
        cx.notify();
        let reply = self.core.team_write(
            TeamWrite::AddPersonalApp {
                app: self.store.definition.clone(),
            },
            None,
        );
        self.working = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = reply.await;
            let _ = this.update(cx, |this, cx| {
                this.adding = false;
                if answer.is_ok() {
                    this.known.added = true;
                    this.changed = true;
                } else {
                    this.failed = true;
                }
                cx.notify();
            });
        }));
    }

    /// Open: the chat with its bot.
    pub fn open(&mut self, cx: &mut Context<Self>) {
        if let Some(chat) = self.known.bot_chat.clone() {
            cx.emit(TeamDialogEvent::OpenChat(chat));
        }
    }

    /// "Add to a chat" (`team` false) or "Add to a team": the place's picker in the dialog's place. Not while an Add
    /// is in flight: the picker drops the dialog and its reply, and the store would not read the account's apps again.
    pub fn add_to(&mut self, team: bool, cx: &mut Context<Self>) {
        if self.adding {
            return;
        }
        cx.emit(TeamDialogEvent::AddAppTo {
            app: Box::new(self.store.clone()),
            team,
            changed: self.changed,
        });
    }

    /// The dialog closes, the store reading the account's apps again when one was added. Not while an Add is in
    /// flight: the close drops the dialog and its reply, and the store would not read the account's apps again.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.adding {
            return;
        }
        if self.changed {
            cx.emit(TeamDialogEvent::AppsChanged { words: "App added" });
        } else {
            cx.emit(TeamDialogEvent::Closed);
        }
    }

    fn actions(&self, cx: &mut Context<Self>) -> gpui::Div {
        let places = self.store.definition.places();
        let read_only = self.core.read_only();
        let mut actions = div().flex().items_center().gap_2();
        if self.known.added && self.known.bot_chat.is_some() {
            actions = actions.child(selected_by(
                "app-details-open",
                Button::new("app-details-open")
                    .primary()
                    .label("Open")
                    .on_click(cx.listener(|this, _, _, cx| this.open(cx))),
            ));
        } else if self.known.added {
            actions = actions.child(selected_by(
                "app-details-added",
                Button::new("app-details-added")
                    .outline()
                    .icon(Glyph::CHECK.icon())
                    .label("Added")
                    .disabled(true),
            ));
        } else if places.personal && !read_only {
            actions = actions.child(selected_by(
                "app-details-add",
                Button::new("app-details-add")
                    .primary()
                    .label(if self.adding { "Adding…" } else { "Add" })
                    .disabled(self.adding)
                    .on_click(cx.listener(|this, _, _, cx| this.add(cx))),
            ));
        }
        if !read_only {
            if places.chat {
                actions = actions.child(selected_by(
                    "app-details-add-chat",
                    Button::new("app-details-add-chat")
                        .outline()
                        .label("Add to a chat")
                        .disabled(self.adding)
                        .on_click(cx.listener(|this, _, _, cx| this.add_to(false, cx))),
                ));
            }
            if places.team {
                actions = actions.child(selected_by(
                    "app-details-add-team",
                    Button::new("app-details-add-team")
                        .outline()
                        .label("Add to a team")
                        .disabled(self.adding)
                        .on_click(cx.listener(|this, _, _, cx| this.add_to(true, cx))),
                ));
            }
        }
        actions
    }

    fn head(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .flex_none()
            .flex()
            .items_start()
            .gap_4()
            .child(app_picture(&self.store, self.known.picture.clone(), 64.0, palette))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(gpui::FontWeight::BOLD)
                            .debug_selector(|| "app-details-name".to_owned())
                            .child(self.store.app.name.clone()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(palette.muted_foreground))
                            .child(self.store.app.developer.clone()),
                    )
                    .child(div().pt_2().child(self.actions(cx)))
                    .when(self.failed, |head| {
                        head.child(problem(
                            "app-details-problem",
                            "The app wasn't added. Please try again.",
                        ))
                    }),
            )
            .child(selected_by(
                "app-details-close",
                icon_button("app-details-close", Glyph::X, "Close")
                    .disabled(self.adding)
                    .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
            ))
    }

    fn stats(&self, palette: Palette) -> Option<gpui::Div> {
        let rating = self.known.rating.filter(|rating| rating.count > 0)?;
        let stat = |big: String, small: String| {
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .child(div().font_weight(gpui::FontWeight::BOLD).child(big))
                .child(div().text_xs().text_color(rgb(palette.muted_foreground)).child(small))
        };
        Some(
            div()
                .debug_selector(|| "app-details-stats".to_owned())
                .py_3()
                .flex()
                .border_t_1()
                .border_b_1()
                .border_color(rgb(palette.border))
                .child(stat(format!("★ {}", stars(rating)), ratings_count(rating.count)))
                .child(stat("Works in".to_owned(), "Teams".to_owned())),
        )
    }

    fn capabilities(&self, palette: Palette) -> Option<gpui::Div> {
        let capabilities = self.store.definition.capabilities();
        if capabilities.is_empty() {
            return None;
        }
        let tiles = capabilities.into_iter().map(|capability| {
            let glyph = match capability {
                AppCapability::Bot => Glyph::BOT,
                AppCapability::Messaging => Glyph::MESSAGE,
                AppCapability::PersonalApp => Glyph::USER,
                AppCapability::Tab => Glyph::FILE,
            };
            div()
                .w(px(230.0))
                .flex()
                .gap_2()
                .child(glyph.icon().size(px(16.0)).flex_none())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().font_weight(gpui::FontWeight::BOLD).child(capability.name()))
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(palette.muted_foreground))
                                .child(capability.line()),
                        ),
                )
        });
        Some(
            div()
                .debug_selector(|| "app-details-capabilities".to_owned())
                .flex()
                .flex_col()
                .gap_2()
                .child(heading("Capabilities in Teams"))
                .child(
                    div()
                        .p_4()
                        .flex()
                        .flex_wrap()
                        .gap_4()
                        .rounded_lg()
                        .bg(rgb(palette.hover))
                        .children(tiles),
                ),
        )
    }

    fn permissions(&self, palette: Palette) -> Option<gpui::Div> {
        let overview = self.store.definition.permission_overview();
        if overview.reads.is_empty() && overview.actions.is_empty() {
            return None;
        }
        let list = |title: &'static str, items: &[&'static str]| {
            (!items.is_empty()).then(|| {
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
                    .children(items.iter().map(|item| {
                        div()
                            .pl_2()
                            .flex()
                            .gap_2()
                            .text_sm()
                            .text_color(rgb(palette.muted_foreground))
                            .child("•")
                            .child(*item)
                    }))
            })
        };
        Some(
            div()
                .debug_selector(|| "app-details-permissions".to_owned())
                .flex()
                .flex_col()
                .gap_2()
                .child(heading("Permissions overview"))
                .children(list("Permission to read and access:", &overview.reads))
                .children(list("Permission to perform these actions:", &overview.actions)),
        )
    }

    fn facts(&self, palette: Palette) -> gpui::Div {
        let definition = &self.store.definition;
        let fact = |name: &'static str, value: String| {
            div()
                .flex()
                .gap_4()
                .text_sm()
                .child(div().w(px(120.0)).text_color(rgb(palette.muted_foreground)).child(name))
                .child(div().flex_1().child(value))
        };
        let languages = match definition.language_count() {
            0 => None,
            1 => Some("1 language".to_owned()),
            many => Some(format!("{many} languages")),
        };
        div()
            .debug_selector(|| "app-details-facts".to_owned())
            .flex()
            .flex_col()
            .gap_1()
            .child(heading("Details"))
            .child(fact("Created by", self.store.app.developer.clone()))
            .children(definition.version().map(|version| fact("Version", version.to_owned())))
            .children(languages.map(|languages| fact("Languages", languages)))
    }

    fn terms(&self, palette: Palette) -> gpui::Div {
        let definition = &self.store.definition;
        let link = |id: &'static str, words: &'static str, url: Option<&str>| -> AnyElement {
            match url {
                Some(url) => {
                    let url = url.to_owned();
                    div()
                        .id(id)
                        .debug_selector(move || id.to_owned())
                        .text_color(rgb(palette.brand))
                        .underline()
                        .cursor_pointer()
                        .on_click(move |_, _, cx| cx.open_url(&url))
                        .child(words)
                        .into_any_element()
                }
                None => div().child(words).into_any_element(),
            }
        };
        div()
            .debug_selector(|| "app-details-terms".to_owned())
            .flex_none()
            .pt_3()
            .border_t_1()
            .border_color(rgb(palette.border))
            .flex()
            .flex_wrap()
            .text_sm()
            .child(format!("By using {}, you agree to the\u{a0}", self.store.app.name))
            .child(link("app-details-privacy", "privacy policy", definition.privacy_url()))
            .child(",\u{a0}")
            .child(link("app-details-terms-of-use", "terms of use", definition.terms_url()))
            .child(", and permissions.")
    }
}

fn heading(words: &'static str) -> gpui::Div {
    div().text_lg().font_weight(gpui::FontWeight::BOLD).child(words)
}

impl Render for AppDetailsDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let definition = &self.store.definition;
        let about = div()
            .flex()
            .flex_col()
            .gap_1()
            .children(definition.short_description().map(|line| {
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(SharedString::from(line.to_owned()))
            }))
            .children(definition.long_description().map(|text| {
                div()
                    .text_sm()
                    .debug_selector(|| "app-details-about".to_owned())
                    .child(SharedString::from(text.to_owned()))
            }));
        let body = div()
            .id("app-details-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_4()
            .children(self.stats(palette))
            .child(about)
            .children(self.capabilities(palette))
            .children(self.permissions(palette))
            .child(self.facts(palette));
        crate::team_dialogs::surface("app-details", palette, window)
            .w(px(820.0))
            .key_context("TeamDialog")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.close(cx);
                }
            }))
            .child(self.head(palette, cx))
            .child(body)
            .child(self.terms(palette))
            .focus_trap("app-details-trap", &self.focus)
    }
}
