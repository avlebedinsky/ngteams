//! The sidebar: the list pane as Teams shows it, one list under the "Chat" header (with the connection state while it
//! isn't up: connecting, reconnecting, offline or signed out; nothing once connected; in the read-only mode, which
//! opens no push channel and reads by polling, a quiet "Read-only" instead of a connection that never comes up, unless
//! the engine stopped or the account is signed out) holding its sections:
//! "Favorites" (the [`ChatList`]'s favourites (chats and channels), only while it has any), "Chats" (the rows of its
//! others it shows, then See more while any are left) and "Teams and channels" (the [`TeamsList`]'s teams, each with
//! its channels). A section's header folds and unfolds it, and a team's line its channels; a team's "..." opens its menu
//! ([`Sidebar::open_team_menu`]); unfolding the teams asks for them again ([`TeamsList::reload`]: the engine reads them
//! when they are old). The rows themselves are the lists': a chat clicked is the [`ChatList`]'s to tell, a channel the
//! [`TeamsList`]'s. The chat that becomes selected is scrolled into view, wherever it was opened from.
//!
//! The keyboard reaches every row as Teams' tree of chats, teams and channels does: Ctrl+2 gives the list the keyboard
//! ([`Sidebar::focus_list`]), its row ringed; the keys are `on_key`'s; a team's "..." menu opened by Shift+F10 or the menu
//! key starts on its first entry, and closing it gives the keyboard back to the team's line.
//!
//! Traced: `sidebar folded`/`unfolded` with the section (`favorites`, `chats` or `teams`), `team folded`/`unfolded` with the team.

use std::collections::HashSet;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Context, ElementId, Entity, FocusHandle, InteractiveElement as _, IntoElement, KeyDownEvent,
    ListAlignment, ListOffset, ListState, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Window, div, list, px, rgb,
};
use gpui_component::Sizable as _;
use gpui_component::tooltip::Tooltip;
use teams_store::Team;

use crate::chat_list::{ChatList, ROW_HEIGHT};
use crate::icons::{Glyph, icon_button, selected_by};
use crate::models::Models;
use crate::teams_list::{CHANNEL_HEIGHT, LOADING, NO_TEAMS, TeamsList, UNREADABLE};
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};
use crate::view::{Shown, status_text};

/// The header's state in the read-only mode, in place of a connection.
pub const READ_ONLY: &str = "Read-only";

/// Its tooltip.
const READ_ONLY_TIP: &str = "This window only reads your chats: it sends nothing";

/// The connection's dot while it comes up (Teams' amber).
const CONNECTING: u32 = 0x00c1_9c00;

/// A section header's height, px.
pub const SECTION_HEIGHT: f32 = 32.0;
/// A team's line's height, px.
pub const TEAM_HEIGHT: f32 = 32.0;
/// See more's height, px.
pub const SEE_MORE_HEIGHT: f32 = 40.0;
/// What the link under the chats says.
pub const SEE_MORE: &str = "See more";
/// Its test selector, and the group its chevron shows on hover in.
const SEE_MORE_SELECTOR: &str = "chats-see-more";
/// What the link under the teams says, and its test selector.
pub const SEE_ALL_TEAMS: &str = "See all your teams";
/// How far past the visible rows the list draws, px.
const OVERDRAW: f32 = 400.0;

/// A section of the sidebar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Favorites,
    Chats,
    Teams,
}

impl Section {
    /// The trace's and the test selector's name for it.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Favorites => "favorites",
            Self::Chats => "chats",
            Self::Teams => "teams",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Favorites => "Favorites",
            Self::Chats => "Chats",
            Self::Teams => "Teams and channels",
        }
    }
}

/// A team's line's kind in [`Entry::key`].
const TEAM_KEY: u8 = 3;

/// One row of the sidebar's list.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Entry {
    Section(Section),
    /// The chat list's favourite at `index`.
    Favorite {
        index: usize,
        id: String,
    },
    /// The chat list's row at `index`.
    Chat {
        index: usize,
        id: String,
    },
    /// The link under the chats shown, while any are left.
    SeeMore,
    /// What the teams section says instead of teams.
    Note(&'static str),
    /// The team at `index` of the Teams list.
    Team {
        index: usize,
        id: String,
    },
    /// The channel at `channel` of the team at `team`; `index` counts the channels across the teams (folded or not).
    Channel {
        index: usize,
        team: usize,
        channel: usize,
        id: String,
    },
    /// A team's "See all channels", while some are hidden.
    SeeAllChannels {
        index: usize,
        id: String,
    },
    /// "See all your teams", under the teams.
    SeeAllTeams,
    CustomSection {
        id: String,
        name: String,
    },
}

