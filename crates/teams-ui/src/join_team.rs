//! "Join a team", which Create team's dropdown opens, as Teams has it (`docs/protocol/teams.md`): Back, Create team, a box
//! that finds public teams, "Join a team with a code" and a card per team the service suggests, with Join. Across both panes,
//! as in Teams. The read-only mode shows the cards and offers no joining.

use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, AsyncApp, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    WeakEntity, Window, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Disableable as _, Sizable as _};
use teams_proto::{JoinableTeam, TeamWrite};

use crate::bridge::Core;
use crate::icons::{Glyph, icon_button, selected_by};
use crate::teams_list::initials;
use crate::teams_page::centered;
use crate::theme::{self, Palette};

/// How long the box waits after the typing.
const PAUSE: Duration = Duration::from_millis(350);

/// What the page tells the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JoinTeamEvent {
    Back,
    CreateTeam,
    /// A team joined, or its owners asked.
    Joined(&'static str),
}

/// "Join a team" (see the module docs).
pub struct JoinTeam {
    core: Arc<dyn Core>,
    search: Entity<InputState>,
    code: Entity<InputState>,
    teams: Option<Vec<JoinableTeam>>,
    joining: Option<String>,
    problem: Option<&'static str>,
    reading: Option<Task<()>>,
    writing: Option<Task<()>>,
    _inputs: [Subscription; 2],
}

impl EventEmitter<JoinTeamEvent> for JoinTeam {}

impl JoinTeam {
    /// The page, its suggestions being read.
    pub fn new(core: Arc<dyn Core>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Type to search"));
        let code_box = cx.new(|cx| InputState::new(window, cx).placeholder("Enter join code."));
        let typed = cx.subscribe(&search, |this: &mut Self, search, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let text = search.read(cx).value().trim().to_owned();
                this.read((!text.is_empty()).then_some(text), cx);
            }
        });
        let code_typed = cx.subscribe(&code_box, |_, _, _: &InputEvent, cx| cx.notify());
        let mut page = Self {
            core,
            search,
            code: code_box,
            teams: None,
            joining: None,
            problem: None,
            reading: None,
            writing: None,
            _inputs: [typed, code_typed],
        };
        page.read(None, cx);
        page
    }

    fn read(&mut self, keywords: Option<String>, cx: &mut Context<Self>) {
        let core = Arc::clone(&self.core);
        self.reading = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            if keywords.is_some() {
                cx.background_executor().timer(PAUSE).await;
            }
            let found = core.joinable_teams(keywords).await.unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                this.teams = Some(found);
                cx.notify();
            });
        }));
    }

    /// Join on a team's card.
    pub fn join(&mut self, group: String, cx: &mut Context<Self>) {
        self.joining = Some(group.clone());
        self.send(TeamWrite::Join { group }, "You joined the team", cx);
    }

    /// Add team, with what the code box holds.
    pub fn join_by_code(&mut self, cx: &mut Context<Self>) {
        let code = self.code.read(cx).value().trim().to_owned();
        if !code.is_empty() {
            self.send(TeamWrite::JoinByCode { code }, "You joined the team", cx);
        }
    }

    fn send(&mut self, write: TeamWrite, words: &'static str, cx: &mut Context<Self>) {
        self.problem = None;
        cx.notify();
        let reply = self.core.team_write(write, None);
        self.writing = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = reply.await;
            let _ = this.update(cx, |this, cx| {
                this.writing = None;
                this.joining = None;
                if answer.is_ok() {
                    cx.emit(JoinTeamEvent::Joined(words));
                } else {
                    this.problem = Some("Something didn't work. Please try joining the team later.");
                }
                cx.notify();
            });
        }));
    }

    fn card(&self, at: usize, team: &JoinableTeam, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let group = team.group.clone();
        let joining = self.joining.as_deref() == Some(group.as_str());
        let can_join = !self.core.read_only();
        card_frame(format!("join-card-{at}"), palette)
            .child(card_badge(initials(&team.name), palette))
            .child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .truncate()
                    .child(team.name.clone()),
            )
            .child(div().text_sm().child(if team.public { "Public" } else { "Private" }))
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .overflow_hidden()
                    .text_color(rgb(palette.muted_foreground))
                    .child(team.description.clone()),
            )
            .when(can_join, |card| {
                card.child(selected_by(
                    format!("join-card-join-{at}"),
                    Button::new(SharedString::from(format!("join-card-join-{at}")))
                        .primary()
                        .small()
                        .label(if team.public { "Join team" } else { "Request to join" })
                        .disabled(joining)
                        .on_click(cx.listener(move |this, _, _, cx| this.join(group.clone(), cx))),
                ))
            })
            .into_any_element()
    }
}

