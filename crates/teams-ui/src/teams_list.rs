//! The Teams list (M2 Task 16): the account's teams, each with its channels (its general one first), in the list pane
//! when the rail chooses Teams.
//!
//! - **From the store only.** The list asks the engine for the stored teams ([`Core::teams`]) when it opens and again
//!   each time the models say they changed ([`crate::Chats::teams_at`]: `Event::Teams`); the engine reads them from
//!   CSA in the background when they are missing or old. Before a read settled them it says so ("Loading teams…"),
//!   or, when the last read told failed ([`crate::Chats::teams_failed_at`]: `Event::Failed` during
//!   [`teams_core::READING_TEAMS`]), that the teams can't be read; the engine tries again when the list asks after
//!   its retry wait (the rail chosen again).
//! - **Read-only.** A channel clicked is told as [`TeamsListEvent::Opened`]; the shell shows its posts in a
//!   [`crate::ChannelView`], which offers no composer.
//!
//! What the list shows is traced (`teams-list shown`: the channels' ids in order, each `general` or `channel`, never a
//! name).

use std::sync::Arc;

use gpui::{
    AnyElement, App, AsyncApp, Context, ElementId, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    WeakEntity, Window, div, px, rgb,
};
use teams_core::EngineError;
use teams_store::Team;

use crate::bridge::Core;
use crate::models::Models;
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};

/// A channel row's height, px.
pub const CHANNEL_HEIGHT: f32 = 32.0;
/// What the list says before the teams were read.
pub const LOADING: &str = "Loading teams…";
/// What the list says when the account is in no team.
pub const NO_TEAMS: &str = "No teams to show";
/// What the list says when no teams are stored and the last read of them failed.
pub const UNREADABLE: &str = "Teams can't be read right now";

/// What the Teams list tells the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TeamsListEvent {
    /// A channel was chosen.
    Opened(ChannelChosen),
}

/// A channel chosen, with the names its header shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelChosen {
    pub id: String,
    pub name: SharedString,
    pub team: SharedString,
}

/// The Teams list view (see the module docs).
pub struct TeamsList {
    core: Arc<dyn Core>,
    trace: Arc<ViewTrace>,
    models: Models,
    /// The teams as last read from the store; `None` before a read settled them.
    teams: Option<Vec<Team>>,
    selected: Option<String>,
    /// The models' teams revision the last load started at.
    read_at: u64,
    /// The models' last failed teams read the list has shown.
    failed_seen: u64,
    loading: Option<Task<()>>,
    /// A change came while a load ran: another follows it.
    dirty: bool,
    loads: u64,
    traced: Option<Vec<(String, bool)>>,
    _observing: Subscription,
}

impl EventEmitter<TeamsListEvent> for TeamsList {}

impl TeamsList {
    /// The list over the store, loaded at once.
    pub fn new(core: Arc<dyn Core>, models: Models, trace: Arc<ViewTrace>, cx: &mut Context<Self>) -> Self {
        let observing = cx.observe(&models.chats, |this: &mut Self, chats, cx| {
            let (teams_at, failed_at) = {
                let chats = chats.read(cx);
                (chats.teams_at(), chats.teams_failed_at())
            };
            if teams_at != this.read_at {
                this.reload(cx);
            } else if failed_at != this.failed_seen {
                this.failed_seen = failed_at;
                cx.notify();
            }
        });
        let mut list = Self {
            core,
            trace,
            models,
            teams: None,
            selected: None,
            read_at: 0,
            failed_seen: 0,
            loading: None,
            dirty: false,
            loads: 0,
            traced: None,
            _observing: observing,
        };
        list.reload(cx);
        list
    }

    /// The teams shown; `None` before a read settled them.
    #[must_use]
    pub fn teams(&self) -> Option<&[Team]> {
        self.teams.as_deref()
    }

    /// Whether the list says the teams can't be read: none are stored, and the last read told failed.
    #[must_use]
    pub fn unreadable(&self, cx: &App) -> bool {
        let chats = self.models.chats.read(cx);
        self.teams.is_none() && chats.teams_failed_at() > chats.teams_at()
    }

    /// The channel highlighted.
    #[must_use]
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// How many loads from the store have ended.
    #[must_use]
    pub fn loads(&self) -> u64 {
        self.loads
    }

    /// Highlights a channel (the one the shell shows), telling nobody.
    pub fn select(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        if self.selected != id {
            self.selected = id;
            cx.notify();
        }
    }