impl Entry {
    /// What the row is, wherever it stands: the list's rows are matched by it across a change.
    fn key(&self) -> (u8, &str) {
        match self {
            Self::CustomSection { id, .. } => (9, id),
            Self::Section(section) => (0, section.key()),
            Self::Chat { id, .. } => (1, id),
            Self::Note(text) => (2, text),
            Self::Team { id, .. } => (TEAM_KEY, id),
            Self::Channel { id, .. } => (4, id),
            Self::Favorite { id, .. } => (5, id),
            Self::SeeMore => (6, SEE_MORE_SELECTOR),
            Self::SeeAllChannels { id, .. } => (7, id),
            Self::SeeAllTeams => (8, SEE_ALL_TEAMS),
        }
    }

    /// The row's height, px: each is drawn at a fixed one, but a note, which is about a section header's and comes
    /// after every row the sidebar scrolls to.
    fn height(&self) -> f32 {
        match self {
            Self::Section(_) | Self::Note(_) | Self::CustomSection { .. } => SECTION_HEIGHT,
            Self::Favorite { .. } | Self::Chat { .. } => ROW_HEIGHT,
            Self::SeeMore | Self::SeeAllChannels { .. } | Self::SeeAllTeams => SEE_MORE_HEIGHT,
            Self::Team { .. } => TEAM_HEIGHT,
            Self::Channel { .. } => CHANNEL_HEIGHT,
        }
    }

    /// The chat the row is (a favourite or a row of "Chats").
    fn chat(&self) -> Option<&str> {
        match self {
            Self::Chat { id, .. } | Self::Favorite { id, .. } => Some(id),
            _ => None,
        }
    }
}

/// The sidebar view (see the module docs).
pub struct Sidebar {
    list: Entity<ChatList>,
    teams: Entity<TeamsList>,
    models: Models,
    trace: Arc<ViewTrace>,
    state: ListState,
    entries: Vec<Entry>,
    favorites_open: bool,
    chats_open: bool,
    teams_open: bool,
    /// The teams whose channels are folded, by id.
    folded: HashSet<String>,
    /// The selected chat last scrolled into view.
    revealed: Option<String>,
    /// The team whose "..." menu is open.
    team_menu: Option<String>,
    /// The entry of the "..." menu the arrow keys are on.
    menu_highlight: Option<usize>,
    menu_focus: FocusHandle,
    /// The list's keyboard focus: Up and Down move [`Self::cursor`] over its rows.
    focus: FocusHandle,
    /// The row the keyboard is on, by what it is ([`Entry::key`]), kept across a change of the rows.
    cursor: Option<(u8, String)>,
    _observing: [Subscription; 2],
}

impl Sidebar {
    /// The sidebar over the chat list and the Teams list, both sections unfolded.
    pub fn new(
        list: Entity<ChatList>,
        teams: Entity<TeamsList>,
        models: Models,
        trace: Arc<ViewTrace>,
        cx: &mut Context<Self>,
    ) -> Self {
        // the rows, a presence, the connection state: the list's notify; the teams, a failed read: the Teams list's
        let chats = cx.observe(&list, |this: &mut Self, _, cx| this.rebuild(cx));
        let teamed = cx.observe(&teams, |this: &mut Self, _, cx| this.rebuild(cx));
        let mut sidebar = Self {
            list,
            teams,
            models,
            trace,
            state: ListState::new(0, ListAlignment::Top, px(OVERDRAW)),
            entries: Vec::new(),
            favorites_open: true,
            chats_open: true,
            teams_open: true,
            folded: HashSet::new(),
            revealed: None,
            team_menu: None,
            menu_highlight: None,
            menu_focus: cx.focus_handle(),
            focus: cx.focus_handle().tab_stop(true),
            cursor: None,
            _observing: [chats, teamed],
        };
        sidebar.rebuild(cx);
        sidebar
    }

    /// Whether `section` is unfolded.
    #[must_use]
    pub fn is_open(&self, section: Section) -> bool {
        match section {
            Section::Favorites => self.favorites_open,
            Section::Chats => self.chats_open,
            Section::Teams => self.teams_open,
        }
    }

    /// Whether the team `id`'s channels are shown.
    #[must_use]
    pub fn team_open(&self, id: &str) -> bool {
        !self.folded.contains(id)
    }

    fn toggle_custom_section(&mut self, id: &str, cx: &mut Context<Self>) {
        self.teams.update(cx, |teams, cx| teams.toggle_section(id, cx));
    }

