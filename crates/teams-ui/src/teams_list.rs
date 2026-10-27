//! The Teams list (M2 Task 16): the account's teams, each with its channels (its general one first), in the
//! [`crate::sidebar`]'s "Teams and channels" section, which draws it.
//!
//! - **From the store only.** The list asks the engine for the stored teams ([`Core::teams`]) when it opens and again
//!   each time the models say they changed ([`crate::Chats::teams_at`]: `Event::Teams`); the engine reads them from
//!   CSA in the background when they are missing or old. Before a read settled them it says so ("Loading teams…"),
//!   or, when the last read told failed ([`crate::Chats::teams_failed_at`]: `Event::Failed` during
//!   [`teams_core::READING_TEAMS`]), that the teams can't be read; the engine tries again when the list asks after
//!   its retry wait (after a catch-up, or the section unfolded again).
//! - **Read-only.** A channel clicked is told as [`TeamsListEvent::Opened`]; the shell shows its posts in a
//!   [`crate::ChannelView`], which offers no composer.
//!
//! What the list shows is traced (`teams-list shown`: the channels' ids in order, each `general` or `channel`, never a
//! name).

use std::sync::Arc;

use gpui::{
    AnyElement, App, AsyncApp, Context, ElementId, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity,
    div, px, rgb,
};
use teams_core::EngineError;
use teams_store::Team;

use crate::bridge::Core;
use crate::models::Models;
use crate::theme::Palette;
use crate::trace::{TraceLine, ViewTrace};

/// A channel row's height, px.
pub const CHANNEL_HEIGHT: f32 = 32.0;
/// How far a channel's name is set in, px: under its team's name (past the team's fold mark and initial).
pub const CHANNEL_INDENT: f32 = 56.0;
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
    /// The models' catch-ups the list has asked again after.
    catch_ups: u64,
    _observing: [Subscription; 2],
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
        // a catch-up (the push channel back): the teams asked for again, read again when old
        let caught_up = cx.observe(&models.status, |this: &mut Self, status, cx| {
            let catch_ups = status.read(cx).catch_ups;
            if catch_ups != this.catch_ups {
                this.catch_ups = catch_ups;
                this.reload(cx);
            }
        });
        let catch_ups = models.status.read(cx).catch_ups;
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
            catch_ups,
            _observing: [observing, caught_up],
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

    /// The channel at `channel` of the team at `team`, as the sidebar shows it: its row, its test selector
    /// `channel-row-<index>` (`index` counts the channels across the teams).
    pub(crate) fn channel_row(
        &self,
        index: usize,
        team: usize,
        channel: usize,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(shown) = self
            .teams
            .as_deref()
            .and_then(|teams| teams.get(team))
            .and_then(|team| team.channels.get(channel))
        else {
            return div().into_any_element();
        };
        let selected = self.selected.as_deref() == Some(shown.id.as_str());
        let background = if selected { palette.selected } else { palette.surface };
        let opened = shown.id.clone();
        div()
            .id(ElementId::Name(format!("channel-{}", shown.id).into()))
            .debug_selector(move || format!("channel-row-{index}"))
            .w_full()
            .h(px(CHANNEL_HEIGHT))
            .pl(px(CHANNEL_INDENT))
            .pr_3()
            .flex()
            .items_center()
            .truncate()
            .text_sm()
            .bg(rgb(background))
            .hover(move |style| style.bg(rgb(if selected { palette.selected } else { palette.hover })))
            .on_click(cx.listener(move |this, _, _, cx| this.open(&opened, cx)))
            .child(shown.name.clone())
            .into_any_element()
    }
}