    /// The channel `id` with its names, when the list shows it.
    #[must_use]
    pub fn chosen(&self, id: &str) -> Option<ChannelChosen> {
        self.teams.iter().flatten().find_map(|team| {
            team.channels
                .iter()
                .find(|channel| channel.id == id)
                .map(|channel| ChannelChosen {
                    id: channel.id.clone(),
                    name: channel.name.clone().into(),
                    team: team.name.clone().into(),
                })
        })
    }

    /// Opens the channel `id`, when the list shows it: highlighted, and told to the shell.
    pub fn open(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(chosen) = self.chosen(id) {
            self.select(Some(chosen.id.clone()), cx);
            cx.emit(TeamsListEvent::Opened(chosen));
        }
    }

    /// Reads the teams from the store again (or once the running load ends); the engine reads them from the service
    /// when they are missing or old.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        if self.loading.is_some() {
            self.dirty = true;
            return;
        }
        self.dirty = false;
        self.read_at = self.models.chats.read(cx).teams_at();
        let core = Arc::clone(&self.core);
        self.loading = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let teams = core.teams().await;
            let _ = this.update(cx, |list, cx| list.loaded(teams, cx));
        }));
    }

    /// A load ended: its teams shown (a stopped engine's keep what is shown).
    fn loaded(&mut self, teams: Result<Option<Vec<Team>>, EngineError>, cx: &mut Context<Self>) {
        self.loading = None;
        self.loads += 1;
        if let Ok(teams) = teams {
            self.teams = teams;
            self.trace_channels();
        }
        cx.notify();
        if self.dirty {
            self.reload(cx);
        }
    }

    /// Traces the channels shown, when they changed.
    fn trace_channels(&mut self) {
        let shown: Vec<(String, bool)> = self
            .teams
            .iter()
            .flatten()
            .flat_map(|team| {
                team.channels
                    .iter()
                    .map(|channel| (channel.id.clone(), channel.general))
            })
            .collect();
        if self.traced.as_ref() == Some(&shown) {
            return;
        }
        let mut line = TraceLine::new("teams-list", "shown").conversations(shown.iter().map(|(id, _)| id.as_str()));
        for (_, general) in &shown {
            line = line.kind(if *general { "general" } else { "channel" });
        }
        self.trace.record(&line);
        self.traced = Some(shown);
    }

    /// A team's name line, its test selector `team-<index>`.
    fn team_header(index: usize, team: &Team, palette: Palette) -> AnyElement {
        let name = if team.archived {
            format!("{} (archived)", team.name)
        } else {
            team.name.clone()
        };
        div()
            .debug_selector(move || format!("team-{index}"))
            .px_3()
            .pt_2()
            .pb_1()
            .truncate()
            .font_weight(gpui::FontWeight::BOLD)
            .text_color(rgb(palette.foreground))
            .child(name)
            .into_any_element()
    }

    /// A channel's row, its test selector `channel-row-<index>` (counted across the teams).
    fn channel_row(&self, index: usize, id: &str, name: &str, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.selected.as_deref() == Some(id);
        let background = if selected { palette.selected } else { palette.surface };
        let opened = id.to_owned();
        div()
            .id(ElementId::Name(format!("channel-{id}").into()))
            .debug_selector(move || format!("channel-row-{index}"))
            .w_full()
            .h(px(CHANNEL_HEIGHT))
            .pl_6()
            .pr_3()
            .flex()
            .items_center()
            .truncate()
            .text_sm()
            .bg(rgb(background))
            .hover(move |style| style.bg(rgb(if selected { palette.selected } else { palette.hover })))
            .on_click(cx.listener(move |this, _, _, cx| this.open(&opened, cx)))
            .child(name.to_owned())
            .into_any_element()
    }

    fn rows(&self, palette: Palette, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut rows = Vec::new();
        let mut index = 0;
        for (team_index, team) in self.teams.iter().flatten().enumerate() {
            rows.push(Self::team_header(team_index, team, palette));
            for channel in &team.channels {
                rows.push(self.channel_row(index, &channel.id, &channel.name, palette, cx));
                index += 1;
            }
        }
        rows
    }
}

impl Render for TeamsList {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let note = |text: &'static str| {
            div()
                .debug_selector(|| "teams-note".to_owned())
                .p_3()
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .child(text)
        };
        let unreadable = self.unreadable(cx);
        let body = match &self.teams {
            None if unreadable => note(UNREADABLE).into_any_element(),
            None => note(LOADING).into_any_element(),
            Some(teams) if teams.is_empty() => note(NO_TEAMS).into_any_element(),
            Some(_) => div()
                .id("teams-rows")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .children(self.rows(palette, cx))
                .into_any_element(),
        };
        div()
            .debug_selector(|| "teams-list".to_owned())
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(palette.surface))
            .child(body)
    }
}