    /// Folds `section`, or unfolds it when it is folded; the teams unfolded are asked for again.
    pub fn toggle(&mut self, section: Section, cx: &mut Context<Self>) {
        let open = match section {
            Section::Favorites => &mut self.favorites_open,
            Section::Chats => &mut self.chats_open,
            Section::Teams => &mut self.teams_open,
        };
        *open = !*open;
        let opened = *open;
        self.trace
            .record(&TraceLine::new("sidebar", if opened { "unfolded" } else { "folded" }).kind(section.key()));
        if opened && section == Section::Teams {
            self.teams.update(cx, TeamsList::reload);
        }
        self.rebuild(cx);
    }

    /// Folds the team `id`'s channels, or unfolds them when they are folded.
    pub fn toggle_team(&mut self, id: &str, cx: &mut Context<Self>) {
        let opened = self.folded.remove(id);
        if !opened {
            self.folded.insert(id.to_owned());
        }
        self.trace
            .record(&TraceLine::new("team", if opened { "unfolded" } else { "folded" }).conversation(id));
        self.rebuild(cx);
    }

    /// The rows as the lists and the folding have them now, told to the list state.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let entries = self.entries(cx);
        splice(&self.state, &self.entries, &entries);
        self.entries = entries;
        self.reveal_selected(cx);
        cx.notify();
    }

    /// Scrolls to the selected chat once, when it became selected and has a row. A row opened from the list is on
    /// screen already, and doesn't move.
    fn reveal_selected(&mut self, cx: &App) {
        let list = self.list.read(cx);
        let Some(selected) = list.selected().filter(|id| self.revealed.as_deref() != Some(*id)) else {
            return;
        };
        let at = self.entries.iter().position(|entry| entry.chat() == Some(selected));
        if at.is_some_and(|at| self.reveal(at)) {
            self.revealed = Some(selected.to_owned());
        }
    }

    /// Scrolls to See more, when it is listed.
    pub fn reveal_see_more(&mut self, cx: &mut Context<Self>) {
        if let Some(at) = self.entries.iter().position(|entry| *entry == Entry::SeeMore) {
            self.reveal(at);
            cx.notify();
        }
    }

    /// Scrolls the row at `at` just into view, unless it is in view; `false` before the list has a height to tell. By
    /// the rows' own heights, as the list knows only the ones it has drawn, and none of the ones just listed.
    fn reveal(&self, at: usize) -> bool {
        let height = f32::from(self.state.viewport_bounds().size.height);
        if height <= 0.0 {
            return false;
        }
        let top_of = |row: usize| -> f32 { self.entries.iter().take(row).map(Entry::height).sum() };
        let scrolled = self.state.logical_scroll_top();
        let from = top_of(scrolled.item_ix) + f32::from(scrolled.offset_in_item);
        let top = top_of(at);
        let bottom = top_of(at + 1);
        let goal = if top < from {
            top
        } else if bottom > from + height {
            bottom - height
        } else {
            return true;
        };
        let mut item_ix = 0;
        let mut item_top = 0.0;
        for entry in &self.entries {
            if item_top + entry.height() > goal {
                break;
            }
            item_top += entry.height();
            item_ix += 1;
        }
        self.state.scroll_to(ListOffset {
            item_ix,
            offset_in_item: px(goal - item_top),
        });
        true
    }

    fn entries(&self, cx: &App) -> Vec<Entry> {
        let mut entries = Vec::new();
        let favorites = self.list.read(cx).favorites();
        if !favorites.is_empty() {
            entries.push(Entry::Section(Section::Favorites));
            if self.favorites_open {
                entries.extend(favorites.iter().enumerate().map(|(index, row)| Entry::Favorite {
                    index,
                    id: row.id.clone(),
                }));
            }
        }
        // Custom folders retain Teams' server order; their channel items retain that folder's order.
        let teams = self.teams.read(cx);
        for section in teams.sections().iter().filter(|section| !section.favorite) {
            entries.push(Entry::CustomSection {
                id: section.id.clone(),
                name: section.name.clone(),
            });
            if section.expanded {
                for id in &section.items {
                    if let Some((index, team, channel)) = teams.channel_location(id) {
                        entries.push(Entry::Channel {
                            index,
                            team,
                            channel,
                            id: id.clone(),
                        });
                    }
                }
            }
        }
        entries.push(Entry::Section(Section::Chats));
        if self.chats_open {
            let list = self.list.read(cx);
            entries.extend(list.shown_rows().into_iter().filter_map(|index| {
                let id = list.rows().get(index)?.id.clone();
                Some(Entry::Chat { index, id })
            }));
            if list.see_more_offered().is_some() {
                entries.push(Entry::SeeMore);
            }
        }
        entries.push(Entry::Section(Section::Teams));
        if self.teams_open {
            let teams = self.teams.read(cx);
            match teams.teams() {
                None if teams.unreadable(cx) => entries.push(Entry::Note(UNREADABLE)),
                None => entries.push(Entry::Note(LOADING)),
                Some([]) => {
                    entries.push(Entry::Note(NO_TEAMS));
                    entries.push(Entry::SeeAllTeams);
                }
                Some(shown) => {
                    self.team_entries(shown, teams.selected(), teams.sections(), &mut entries);
                    entries.push(Entry::SeeAllTeams);
                }
            }
        }
        entries
    }

    /// Each team's line unless its channels are hidden, as Teams leaves them out, with the channels it shows unless it is
    /// folded (the one open stays, shown or not) and "See all channels" while some are hidden.
    fn team_entries(
        &self,
        teams: &[Team],
        selected: Option<&str>,
        sections: &[teams_proto::ChannelSection],
        entries: &mut Vec<Entry>,
    ) {
        let mut index = 0;
        for (team_index, team) in teams.iter().enumerate() {
            let listed = !team.about.hidden
                || team
                    .channels
                    .iter()
                    .any(|channel| Some(channel.id.as_str()) == selected);
            if listed {
                entries.push(Entry::Team {
                    index: team_index,
                    id: team.id.clone(),
                });
            }
            let open = listed && self.team_open(&team.id);
            for (channel, shown) in team.channels.iter().enumerate() {
                let moved = sections.iter().any(|section| section.items.contains(&shown.id));
                if open && !moved && (shown.shown || selected == Some(shown.id.as_str())) {
                    entries.push(Entry::Channel {
                        index,
                        team: team_index,
                        channel,
                        id: shown.id.clone(),
                    });
                }
                index += 1;
            }
            if open && team.hidden_channels() > 0 {
                entries.push(Entry::SeeAllChannels {
                    index: team_index,
                    id: team.id.clone(),
                });
            }
        }
    }

    /// The row at `at`, ringed while the keyboard is on it.
    fn render_entry(&mut self, at: usize, window: &Window, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let keyed = self.focus.is_focused(window) && self.cursor_at() == Some(at);
        let row = self.entry_row(at, keyed, palette, cx);
        if !keyed {
            return row;
        }
        div()
            .debug_selector(|| "sidebar-cursor".to_owned())
            .relative()
            .w_full()
            .child(row)
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded_md()
                    .border_2()
                    .border_color(rgb(palette.brand)),
            )
            .into_any_element()
    }

    fn entry_row(&mut self, at: usize, keyed: bool, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let Some(entry) = self.entries.get(at).cloned() else {
            return div().into_any_element();
        };
        match entry {
            Entry::CustomSection { id, name } => {
                let open = self
                    .teams
                    .read(cx)
                    .sections()
                    .iter()
                    .find(|section| section.id == id)
                    .is_none_or(|section| section.expanded);
                let selector = id.clone();
                div()
                    .id(SharedString::from(format!("channel-section-{id}")))
                    .debug_selector(move || format!("channel-section-{selector}"))
                    .w_full()
                    .h(px(SECTION_HEIGHT))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_sm()
                    .text_color(rgb(palette.muted_foreground))
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgb(palette.hover)))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_custom_section(&id, cx)))
                    .child(chevron(open))
                    .child(name)
                    .into_any_element()
            }
            Entry::Section(section) => self.section_header(section, palette, cx),
            Entry::Favorite { index, id } => {
                let location = self.teams.read(cx).channel_location(&id);
                if let Some((index, team, channel)) = location {
                    self.teams
                        .update(cx, |teams, cx| teams.channel_row(index, team, channel, palette, cx))
                } else {
                    self.list
                        .update(cx, |list, cx| list.render_favorite(index, palette, cx))
                }
            }
            Entry::Chat { index, .. } => self.list.update(cx, |list, cx| list.render_row(index, palette, cx)),
            Entry::SeeMore => self.see_more_row(palette, cx),
            Entry::SeeAllChannels { index, id } => Self::see_all_channels_row(index, id, palette, cx),
            Entry::SeeAllTeams => Self::see_all_teams_row(palette, cx),
            Entry::Note(text) => div()
                .debug_selector(|| "teams-note".to_owned())
                .px_3()
                .py_2()
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .child(text)
                .into_any_element(),
            Entry::Team { index, id } => self.team_line(index, &id, keyed, palette, cx),
            Entry::Channel {
                index, team, channel, ..
            } => self
                .teams
                .update(cx, |teams, cx| teams.channel_row(index, team, channel, palette, cx)),
        }
    }

    /// A section's header, its test selector `section-<key>`.
    fn section_header(&self, section: Section, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id(ElementId::Name(format!("section-{}", section.key()).into()))
            .debug_selector(move || format!("section-{}", section.key()))
            .w_full()
            .h(px(SECTION_HEIGHT))
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .text_sm()
            .text_color(rgb(palette.muted_foreground))
            .cursor_pointer()
            .hover(move |style| style.bg(rgb(palette.hover)))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle(section, cx)))
            .child(chevron(self.is_open(section)))
            .child(section.title())
            .into_any_element()
    }

    /// See more, as Teams draws it: a link in the brand colour, bold while a chat left below it has something unread,
    /// its chevron shown on hover; its test selector [`SEE_MORE_SELECTOR`].
    fn see_more_row(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let unread = self.list.read(cx).see_more_offered().is_some_and(|more| more.unread);
        let label = div().child(SEE_MORE);
        let label = if unread {
            label.font_weight(gpui::FontWeight::BOLD)
        } else {
            label
        };
        let chevron = div()
            .flex_none()
            .opacity(0.0)
            .group_hover(SEE_MORE_SELECTOR, |style| style.opacity(1.0))
            .child(Glyph::CHEVRON_DOWN.icon().size(px(14.0)));
        div()
            .id(SEE_MORE_SELECTOR)
            .debug_selector(|| SEE_MORE_SELECTOR.to_owned())
            .group(SEE_MORE_SELECTOR)
            .w_full()
            .h(px(SEE_MORE_HEIGHT))
            .px_3()
            .flex()
            .items_center()
            .gap_1()
            .text_color(rgb(palette.brand))
            .cursor_pointer()
            .hover(move |style| style.bg(rgb(palette.hover)))
            .on_click(cx.listener(|this, _, _, cx| this.list.update(cx, ChatList::see_more)))
            .child(label)
            .child(chevron)
            .into_any_element()
    }

    /// A team's "See all channels", a link in the brand colour under its channels; its test selector
    /// `see-all-channels-<index>`.
    fn see_all_channels_row(index: usize, id: String, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id(ElementId::Name(format!("see-all-channels-{id}").into()))
            .debug_selector(move || format!("see-all-channels-{index}"))
            .w_full()
            .h(px(SEE_MORE_HEIGHT))
            .pl(px(crate::teams_list::CHANNEL_INDENT))
            .flex()
            .items_center()
            .text_sm()
            .text_color(rgb(palette.brand))
            .cursor_pointer()
            .hover(move |style| style.bg(rgb(palette.hover)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.teams.update(cx, |teams, cx| teams.see_all_channels(&id, cx));
            }))
            .child("See all channels")
            .into_any_element()
    }

    /// "See all your teams", a link in the brand colour under the teams; its test selector `see-all-teams`.
    fn see_all_teams_row(palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("see-all-teams")
            .debug_selector(|| "see-all-teams".to_owned())
            .w_full()
            .h(px(SEE_MORE_HEIGHT))
            .px_3()
            .flex()
            .items_center()
            .text_sm()
            .text_color(rgb(palette.brand))
            .cursor_pointer()
            .hover(move |style| style.bg(rgb(palette.hover)))
            .on_click(cx.listener(|this, _, _, cx| this.teams.update(cx, TeamsList::see_all_teams)))
            .child(SEE_ALL_TEAMS)
            .into_any_element()
    }

    /// A team's line (its fold mark, its picture and its name), its test selector `team-<index>`.
    fn team_line(&self, index: usize, id: &str, keyed: bool, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let teams = self.teams.read(cx);
        let Some(team) = teams.teams().and_then(|listed| listed.get(index)) else {
            return div().into_any_element();
        };
        let name = team.name.clone();
        let badge = teams.avatar(team, 20.0, false, palette);
        let toggled = id.to_owned();
        let group = team_line_group(index);
        let team = team.clone();
        let more = self.team_more(index, &team, keyed, palette, cx);
        let pointed = id.to_owned();
        div()
            .group(group.clone())
            .id(ElementId::Name(format!("team-{id}").into()))
            .debug_selector(move || format!("team-{index}"))
            .w_full()
            .h(px(TEAM_HEIGHT))
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .text_sm()
            .text_color(rgb(palette.foreground))
            .cursor_pointer()
            .hover(move |style| style.bg(rgb(palette.hover)))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_team(&toggled, cx)))
            // a right click opens its menu, as in Teams
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.open_team_menu(&pointed, false, window, cx);
                }),
            )
            .child(chevron(self.team_open(id)))
            .child(badge)
            .child(div().flex_1().min_w_0().truncate().child(name))
            .when(team.archived, |line| {
                line.child(
                    div()
                        .flex_none()
                        .text_color(rgb(palette.muted_foreground))
                        .child(Glyph::ARCHIVE.icon().size(px(14.0))),
                )
            })
            .child(more)
            .into_any_element()
    }

    /// A team's "..." and its menu ([`crate::team_menu`]), shown while the line is under the pointer or its menu is open.
    fn team_more(
        &self,
        index: usize,
        team: &Team,
        keyed: bool,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = team.id.as_str();
        let group = team_line_group(index);
        let read_only = self.list.read(cx).read_only();
        let open = self.team_menu.as_deref() == Some(id);
        let toggled = id.to_owned();
        let button = selected_by(
            format!("team-more-{index}"),
            icon_button(format!("team-more-{id}"), Glyph::ELLIPSIS, "More options")
                .xsmall()
                // the list takes the keyboard, Shift+F10 opening this menu
                .tab_stop(false)
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    if this.team_menu.as_deref() == Some(toggled.as_str()) {
                        this.close_team_menu(window, cx);
                    } else {
                        this.open_team_menu(&toggled, false, window, cx);
                    }
                })),
        );
        let menu = open.then(|| {
            let weak = cx.entity().downgrade();
            let (chosen_from, closed, keys) = (weak.clone(), weak.clone(), weak);
            let (chosen_team, keyed_team) = (id.to_owned(), id.to_owned());
            let shown = crate::team_menu::entries(team, read_only);
            crate::team_menu::Panel {
                index,
                team,
                read_only,
                highlight: self.menu_highlight,
                focus: &self.menu_focus,
                palette,
                anchor: gpui::Anchor::TopLeft,
            }
            .render(
                move |choice, window, cx| {
                    let _ = chosen_from.update(cx, |this, cx| this.choose(&chosen_team, choice, window, cx));
                },
                move |_, _, cx| {
                    // a click elsewhere takes the focus where it clicked
                    let _ = closed.update(cx, |this, cx| {
                        this.team_menu = None;
                        cx.notify();
                    });
                },
                move |event: &KeyDownEvent, window, cx| {
                    let _ = keys.update(cx, |this, cx| this.menu_key(&keyed_team, &shown, event, window, cx));
                },
            )
        });
        div()
            .id(ElementId::Name(format!("team-more-wrap-{id}").into()))
            .flex_none()
            // shown while the line is under the pointer, as Teams' "..." is, and while its menu is open; it takes its own
            // clicks
            // and while the keyboard is on the line
            .when(!open && !keyed, |more| {
                more.invisible().group_hover(group.clone(), gpui::Styled::visible)
            })
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(button)
            .children(menu)
            .into_any_element()
    }

    /// A key on the team `id`'s "..." menu, `shown` its entries.
    fn menu_key(
        &mut self,
        id: &str,
        shown: &[crate::team_menu::TeamChoice],
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        match crate::team_menu::key(event, shown, &mut self.menu_highlight) {
            crate::team_menu::MenuKey::Close => self.close_team_menu(window, cx),
            crate::team_menu::MenuKey::Choose(choice) => self.choose(id, choice, window, cx),
            crate::team_menu::MenuKey::Stay => cx.notify(),
        }
    }

    /// Opens the team `id`'s "..." menu, the keyboard on its first entry when `by_keyboard`, as Teams opens it; the
    /// keyboard's row is the team's line, where the focus comes back to.
    pub fn open_team_menu(&mut self, id: &str, by_keyboard: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.team_menu = Some(id.to_owned());
        self.menu_highlight = by_keyboard.then_some(0);
        self.cursor = Some((TEAM_KEY, id.to_owned()));
        self.menu_focus.focus(window, cx);
        cx.notify();
    }

    /// Closes the team's "..." menu, the focus back on the list, on the team's line.
    pub fn close_team_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.team_menu = None;
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// An entry of the menu chosen: the shell acts on it (the focus comes back to the team's line first, so a dialog
    /// it opens gives it back there).
    fn choose(&mut self, id: &str, choice: crate::team_menu::TeamChoice, window: &mut Window, cx: &mut Context<Self>) {
        self.close_team_menu(window, cx);
        self.teams.update(cx, |teams, cx| teams.choose(id, choice, cx));
    }
}