fn card_frame(id: String, palette: Palette) -> gpui::Div {
    div()
        .debug_selector(move || id.clone())
        .w(px(240.0))
        .h(px(270.0))
        .p_4()
        .flex()
        .flex_col()
        .items_center()
        .gap_2()
        .rounded_lg()
        .border_1()
        .border_color(rgb(palette.border))
        .bg(rgb(palette.surface))
}

fn card_badge(face: String, palette: Palette) -> gpui::Div {
    div()
        .size(px(64.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_lg()
        .text_xl()
        .bg(rgb(palette.brand))
        .text_color(rgb(palette.on_brand))
        .child(face)
}

impl JoinTeam {
    /// The page's own column: Back, its title, Create team and the search box.
    fn side_panel(&self, can_join: bool, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .w(px(320.0))
            .flex_none()
            .h_full()
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .border_r_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(selected_by(
                        "join-back",
                        icon_button("join-back", Glyph::CHEVRON_LEFT, "Back")
                            .small()
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(JoinTeamEvent::Back))),
                    ))
                    .child(
                        div()
                            .flex_1()
                            .text_lg()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child("Join a team"),
                    )
                    .when(can_join, |row| {
                        row.child(selected_by(
                            "join-create",
                            icon_button("join-create", Glyph::PLUS, "Create team")
                                .small()
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(JoinTeamEvent::CreateTeam))),
                        ))
                    }),
            )
            .child(
                div()
                    .debug_selector(|| "join-search".to_owned())
                    .child(Input::new(&self.search)),
            )
            .child(
                div()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(palette.selected))
                    .child("Teams for you"),
            )
    }
}

impl Render for JoinTeam {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let can_join = !self.core.read_only();
        let code_card = can_join.then(|| {
            let empty = self.code.read(cx).value().trim().is_empty();
            card_frame("join-code-card".to_owned(), palette)
                .child(card_badge("#".to_owned(), palette))
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Join a team with a code"),
                )
                .child(div().w_full().child(Input::new(&self.code)))
                .child(div().flex_1())
                .child(selected_by(
                    "join-code-add",
                    Button::new("join-code-add")
                        .primary()
                        .label("Add team")
                        .disabled(empty)
                        .on_click(cx.listener(|this, _, _, cx| this.join_by_code(cx))),
                ))
        });
        let cards: Vec<AnyElement> = self
            .teams
            .clone()
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(at, team)| self.card(at, team, palette, cx))
            .collect();
        let empty = self.teams.as_ref().is_some_and(Vec::is_empty) && !can_join;
        let loading = self.teams.is_none();
        div()
            .debug_selector(|| "join-team".to_owned())
            .size_full()
            .flex()
            .bg(rgb(palette.background))
            .text_color(rgb(palette.foreground))
            .child(self.side_panel(can_join, palette, cx))
            .child(
                div()
                    .id("join-cards")
                    .debug_selector(|| "join-cards".to_owned())
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .p_5()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .children(self.problem.map(|words| {
                        div()
                            .debug_selector(|| "join-problem".to_owned())
                            .text_sm()
                            .text_color(rgb(theme::DANGER))
                            .child(words)
                    }))
                    .child(div().flex().flex_wrap().gap_4().children(code_card).children(cards))
                    .when(loading, |page| page.child(centered("Loading…", palette)))
                    .when(empty, |page| page.child(centered("No teams to show", palette))),
            )
    }
}
