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
//!   [`crate::ChannelView`], which offers no composer. A private channel's row carries Teams' lock.
//! - **A new channel.** A team's "Add channel" ([`crate::sidebar`]'s menu) is told as [`TeamsListEvent::AddChannel`]; the
//!   shell shows the [`crate::channel_dialog`].
//!
//! What the list shows is traced (`teams-list shown`: the channels' ids in order, each `general` or `channel`, never a
//! name).

use std::collections::HashMap;
use std::sync::Arc;

use crate::icons::{icon_button, selected_by};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AsyncApp, Context, DismissEvent, ElementId, Entity, EventEmitter, Focusable as _, Image,
    ImageFormat, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity, Window, div, px, rgb,
};
use gpui_component::{Sizable as _, menu::PopupMenu};
use teams_core::EngineError;
use teams_proto::{PastedFormat, TeamPicture};
use teams_store::{ChannelKind, Team, TeamChannel};

use crate::bridge::Core;
use crate::icons::Glyph;
use crate::models::Models;
use crate::team_menu::TeamChoice;
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
    /// A team's "Add channel" was chosen.
    AddChannel(Team),
    /// Another entry of a team's "..." menu was chosen.
    Chosen(Team, TeamChoice),
    ChannelChosen(Team, TeamChannel, crate::channel_menu::ChannelChoice),
    /// "See all your teams".
    SeeAllTeams,
    SectionFailed,
    /// A team's "See all channels".
    SeeAllChannels(String),
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
    sections: Vec<teams_proto::ChannelSection>,
    section_read: Option<Task<()>>,
    channel_menu: Option<(String, Entity<PopupMenu>)>,
    menu_dismiss: Option<Subscription>,
    section_pending: std::collections::HashSet<String>,
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
    /// The teams' pictures as read, by team; a team asked for and not read has `None`.
    pictures: HashMap<String, Option<Arc<Image>>>,
    picture_reads: Vec<Task<()>>,
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
            sections: Vec::new(),
            section_read: None,
            channel_menu: None,
            menu_dismiss: None,
            section_pending: std::collections::HashSet::new(),
            read_at: 0,
            failed_seen: 0,
            loading: None,
            dirty: false,
            loads: 0,
            traced: None,
            catch_ups,
            pictures: HashMap::new(),
            picture_reads: Vec::new(),
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

    /// A channel's location, retaining the stable selector index used in the team tree.
    pub(crate) fn channel_location(&self, id: &str) -> Option<(usize, usize, usize)> {
        let mut index = 0;
        for (team_at, team) in self.teams.iter().flatten().enumerate() {
            for (channel_at, channel) in team.channels.iter().enumerate() {
                if channel.id == id {
                    return Some((index, team_at, channel_at));
                }
                index += 1;
            }
        }
        None
    }

    /// The account's custom sections in their stored Teams order.
    #[must_use]
    pub fn sections(&self) -> &[teams_proto::ChannelSection] {
        &self.sections
    }

    fn read_sections(&mut self, cx: &mut Context<Self>) {
        let reply = self.core.channel_data(teams_proto::ChannelRead::Sections);
        self.section_read = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            if let Ok(data) = reply.await
                && let Some(sections) = teams_proto::ChannelSections::read(&data.0)
            {
                let _ = this.update(cx, |this, cx| {
                    this.sections = sections.sections;
                    cx.notify();
                });
            }
        }));
    }

    /// Opens a channel's accessible menu from its ellipsis, a right click, or the sidebar keyboard.
    pub fn open_channel_menu(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some((index, team_at, channel_at)) = self.channel_location(id) else {
            return;
        };
        let Some(team) = self.teams.as_ref().and_then(|teams| teams.get(team_at)).cloned() else {
            return;
        };
        let channel = team.channels[channel_at].clone();
        let weak = cx.entity().downgrade();
        let folders = self.sections.clone();
        let read_only = self.core.read_only();
        let return_focus = window.focused(cx);
        let palette = crate::theme::current(cx);
        let menu = PopupMenu::build(window, cx, move |menu, window, cx| {
            let chosen_team = team.clone();
            let chosen_channel = channel.clone();
            let menu = crate::channel_menu::build(
                menu,
                crate::channel_menu::MenuContext {
                    team: &team,
                    channel: &channel,
                    read_only,
                    folders: &folders,
                    index,
                    palette,
                },
                window,
                cx,
                move |choice, _, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.channel_choice(chosen_team.clone(), chosen_channel.clone(), choice, cx);
                    });
                },
            );
            if let Some(focus) = return_focus {
                menu.action_context(focus)
            } else {
                menu
            }
        });
        self.menu_dismiss = Some(cx.subscribe(&menu, |this, _, _: &DismissEvent, cx| {
            this.channel_menu = None;
            cx.notify();
        }));
        menu.focus_handle(cx).focus(window, cx);
        self.channel_menu = Some((id.to_owned(), menu));
        cx.notify();
    }

    /// A section's fold is an account-owned Teams setting and survives a restart.
    pub fn toggle_section(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.section_pending.contains(id) {
            return;
        }
        let Some(section) = self.sections.iter_mut().find(|section| section.id == id) else {
            return;
        };
        let expanded = !section.expanded;
        section.expanded = expanded;
        if self.core.read_only() {
            cx.notify();
            return;
        }
        let id = id.to_owned();
        self.section_pending.insert(id.clone());
        let reply = self.core.channel_section(teams_proto::ChannelSectionAction::Expanded {
            section: id.clone(),
            expanded,
        });
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = reply.await;
            let _ = this.update(cx, |this, cx| this.section_toggled(&id, expanded, result.is_ok(), cx));
        })
        .detach();
        cx.notify();
    }

    fn section_toggled(&mut self, id: &str, expanded: bool, went: bool, cx: &mut Context<Self>) {
        self.section_pending.remove(id);
        if !went {
            if let Some(section) = self.sections.iter_mut().find(|section| section.id == id) {
                section.expanded = !expanded;
            }
            cx.emit(TeamsListEvent::SectionFailed);
        }
        cx.notify();
    }

    /// A channel menu choice, routed to the shell without a request from the view.
    pub fn channel_choice(
        &mut self,
        team: Team,
        channel: TeamChannel,
        choice: crate::channel_menu::ChannelChoice,
        cx: &mut Context<Self>,
    ) {
        self.channel_menu = None;
        cx.emit(TeamsListEvent::ChannelChosen(team, channel, choice));
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

    /// Asks the shell for the team `id`'s "Create a channel" dialog.
    pub fn add_channel(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(team) = self.teams.iter().flatten().find(|team| team.id == id) {
            cx.emit(TeamsListEvent::AddChannel(team.clone()));
        }
    }

    /// Tells the shell an entry of the team `id`'s "..." menu was chosen.
    pub fn choose(&mut self, id: &str, choice: TeamChoice, cx: &mut Context<Self>) {
        if choice == TeamChoice::AddChannel {
            self.add_channel(id, cx);
        } else if let Some(team) = self.teams.iter().flatten().find(|team| team.id == id) {
            cx.emit(TeamsListEvent::Chosen(team.clone(), choice));
        }
    }

    /// Tells the shell "See all your teams" was chosen.
    pub fn see_all_teams(&mut self, cx: &mut Context<Self>) {
        cx.emit(TeamsListEvent::SeeAllTeams);
    }

    /// Tells the shell a team's "See all channels" was chosen.
    pub fn see_all_channels(&mut self, id: &str, cx: &mut Context<Self>) {
        cx.emit(TeamsListEvent::SeeAllChannels(id.to_owned()));
    }

    /// A team's face: its picture once read, its initials on a colour of its own until then. `round` as on the teams page,
    /// square as in the sidebar.
    #[must_use]
    pub fn avatar(&self, team: &Team, size: f32, round: bool, palette: Palette) -> AnyElement {
        let face = div().size(px(size)).flex_none().overflow_hidden();
        let face = if round { face.rounded_full() } else { face.rounded_sm() };
        if let Some(Some(picture)) = self.pictures.get(&team.id) {
            return face
                .child(gpui::img(Arc::clone(picture)).size_full())
                .into_any_element();
        }
        face.flex()
            .items_center()
            .justify_center()
            .text_size(px(size * 0.36))
            .bg(rgb(team_colour(&team.name)))
            .text_color(rgb(palette.on_brand))
            .child(initials(&team.name))
            .into_any_element()
    }

    /// A team's picture changed on the user's Save: it shows at once.
    pub fn picture_changed(&mut self, team: String, picture: Arc<Image>, cx: &mut Context<Self>) {
        self.pictures.insert(team, Some(picture));
        cx.notify();
    }

    /// A team's picture came, when it did.
    fn picture_read(&mut self, team: String, picture: Option<Arc<Image>>, cx: &mut Context<Self>) {
        if picture.is_some() {
            self.pictures.insert(team, picture);
            cx.notify();
        }
    }

    /// Reads the pictures of the teams it hasn't asked for yet.
    fn read_pictures(&mut self, cx: &mut Context<Self>) {
        let own = self.core.me().strip_prefix("8:orgid:").unwrap_or_default().to_owned();
        if own.is_empty() {
            return;
        }
        let wanted: Vec<Team> = self
            .teams
            .iter()
            .flatten()
            .filter(|team| team.group_id.is_some() && !self.pictures.contains_key(&team.id))
            .cloned()
            .collect();
        for team in wanted {
            self.pictures.insert(team.id.clone(), None);
            let ask = TeamPicture {
                own: own.clone(),
                group: team.group_id.clone().unwrap_or_default(),
                name: team.name.clone(),
                etag: team.about.picture_etag.clone().unwrap_or_default(),
            };
            let reply = self.core.team_picture(ask);
            let read = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let picture = reply.await.ok().flatten().map(|picture| {
                    let format = match picture.format {
                        PastedFormat::Png => ImageFormat::Png,
                        PastedFormat::Jpeg => ImageFormat::Jpeg,
                        PastedFormat::Gif => ImageFormat::Gif,
                        PastedFormat::Webp => ImageFormat::Webp,
                    };
                    Arc::new(Image::from_bytes(format, picture.bytes.clone()))
                });
                let _ = this.update(cx, |list, cx| list.picture_read(team.id, picture, cx));
            });
            self.picture_reads.push(read);
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
        self.read_sections(cx);
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
            self.read_pictures(cx);
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
        let owner = self.teams.as_ref().and_then(|teams| teams.get(team)).cloned();
        let selected = self.selected.as_deref() == Some(shown.id.as_str());
        let background = if selected { palette.selected } else { palette.surface };
        let opened = shown.id.clone();
        let pointed = shown.id.clone();
        div()
            .id(ElementId::Name(format!("channel-{}", shown.id).into()))
            .debug_selector(move || format!("channel-row-{index}"))
            .w_full()
            .h(px(CHANNEL_HEIGHT))
            .pl(px(CHANNEL_INDENT))
            .pr_3()
            .flex()
            .items_center()
            .gap_1()
            .text_sm()
            .bg(rgb(background))
            .hover(move |style| style.bg(rgb(if selected { palette.selected } else { palette.hover })))
            .on_click(cx.listener(move |this, _, _, cx| this.open(&opened, cx)))
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.open_channel_menu(&pointed, window, cx);
                }),
            )
            .child(div().flex_1().min_w_0().truncate().child(shown.name.clone()))
            .when(shown.kind == ChannelKind::Private, |row| {
                row.child(
                    div()
                        .debug_selector(move || format!("channel-private-{index}"))
                        .flex_none()
                        .text_color(rgb(palette.muted_foreground))
                        .child(Glyph::LOCK.icon().size(px(12.0))),
                )
            })
            .when_some(owner, |row, _| {
                let toggled = shown.id.clone();
                let open = self
                    .channel_menu
                    .as_ref()
                    .filter(|(id, _)| id == &shown.id)
                    .map(|(_, menu)| menu.clone());
                let button = icon_button(format!("channel-more-{index}"), Glyph::ELLIPSIS, "More channel options")
                    .xsmall()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        if this.channel_menu.as_ref().is_some_and(|(id, _)| id == &toggled) {
                            this.channel_menu = None;
                            cx.notify();
                        } else {
                            this.open_channel_menu(&toggled, window, cx);
                        }
                    }));
                row.child(
                    div()
                        .id(SharedString::from(format!("channel-more-wrap-{index}")))
                        .flex_none()
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(selected_by(format!("channel-more-{index}"), button))
                        .children(open.map(|menu| {
                            gpui::deferred(
                                gpui::anchored()
                                    .anchor(gpui::Anchor::TopLeft)
                                    .snap_to_window_with_margin(px(8.0))
                                    .child(menu),
                            )
                            .with_priority(1)
                        })),
                )
            })
            .into_any_element()
    }
}

/// A team's initials as the service draws them: the first letters of its first two words, or its first letter.
#[must_use]
pub fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|word| word.chars().find(|c| c.is_alphanumeric()))
        .take(2)
        .flat_map(char::to_uppercase)
        .collect()
}

/// The colour of a team's initials: one of the service's by its name, the same for a name each time.
pub(crate) fn team_colour(name: &str) -> u32 {
    const COLOURS: [u32; 8] = [
        0x0020_8c5c,
        0x0014_8a8f,
        0x0077_4dff,
        0x00e6_1a3a,
        0x0000_6fe6,
        0x00d6_10a9,
        0x0066_6666,
        0x00c8_5a00,
    ];
    let sum: usize = name.bytes().map(usize::from).sum();
    COLOURS[sum % COLOURS.len()]
}