/// The list's keys, as Teams' tree of chats, teams and channels takes them: Up, Down, Home and End move over the rows;
/// Enter and Space open a chat or a channel, fold a section or a team, or follow a link; Right unfolds and Left folds, or
/// goes up to the row's team or section; Shift+F10 or the menu key opens a team's "..." menu.
impl Sidebar {
    /// Gives the list the keyboard, on the selected row (or the first).
    pub fn focus_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.cursor_at().is_none() {
            let selected = self.list.read(cx).selected().map(str::to_owned);
            let at = selected
                .and_then(|id| self.entries.iter().position(|entry| entry.chat() == Some(id.as_str())))
                .or_else(|| self.rows().next());
            if let Some(at) = at {
                self.set_cursor(at);
            }
        }
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// Gives the list the keyboard on the team `id`'s line.
    pub fn focus_team(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(at) = self.entries.iter().position(|entry| entry.key() == (TEAM_KEY, id)) {
            self.set_cursor(at);
        }
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// Whether the list has the keyboard.
    #[must_use]
    pub fn has_keyboard(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }

    /// The row the keyboard is on, where it is now.
    fn cursor_at(&self) -> Option<usize> {
        let (kind, id) = self.cursor.as_ref()?;
        self.entries
            .iter()
            .position(|entry| entry.key() == (*kind, id.as_str()))
    }

    fn set_cursor(&mut self, at: usize) {
        if let Some(entry) = self.entries.get(at) {
            let (kind, id) = entry.key();
            self.cursor = Some((kind, id.to_owned()));
            self.reveal(at);
        }
    }

    /// The rows the keyboard stops on: all but a section's note.
    fn rows(&self) -> impl Iterator<Item = usize> + '_ {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| !matches!(entry, Entry::Note(_)))
            .map(|(at, _)| at)
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let rows: Vec<usize> = self.rows().collect();
        let at = self.cursor_at();
        let place = at.and_then(|at| rows.iter().position(|row| *row == at));
        if let Some(to) = crate::keyboard::list_step(event, rows.len(), place) {
            self.set_cursor(rows[to]);
        } else if let Some(at) = at {
            if !self.row_key(at, event, window, cx) {
                return;
            }
        } else {
            return;
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// A key on the row at `at`; `false` when it does nothing there.
    fn row_key(&mut self, at: usize, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(entry) = self.entries.get(at).cloned() else {
            return false;
        };
        if crate::keyboard::opens_menu(event) {
            if let Entry::Channel { id, .. } | Entry::Favorite { id, .. } = &entry {
                if self.teams.read(cx).channel_location(id).is_some() {
                    self.teams
                        .update(cx, |teams, cx| teams.open_channel_menu(id, window, cx));
                    return true;
                }
            }
            let Entry::Team { id, .. } = entry else {
                return false;
            };
            let offered = self
                .teams
                .read(cx)
                .teams()
                .is_some_and(|teams| teams.iter().any(|team| team.id == id));
            if offered {
                self.open_team_menu(&id, true, window, cx);
            }
            return offered;
        }
        if event.keystroke.modifiers.modified() {
            return false;
        }
        match event.keystroke.key.as_str() {
            "enter" | "space" => self.activate(entry, cx),
            "right" => match entry {
                Entry::Section(section) if !self.is_open(section) => self.toggle(section, cx),
                Entry::Team { id, .. } if !self.team_open(&id) => self.toggle_team(&id, cx),
                _ => {}
            },
            "left" => match entry {
                Entry::Section(section) if self.is_open(section) => self.toggle(section, cx),
                Entry::Team { id, .. } if self.team_open(&id) => self.toggle_team(&id, cx),
                _ => {
                    if let Some(parent) = self.parent(at) {
                        self.set_cursor(parent);
                    }
                }
            },
            _ => return false,
        }
        true
    }

    /// What Enter does on a row: what a click on it does.
    fn activate(&mut self, entry: Entry, cx: &mut Context<Self>) {
        match entry {
            Entry::CustomSection { id, .. } => self.toggle_custom_section(&id, cx),
            Entry::Section(section) => self.toggle(section, cx),
            Entry::Favorite { id, .. } | Entry::Chat { id, .. } => self.list.update(cx, |list, cx| list.open(id, cx)),
            Entry::SeeMore => self.list.update(cx, ChatList::see_more),
            Entry::Note(_) => {}
            Entry::Team { id, .. } => self.toggle_team(&id, cx),
            Entry::Channel { id, .. } => self.teams.update(cx, |teams, cx| teams.open(&id, cx)),
            Entry::SeeAllChannels { id, .. } => self.teams.update(cx, |teams, cx| teams.see_all_channels(&id, cx)),
            Entry::SeeAllTeams => self.teams.update(cx, TeamsList::see_all_teams),
        }
    }

    /// The row a row belongs to: a channel's team, or a row's section.
    fn parent(&self, at: usize) -> Option<usize> {
        let in_team = matches!(
            self.entries.get(at),
            Some(Entry::Channel { .. } | Entry::SeeAllChannels { .. })
        );
        self.entries[..at].iter().rposition(|entry| match entry {
            Entry::Team { .. } => in_team,
            Entry::Section(_) | Entry::CustomSection { .. } => true,
            _ => false,
        })
    }
}

/// A fold mark: down while unfolded.
fn chevron(open: bool) -> impl IntoElement {
    div()
        .w(px(12.0))
        .flex_none()
        .text_xs()
        .child(if open { "▾" } else { "▸" })
}

/// Tells `state` how `old` became `new`: the rows they start and end with (by what they are) keep their place and
/// their measured height, the ones between are replaced. A list scrolled to its very top stays there: rows that come
/// in above its first one (the "Favorites" section arriving after the first load, once the teams' read gave the
/// favourites) are shown, not scrolled past. GPUI's list anchors its scroll position on the item it shows first, and
/// sets that anchor after any layout whose rows didn't fill the pane, so without this the list would keep "Chats" at
/// its top and the favourites out of sight above it.
fn splice(state: &ListState, old: &[Entry], new: &[Entry]) {
    let head = old
        .iter()
        .zip(new)
        .take_while(|(old, new)| old.key() == new.key())
        .count();
    let tail = old[head..]
        .iter()
        .rev()
        .zip(new[head..].iter().rev())
        .take_while(|(old, new)| old.key() == new.key())
        .count();
    if head + tail == old.len() && old.len() == new.len() {
        return;
    }
    let top = state.logical_scroll_top();
    let at_top = top.item_ix == 0 && top.offset_in_item <= px(0.0);
    state.splice(head..old.len() - tail, new.len() - head - tail);
    if at_top {
        state.scroll_to(ListOffset::default());
    }
}

impl Sidebar {
    /// The header's connection state (see the module docs): `None` while it is up, as in Teams.
    fn connection(&self, palette: Palette, cx: &App) -> Option<AnyElement> {
        let status = self.models.status.read(cx);
        let dot = match Shown::of(status) {
            Shown::Offline | Shown::SignedOut => theme::DANGER,
            // no push channel ever comes up: the list is read by polling, which is nothing to wait for
            _ if self.list.read(cx).read_only() => return Some(read_only_state(palette)),
            Shown::Connected => return None,
            Shown::Connecting | Shown::Reconnecting => CONNECTING,
        };
        let pill = div()
            .debug_selector(|| "connection".to_owned())
            .flex()
            .items_center()
            .gap_1p5()
            .px_2()
            .py_0p5()
            .rounded_full()
            .bg(rgb(palette.hover))
            .text_xs()
            .text_color(rgb(palette.muted_foreground))
            .child(div().size(px(6.0)).rounded_full().bg(rgb(dot)))
            .child(status_text(status));
        Some(pill.into_any_element())
    }
}

/// The header's state in the read-only mode: a quiet "Read-only", its tooltip saying what that means.
fn read_only_state(palette: Palette) -> AnyElement {
    div()
        .id("read-only")
        .debug_selector(|| "read-only".to_owned())
        .flex()
        .items_center()
        .gap_1()
        .text_xs()
        .text_color(rgb(palette.muted_foreground))
        .child(Glyph::INFO.icon().size(px(12.0)))
        .child(READ_ONLY)
        .tooltip(|window, cx| Tooltip::new(READ_ONLY_TIP).build(window, cx))
        .into_any_element()
}

impl Render for Sidebar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let connection = self.connection(palette, cx);
        let header = div()
            .h(px(48.0))
            .flex_none()
            .px_3()
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(div().text_lg().font_weight(gpui::FontWeight::BOLD).child("Chat"))
            .children(connection);
        let rows = list(
            self.state.clone(),
            cx.processor(move |this: &mut Self, at: usize, window, cx| this.render_entry(at, window, palette, cx)),
        )
        .size_full();
        let rows = div()
            .id("sidebar-list")
            .debug_selector(|| "sidebar-list".to_owned())
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .flex_1()
            .min_h_0()
            .w_full()
            .child(rows);
        div()
            .debug_selector(|| "sidebar".to_owned())
            .size_full()
            .overflow_hidden()
            .flex()
            .flex_col()
            .bg(rgb(palette.surface))
            .child(header)
            .child(rows)
    }
}

/// The hover group of a team's line, which shows its "...".
fn team_line_group(index: usize) -> SharedString {
    SharedString::from(format!("team-line-{index}"))
}
