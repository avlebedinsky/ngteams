//! "Add <app> to a chat" and "Add <app> to a team": "Choose a place to use the app", a search over the account's chats
//! or teams, one picked, then Go, which adds the app there (`POST beta/chats/<chat>/apps/entitlements` or
//! `beta/teams/<team>/apps/entitlements` with its definition).

use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, AsyncApp, Context, Entity, EventEmitter, FocusHandle, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, WeakEntity, Window, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Disableable as _, FocusTrapElement as _};
use teams_proto::{StoreApp, TeamWrite};

use crate::bridge::Core;
use crate::icons::{Glyph, selected_by};
use crate::team_dialogs::{TeamDialogEvent, buttons, problem, surface, title};
use crate::theme::{self, Palette};

/// A place an app can go: its id (a chat's or a team's thread) and its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    pub id: String,
    pub name: String,
}

/// How many places the list shows.
const MOST: usize = 50;

/// The picker (see the module docs).
pub struct AddToPlaceDialog {
    core: Arc<dyn Core>,
    store: StoreApp,
    team: bool,
    places: Vec<Place>,
    search: Entity<InputState>,
    chosen: Option<String>,
    adding: bool,
    failed: bool,
    working: Option<Task<()>>,
    focus: FocusHandle,
    _typed: Subscription,
}

impl EventEmitter<TeamDialogEvent> for AddToPlaceDialog {}

impl AddToPlaceDialog {
    /// The picker of `places` (the account's teams when `team`, else its chats) for `store`.
    pub fn new(
        core: Arc<dyn Core>,
        store: StoreApp,
        team: bool,
        places: Vec<Place>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let words = if team { "Search for a team" } else { "Search for a chat" };
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(words));
        let typed = cx.subscribe(&search, |_, _, _: &InputEvent, cx| cx.notify());
        search.update(cx, |search, cx| search.focus(window, cx));
        Self {
            core,
            store,
            team,
            places,
            search,
            chosen: None,
            adding: false,
            failed: false,
            working: None,
            focus: cx.focus_handle(),
            _typed: typed,
        }
    }

    /// The places listed: those whose name holds the search's words.
    #[must_use]
    pub fn shown(&self, cx: &gpui::App) -> Vec<Place> {
        let query = self.search.read(cx).value().trim().to_lowercase();
        self.places
            .iter()
            .filter(|place| query.is_empty() || place.name.to_lowercase().contains(&query))
            .take(MOST)
            .cloned()
            .collect()
    }

    /// A place picked.
    pub fn choose(&mut self, id: String, cx: &mut Context<Self>) {
        self.chosen = Some(id);
        cx.notify();
    }

    /// Go: the app added to the place picked.
    pub fn go(&mut self, cx: &mut Context<Self>) {
        let Some(place) = self.chosen.clone() else {
            return;
        };
        if self.core.read_only() || self.adding {
            return;
        }
        self.adding = true;
        self.failed = false;
        cx.notify();
        let app = self.store.definition.clone();
        let write = if self.team {
            TeamWrite::AddApp { team: place, app }
        } else {
            TeamWrite::AddChatApp { chat: place, app }
        };
        let words = if self.team {
            "App added to the team"
        } else {
            "App added to the chat"
        };
        let reply = self.core.team_write(write, None);
        self.working = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = reply.await;
            let _ = this.update(cx, |this, cx| {
                this.adding = false;
                if answer.is_ok() {
                    cx.emit(TeamDialogEvent::AppsChanged { words });
                } else {
                    this.failed = true;
                }
                cx.notify();
            });
        }));
    }

    fn row(&self, at: usize, place: &Place, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let chosen = self.chosen.as_deref() == Some(place.id.as_str());
        let id = place.id.clone();
        let selector = format!("app-place-{at}");
        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .px_3()
            .py_2()
            .flex()
            .items_center()
            .gap_3()
            .rounded_md()
            .cursor_pointer()
            .when(chosen, |row| row.bg(rgb(palette.selected)))
            .hover(move |row| row.bg(rgb(palette.hover)))
            .on_click(cx.listener(move |this, _, _, cx| this.choose(id.clone(), cx)))
            .child(if self.team { Glyph::USERS } else { Glyph::CHAT }.icon().size(px(16.0)))
            .child(div().flex_1().min_w_0().truncate().child(place.name.clone()))
            .when(chosen, |row| row.child(Glyph::CHECK.icon().size(px(14.0))))
            .into_any_element()
    }
}

impl Render for AddToPlaceDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let rows: Vec<AnyElement> = self
            .shown(cx)
            .iter()
            .enumerate()
            .map(|(at, place)| self.row(at, place, palette, cx))
            .collect();
        let list: AnyElement = if rows.is_empty() {
            div()
                .debug_selector(|| "app-place-none".to_owned())
                .py_8()
                .text_center()
                .text_color(rgb(palette.muted_foreground))
                .child(if self.team {
                    "No teams match your search."
                } else {
                    "No chats match your search."
                })
                .into_any_element()
        } else {
            div()
                .id("app-place-list")
                .max_h(px(320.0))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .children(rows)
                .into_any_element()
        };
        let place = if self.team { "team" } else { "chat" };
        surface("app-place-dialog", palette, window)
            .key_context("TeamDialog")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|_, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    cx.emit(TeamDialogEvent::Closed);
                }
            }))
            .child(title(format!("Add {} to a {place}", self.store.app.name)))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(palette.muted_foreground))
                    .child("Choose a place to use the app"),
            )
            .child(selected_by(
                "app-place-search",
                Input::new(&self.search).prefix(Glyph::SEARCH.icon().size(px(14.0))),
            ))
            .child(list)
            .when(self.failed, |dialog| {
                dialog.child(problem("app-place-problem", "The app wasn't added. Please try again."))
            })
            .child(
                buttons()
                    .child(selected_by(
                        "app-place-cancel",
                        Button::new("app-place-cancel")
                            .outline()
                            .label("Cancel")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(TeamDialogEvent::Closed))),
                    ))
                    .child(selected_by(
                        "app-place-go",
                        Button::new("app-place-go")
                            .primary()
                            .label(if self.adding { "Adding…" } else { "Go" })
                            .disabled(self.chosen.is_none() || self.adding || self.core.read_only())
                            .on_click(cx.listener(|this, _, _, cx| this.go(cx))),
                    )),
            )
            .focus_trap("app-place-trap", &self.focus)
    }
}
