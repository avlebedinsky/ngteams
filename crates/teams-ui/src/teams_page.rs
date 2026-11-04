//! "Your teams and channels", which "See all your teams" opens in the conversation pane, as Teams has it
//! (`docs/protocol/teams.md`): the tabs Teams, Pending invites and Analytics; Create team with its dropdown (Create team,
//! Join team); on Teams the filters All, Teams you own and the rest under "N more", Sort, Search teams and a row per team
//! with its "..." menu ([`crate::team_menu`]). A row clicked opens Manage team.
//!
//! The keyboard: the page opens on its rows ([`TeamsPage::focus_rows`]: Up, Down, Home and End move, Enter opens Manage
//! team, Shift+F10 or the menu key the row's menu); the tabs move by Left and Right; every chip, button and popup row is a
//! tab stop, a popup's Up, Down, Enter and Escape giving the keyboard back to what opened it ([`crate::keyboard`]).
//!
//! The teams are the [`TeamsList`]'s, from the store; the invitations and the analytics are read when their tab opens
//! ([`Core::pending_invites`], [`Core::all_teams_analytics`]). Nothing here sends anything but through [`Core`], and the
//! menu's choices are the shell's to act on. Traced: `teams-page opened`, `tab` with its key; never a name.

use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, AsyncApp, Context, ElementId, Entity, EventEmitter, FocusHandle,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity, Window, div, px, rgb,
};
use gpui_component::Sizable as _;
use gpui_component::input::{Input, InputEvent, InputState};
use teams_proto::{AnalyticsPeriod, PendingInvite, TeamAnalytics};
use teams_store::Team;

use crate::bridge::Core;
use crate::icons::{Glyph, icon_button, selected_by};
use crate::keyboard::{self, Handles, PopupFocus};
use crate::shell::menu_row;
use crate::team_menu::{self, TeamChoice};
use crate::teams_list::TeamsList;
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};

/// The page's tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PageTab {
    #[default]
    Teams,
    PendingInvites,
    Analytics,
}

impl PageTab {
    /// The tabs in order.
    pub const ALL: [Self; 3] = [Self::Teams, Self::PendingInvites, Self::Analytics];

    /// Its test selector's and the trace's name.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Teams => "teams",
            Self::PendingInvites => "pending-invites",
            Self::Analytics => "analytics",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Teams => "Teams",
            Self::PendingInvites => "Pending invites",
            Self::Analytics => "Analytics",
        }
    }
}

/// Which teams the page lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    All,
    Owned,
    ChannelsHidden,
    ChannelsShown,
    Archived,
}

impl Filter {
    /// Its test selector's name.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Owned => "owned",
            Self::ChannelsHidden => "hidden",
            Self::ChannelsShown => "shown",
            Self::Archived => "archived",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Owned => "Teams you own",
            Self::ChannelsHidden => "Teams with channels hidden",
            Self::ChannelsShown => "Teams with channels shown",
            Self::Archived => "Archived Teams",
        }
    }

    fn holds(self, team: &Team) -> bool {
        match self {
            Self::All => true,
            Self::Owned => team.owner,
            Self::ChannelsHidden => team.about.hidden,
            Self::ChannelsShown => !team.about.hidden,
            Self::Archived => team.archived,
        }
    }
}

/// How the page orders its teams.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sort {
    #[default]
    AToZ,
    ZToA,
    Membership,
    TeamType,
    Visibility,
}

impl Sort {
    const ALL: [Self; 5] = [
        Self::AToZ,
        Self::ZToA,
        Self::Membership,
        Self::TeamType,
        Self::Visibility,
    ];

    /// Its test selector's name.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::AToZ => "az",
            Self::ZToA => "za",
            Self::Membership => "membership",
            Self::TeamType => "type",
            Self::Visibility => "visibility",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::AToZ => "A-Z",
            Self::ZToA => "Z-A",
            Self::Membership => "Membership Type",
            Self::TeamType => "Team type",
            Self::Visibility => "Visibility Type",
        }
    }
}

/// The teams `teams` the page lists for `filter` and `query` (in any case, in the name or the description), in `sort`'s
/// order.
#[must_use]
pub fn listed<'a>(teams: &'a [Team], filter: Filter, sort: Sort, query: &str) -> Vec<&'a Team> {
    let query = query.trim().to_lowercase();
    let mut shown: Vec<&Team> = teams
        .iter()
        .filter(|team| filter.holds(team))
        .filter(|team| {
            query.is_empty()
                || team.name.to_lowercase().contains(&query)
                || team.about.description.to_lowercase().contains(&query)
        })
        .collect();
    shown.sort_by_key(|team| team.name.to_lowercase());
    match sort {
        Sort::AToZ => {}
        Sort::ZToA => shown.reverse(),
        Sort::Membership => shown.sort_by_key(|team| !team.owner),
        Sort::TeamType => shown.sort_by_key(|team| team.about.tenant_wide),
        Sort::Visibility => shown.sort_by_key(|team| team.about.public),
    }
    shown
}

/// What the page tells the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TeamsPageEvent {
    /// Create team.
    CreateTeam,
    /// Join Team.
    JoinTeam,
    /// A team's row, to manage it.
    Manage(String),
    /// An entry of a team's "..." menu.
    Chosen(Box<Team>, TeamChoice),
}

impl EventEmitter<TeamsPageEvent> for TeamsPage {}

/// Which of the page's popups is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Popup {
    CreateMenu,
    MoreFilters,
    Sort,
    Period,
}

/// The page (see the module docs).
pub struct TeamsPage {
    core: Arc<dyn Core>,
    teams: Entity<TeamsList>,
    trace: Arc<ViewTrace>,
    tab: PageTab,
    filter: Filter,
    sort: Sort,
    popup: Option<Popup>,
    search: Option<Entity<InputState>>,
    query: String,
    menu: Option<String>,
    menu_highlight: Option<usize>,
    menu_focus: FocusHandle,
    /// The keyboard's popup (Create team's, "N more", Sort, the time period).
    popup_focus: PopupFocus,
    /// The tabs, Left and Right moving through them.
    tabs: Handles,
    /// The rows' keyboard: Up and Down move [`Self::row_cursor`].
    rows_focus: FocusHandle,
    /// The row the keyboard is on, by the team's id.
    row_cursor: Option<String>,
    invites: Option<Vec<PendingInvite>>,
    analytics: Option<Vec<TeamAnalytics>>,
    period: AnalyticsPeriod,
    reading: Option<Task<()>>,
    observing_: Vec<Subscription>,
}

impl TeamsPage {
    /// The page on `teams`.
    pub fn new(core: Arc<dyn Core>, teams: Entity<TeamsList>, trace: Arc<ViewTrace>, cx: &mut Context<Self>) -> Self {
        let observing = vec![cx.observe(&teams, |_, _, cx| cx.notify())];
        Self {
            core,
            teams,
            trace,
            tab: PageTab::default(),
            filter: Filter::default(),
            sort: Sort::default(),
            popup: None,
            search: None,
            query: String::new(),
            menu: None,
            menu_highlight: None,
            menu_focus: cx.focus_handle(),
            popup_focus: PopupFocus::new(cx),
            tabs: Handles::default(),
            rows_focus: cx.focus_handle().tab_stop(true),
            row_cursor: None,
            invites: None,
            analytics: None,
            period: AnalyticsPeriod::default(),
            reading: None,
            observing_: observing,
        }
    }

    /// The tab shown.
    #[must_use]
    pub fn tab(&self) -> PageTab {
        self.tab
    }

    /// Shows `tab`, reading what it shows.
    pub fn show_tab(&mut self, tab: PageTab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.popup = None;
        self.trace.record(&TraceLine::new("teams-page", "tab").kind(tab.key()));
        match tab {
            PageTab::Teams => {}
            PageTab::PendingInvites => self.read_invites(cx),
            PageTab::Analytics => self.read_analytics(cx),
        }
        cx.notify();
    }

    /// The filter on the teams.
    pub fn set_filter(&mut self, filter: Filter, cx: &mut Context<Self>) {
        self.filter = filter;
        self.popup = None;
        cx.notify();
    }

    /// The order of the teams.
    pub fn set_sort(&mut self, sort: Sort, cx: &mut Context<Self>) {
        self.sort = sort;
        self.popup = None;
        cx.notify();
    }

    /// Opens the Create team dropdown (the tour shows it).
    pub fn open_create_menu(&mut self, cx: &mut Context<Self>) {
        self.popup = Some(Popup::CreateMenu);
        cx.notify();
    }

    /// The Search teams box's text.
    pub fn set_query(&mut self, query: &str, cx: &mut Context<Self>) {
        query.clone_into(&mut self.query);
        cx.notify();
    }

    /// Opens the team `id`'s "..." menu, the keyboard on its first entry when `by_keyboard`; the keyboard's row is the
    /// team's, where the focus comes back to.
    pub fn open_menu(&mut self, id: &str, by_keyboard: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = Some(id.to_owned());
        self.menu_highlight = by_keyboard.then_some(0);
        self.row_cursor = Some(id.to_owned());
        self.menu_focus.focus(window, cx);
        cx.notify();
    }

    /// Closes the "..." menu, the keyboard back on the team's row.
    pub fn close_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        self.rows_focus.focus(window, cx);
        cx.notify();
    }

    fn choose(&mut self, team: &str, choice: TeamChoice, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menu(window, cx);
        let found = self
            .teams
            .read(cx)
            .teams()
            .and_then(|teams| teams.iter().find(|listed| listed.id == team).cloned());
        if let Some(team) = found {
            cx.emit(TeamsPageEvent::Chosen(Box::new(team), choice));
        }
    }

    /// A key on the "..." menu of the team `id`.
    fn menu_key(
        &mut self,
        id: &str,
        shown: &[TeamChoice],
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        match team_menu::key(event, shown, &mut self.menu_highlight) {
            team_menu::MenuKey::Close => self.close_menu(window, cx),
            team_menu::MenuKey::Choose(choice) => self.choose(id, choice, window, cx),
            team_menu::MenuKey::Stay => cx.notify(),
        }
    }

    fn read_invites(&mut self, cx: &mut Context<Self>) {
        let reply = self.core.pending_invites();
        self.reading = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let invites = reply.await.unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                this.invites = Some(invites);
                cx.notify();
            });
        }));
    }

    fn read_analytics(&mut self, cx: &mut Context<Self>) {
        self.analytics = None;
        let reply = self.core.all_teams_analytics(self.period);
        self.reading = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let analytics = reply.await.unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                this.analytics = Some(analytics);
                cx.notify();
            });
        }));
    }

    fn search_box(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some(search) = &self.search {
            return search.clone();
        }
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search teams"));
        let typed = cx.subscribe(&search, |this: &mut Self, search, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.query = search.read(cx).value().to_string();
                cx.notify();
            }
        });
        self.observing_.push(typed);
        self.search = Some(search.clone());
        search
    }

    fn toggle_popup(&mut self, popup: Popup, window: &mut Window, cx: &mut Context<Self>) {
        if self.popup == Some(popup) {
            self.close_popup(window, cx);
        } else {
            self.popup = Some(popup);
            self.popup_focus.opened(window, cx);
            cx.notify();
        }
    }

    /// Closes the popup, the keyboard back on what opened it.
    fn close_popup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.popup = None;
        self.popup_focus.closed(window, cx);
        cx.notify();
    }

    /// The open popup's keys.
    fn popup_keys(&self, cx: &mut Context<Self>) -> impl Fn(&KeyDownEvent, &mut Window, &mut gpui::App) + 'static {
        let weak = cx.entity().downgrade();
        keyboard::menu_keys(self.popup_focus.root().clone(), move |window, cx| {
            let _ = weak.update(cx, |this, cx| this.close_popup(window, cx));
        })
    }

    /// A click outside the popup: it closes, the focus where it clicked.
    fn popup_clicked_out(&mut self, cx: &mut Context<Self>) {
        self.popup = None;
        cx.notify();
    }

    /// The header: the title, the tabs and Create team with its dropdown.
    fn header(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let tabs = PageTab::ALL.map(|tab| {
            let key = format!("teams-tab-{}", tab.key());
            let chosen = self.tab == tab;
            keyboard::stop(
                key.clone(),
                tab_button(key.clone(), tab.label(), chosen, palette)
                    .on_click(cx.listener(move |this, _, _, cx| this.show_tab(tab, cx)))
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                        this.tab_key(tab, event, window, cx);
                    })),
                palette,
            )
            .handle(&self.tabs.get(&key, cx))
            .tab_stop(chosen)
        });
        div()
            .h(px(60.0))
            .flex_none()
            .px_6()
            .flex()
            .items_center()
            .gap_3()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .debug_selector(|| "teams-page-title".to_owned())
                    .text_xl()
                    .font_weight(gpui::FontWeight::BOLD)
                    .truncate()
                    .child("Your teams and channels"),
            )
            .children(tabs)
            .child(div().flex_1())
            .child(self.create_button(palette, cx))
    }

    /// Left and Right on a tab: the one next to it is shown, the keyboard on it, as Teams' tabs take them.
    fn tab_key(&mut self, tab: PageTab, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let at = PageTab::ALL.iter().position(|shown| *shown == tab).unwrap_or(0);
        let Some(to) = keyboard::step(event, PageTab::ALL.len(), at) else {
            return;
        };
        cx.stop_propagation();
        let tab = PageTab::ALL[to];
        self.show_tab(tab, cx);
        self.tabs.get(&format!("teams-tab-{}", tab.key()), cx).focus(window, cx);
    }

    /// Create team and its dropdown; the read-only mode offers neither.
    fn create_button(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        if self.core.read_only() {
            return div().into_any_element();
        }
        let open = self.popup == Some(Popup::CreateMenu);
        let menu = open.then(|| {
            popup_menu(palette, cx.listener(|this, _, _, cx| this.popup_clicked_out(cx)))
                .keyed(self.popup_focus.root(), self.popup_keys(cx))
                .row(
                    "teams-create-menu-create",
                    menu_row("teams-create-menu-create", palette)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.close_popup(window, cx);
                            cx.emit(TeamsPageEvent::CreateTeam);
                        }))
                        .child(Glyph::USERS.icon().size(px(16.0)))
                        .child("Create team"),
                    palette,
                )
                .row(
                    "teams-create-menu-join",
                    menu_row("teams-create-menu-join", palette)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.close_popup(window, cx);
                            cx.emit(TeamsPageEvent::JoinTeam);
                        }))
                        .child(Glyph::USER_PLUS.icon().size(px(16.0)))
                        .child("Join Team"),
                    palette,
                )
        });
        div()
            .flex_none()
            .flex()
            .items_center()
            .child(keyboard::stop(
                "teams-create",
                div()
                    .id("teams-create")
                    .debug_selector(|| "teams-create".to_owned())
                    .px_2()
                    .py_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded_md()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .cursor_pointer()
                    .hover(move |button| button.bg(rgb(palette.hover)))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(TeamsPageEvent::CreateTeam)))
                    .child(Glyph::USERS.icon().size(px(18.0)))
                    .child("Create team"),
                palette,
            ))
            .child(
                div()
                    .child(selected_by(
                        "teams-create-more",
                        icon_button("teams-create-more", Glyph::CHEVRON_DOWN, "More create team options")
                            .xsmall()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_popup(Popup::CreateMenu, window, cx);
                            })),
                    ))
                    .children(menu),
            )
            .into_any_element()
    }

    /// The filter chips, the sort and the search box.
    fn toolbar(&mut self, palette: Palette, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let search = self.search_box(window, cx);
        let mut chips: Vec<AnyElement> = [Filter::All, Filter::Owned]
            .into_iter()
            .map(|filter| self.chip(filter, palette, cx))
            .collect();
        let others = [Filter::ChannelsHidden, Filter::ChannelsShown, Filter::Archived];
        if others.contains(&self.filter) {
            chips.push(self.chip(self.filter, palette, cx));
        }
        chips.push(self.more_filters(others, palette, cx));
        chips.push(self.sort_button(palette, cx));
        div()
            .flex_none()
            .px_6()
            .pt_6()
            .pb_3()
            .flex()
            .items_center()
            .gap_3()
            .children(chips)
            .child(div().flex_1())
            .child(
                div()
                    .debug_selector(|| "teams-search".to_owned())
                    .w(px(280.0))
                    .flex_none()
                    .child(Input::new(&search).prefix(Glyph::SEARCH.icon().size(px(14.0)))),
            )
    }

    fn chip(&self, filter: Filter, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let chosen = self.filter == filter;
        let key = format!("teams-filter-{}", filter.key());
        keyboard::stop(
            key.clone(),
            chip_base(key, chosen, palette)
                .child(filter.label())
                .on_click(cx.listener(move |this, _, _, cx| this.set_filter(filter, cx))),
            palette,
        )
        .edge()
        .into_any_element()
    }

    /// "N more", whose menu holds what no chip shows.
    fn more_filters(&self, others: [Filter; 3], palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let open = self.popup == Some(Popup::MoreFilters);
        let left: Vec<Filter> = others.into_iter().filter(|filter| *filter != self.filter).collect();
        let menu = open.then(|| {
            let mut menu = popup_menu(palette, cx.listener(|this, _, _, cx| this.popup_clicked_out(cx)))
                .keyed(self.popup_focus.root(), self.popup_keys(cx));
            for filter in left.iter().copied() {
                let key = format!("teams-more-{}", filter.key());
                menu = menu.row(
                    key.clone(),
                    menu_row(key, palette)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.set_filter(filter, cx);
                            this.popup_focus.closed(window, cx);
                        }))
                        .child(filter.label()),
                    palette,
                );
            }
            menu
        });
        div()
            .child(
                keyboard::stop(
                    "teams-filter-more",
                    chip_base("teams-filter-more".to_owned(), false, palette)
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(format!("{} more", left.len()))
                        .child(Glyph::CHEVRON_DOWN.icon().size(px(12.0)))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.toggle_popup(Popup::MoreFilters, window, cx);
                        })),
                    palette,
                )
                .edge(),
            )
            .children(menu)
            .into_any_element()
    }

    fn sort_button(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let open = self.popup == Some(Popup::Sort);
        let current = self.sort;
        let menu = open.then(|| {
            let mut menu = popup_menu(palette, cx.listener(|this, _, _, cx| this.popup_clicked_out(cx)))
                .keyed(self.popup_focus.root(), self.popup_keys(cx));
            for sort in Sort::ALL {
                let key = format!("teams-sort-{}", sort.key());
                menu = menu.row(
                    key.clone(),
                    menu_row(key, palette)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.set_sort(sort, cx);
                            this.popup_focus.closed(window, cx);
                        }))
                        .child(div().flex_1().child(sort.label()))
                        .when(sort == current, |row| row.child(Glyph::CHECK.icon().size(px(14.0)))),
                    palette,
                );
            }
            menu
        });
        div()
            .child(keyboard::stop(
                "teams-sort",
                div()
                    .id("teams-sort")
                    .debug_selector(|| "teams-sort".to_owned())
                    .px_2()
                    .py_1()
                    .flex()
                    .items_center()
                    .gap_1()
                    .rounded_md()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .cursor_pointer()
                    .hover(move |button| button.bg(rgb(palette.hover)))
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_popup(Popup::Sort, window, cx)))
                    .child(format!("Sort: {}", current.label()))
                    .child(Glyph::CHEVRON_DOWN.icon().size(px(12.0))),
                palette,
            ))
            .children(menu)
            .into_any_element()
    }

    /// The Teams tab's rows.
    fn rows(&self, window: &Window, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let teams = self.teams.read(cx);
        let Some(all) = teams.teams() else {
            return centered("Loading teams…", palette);
        };
        let shown: Vec<Team> = listed(all, self.filter, self.sort, &self.query)
            .into_iter()
            .cloned()
            .collect();
        if shown.is_empty() {
            return centered("No teams to show", palette);
        }
        let keyboard = self.rows_focus.is_focused(window);
        let rows: Vec<AnyElement> = shown
            .iter()
            .enumerate()
            .map(|(index, team)| {
                let keyed = keyboard && self.row_cursor.as_deref() == Some(team.id.as_str());
                self.row(index, team, keyed, palette, cx)
            })
            .collect();
        let ids: Vec<String> = shown.iter().map(|team| team.id.clone()).collect();
        div()
            .id("teams-rows")
            .debug_selector(|| "teams-rows".to_owned())
            .track_focus(&self.rows_focus)
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                this.rows_key(&ids, event, window, cx);
            }))
            .flex_1()
            .min_h_0()
            .px_6()
            .overflow_y_scroll()
            .children(rows)
            .into_any_element()
    }

    /// The rows' keys: Up, Down, Home and End move over them; Enter opens Manage team; Shift+F10 or the menu key opens
    /// the row's "..." menu.
    fn rows_key(&mut self, ids: &[String], event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let at = self
            .row_cursor
            .as_ref()
            .and_then(|cursor| ids.iter().position(|id| id == cursor));
        if let Some(to) = keyboard::list_step(event, ids.len(), at) {
            self.row_cursor = ids.get(to).cloned();
        } else if let Some(id) = at.and_then(|at| ids.get(at)).cloned() {
            if keyboard::opens_menu(event) {
                self.open_menu(&id, true, window, cx);
            } else if matches!(event.keystroke.key.as_str(), "enter" | "space") && !event.keystroke.modifiers.modified()
            {
                cx.emit(TeamsPageEvent::Manage(id));
            } else {
                return;
            }
        } else {
            return;
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// Gives the rows the keyboard, on the first unless one is kept; `false` while the Teams tab lists none, the
    /// keyboard left where it is.
    pub fn focus_rows(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let ids: Vec<String> = self
            .teams
            .read(cx)
            .teams()
            .map(|all| {
                listed(all, self.filter, self.sort, &self.query)
                    .into_iter()
                    .map(|team| team.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        if self.tab != PageTab::Teams || ids.is_empty() {
            return false;
        }
        if !self.row_cursor.as_ref().is_some_and(|cursor| ids.contains(cursor)) {
            self.row_cursor = ids.first().cloned();
        }
        self.rows_focus.focus(window, cx);
        cx.notify();
        true
    }

    /// A team's row, its test selector `teams-row-<index>`, ringed while the keyboard is on it.
    fn row(&self, index: usize, team: &Team, keyed: bool, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let id = team.id.clone();
        let group = row_group(index);
        let avatar = self.teams.read(cx).avatar(team, 36.0, true, palette);
        let facts = team_facts(team, palette);
        let hidden = team.about.hidden.then(|| {
            div()
                .debug_selector(move || format!("teams-row-hidden-{index}"))
                .pr_3()
                .border_r_1()
                .border_color(rgb(palette.border))
                .text_color(rgb(palette.muted_foreground))
                .child(Glyph::EYE_OFF.icon().size(px(18.0)))
        });
        let archived = team.archived.then(|| {
            div()
                .debug_selector(move || format!("teams-row-archived-{index}"))
                .flex_none()
                .child(Glyph::ARCHIVE.icon().size(px(14.0)))
        });
        let opened = id.clone();
        let more = self.row_more(index, team, keyed, palette, cx);
        div()
            .group(group)
            .id(SharedString::from(format!("teams-row-{index}")))
            .debug_selector(move || format!("teams-row-{index}"))
            .h(px(61.0))
            .px_2()
            .flex()
            .items_center()
            .gap_3()
            .border_b_1()
            .border_color(rgb(palette.border))
            // the keyboard's ring, over the row (its own test selector kept)
            .when(keyed, |row| {
                row.relative().child(
                    div()
                        .debug_selector(|| "teams-row-cursor".to_owned())
                        .absolute()
                        .inset_0()
                        .rounded_md()
                        .border_2()
                        .border_color(rgb(palette.brand)),
                )
            })
            .cursor_pointer()
            .hover(move |row| row.bg(rgb(palette.hover)))
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(TeamsPageEvent::Manage(opened.clone()))))
            .child(avatar)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .truncate()
                                    .child(team.name.clone()),
                            )
                            .children(archived),
                    )
                    .child(facts),
            )
            .children(hidden)
            .child(more)
            .child(
                div()
                    .flex_none()
                    .debug_selector(move || format!("teams-row-role-{index}"))
                    .child(role_of(team)),
            )
            .into_any_element()
    }

    /// A team row's "..." and its menu ([`team_menu`]), shown while the row is under the pointer or its menu is open.
    fn row_more(&self, index: usize, team: &Team, keyed: bool, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let id = team.id.clone();
        let group = row_group(index);
        let open = self.menu.as_deref() == Some(id.as_str());
        let menu = open.then(|| {
            let read_only = self.core.read_only();
            let weak = cx.entity().downgrade();
            let (chosen_from, closed, keys) = (weak.clone(), weak.clone(), weak);
            let chosen_team = id.clone();
            let shown = team_menu::entries(team, read_only);
            team_menu::Panel {
                index,
                team,
                read_only,
                highlight: self.menu_highlight,
                focus: &self.menu_focus,
                palette,
                anchor: gpui::Anchor::TopRight,
            }
            .render(
                move |choice, window, cx| {
                    let _ = chosen_from.update(cx, |this, cx| this.choose(&chosen_team, choice, window, cx));
                },
                move |_, _, cx| {
                    // a click elsewhere takes the focus where it clicked
                    let _ = closed.update(cx, |this, cx| {
                        this.menu = None;
                        cx.notify();
                    });
                },
                {
                    let id = id.clone();
                    move |event: &KeyDownEvent, window, cx| {
                        let _ = keys.update(cx, |this, cx| this.menu_key(&id, &shown, event, window, cx));
                    }
                },
            )
        });
        let toggled = id.clone();
        div()
            .id(SharedString::from(format!("teams-row-more-wrap-{index}")))
            .flex_none()
            // and while the keyboard is on the row
            .when(!open && !keyed, |more| {
                more.invisible().group_hover(group.clone(), gpui::Styled::visible)
            })
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(selected_by(
                format!("teams-row-more-{index}"),
                icon_button(
                    SharedString::from(format!("teams-row-more-button-{index}")),
                    Glyph::ELLIPSIS,
                    "More options",
                )
                .small()
                // the rows take the keyboard, Shift+F10 opening this menu
                .tab_stop(false)
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    if this.menu.as_deref() == Some(toggled.as_str()) {
                        this.close_menu(window, cx);
                    } else {
                        this.open_menu(&toggled, false, window, cx);
                    }
                })),
            ))
            .children(menu)
            .into_any_element()
    }

    /// The Pending invites tab: what's waiting, or what will show up here.
    fn invites_tab(&self, palette: Palette) -> AnyElement {
        let Some(invites) = &self.invites else {
            return centered("Loading…", palette);
        };
        if invites.is_empty() {
            return centered(
                "When people invite your team to a shared channel, the invite will show up here.",
                palette,
            );
        }
        div()
            .id("teams-invites")
            .debug_selector(|| "teams-invites".to_owned())
            .flex_1()
            .min_h_0()
            .p_6()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .child(
                table_row(
                    ["Shared channel", "Team", "Sent by"]
                        .map(std::borrow::ToOwned::to_owned)
                        .to_vec(),
                    palette,
                )
                .font_weight(gpui::FontWeight::SEMIBOLD),
            )
            .children(invites.iter().map(|invite| {
                table_row(
                    vec![invite.channel.clone(), invite.team.clone(), invite.from.clone()],
                    palette,
                )
            }))
            .into_any_element()
    }

    /// The Analytics tab: its time period and a line per team.
    fn analytics_tab(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let table: AnyElement = match &self.analytics {
            None => centered("Loading…", palette),
            Some(rows) if rows.is_empty() => centered("There's nothing to show for this time period yet.", palette),
            Some(rows) => div()
                .flex()
                .flex_col()
                .child(
                    table_row(
                        ["Name", "Active users", "Active guests", "Posts", "Replies"]
                            .map(std::borrow::ToOwned::to_owned)
                            .to_vec(),
                        palette,
                    )
                    .font_weight(gpui::FontWeight::SEMIBOLD),
                )
                .children(rows.iter().map(|row| {
                    table_row(
                        vec![
                            row.name.clone(),
                            row.active_users.to_string(),
                            row.guests.to_string(),
                            row.posts.to_string(),
                            row.replies.to_string(),
                        ],
                        palette,
                    )
                }))
                .into_any_element(),
        };
        div()
            .id("teams-analytics")
            .debug_selector(|| "teams-analytics".to_owned())
            .flex_1()
            .min_h_0()
            .p_6()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_4()
            .child(self.period_picker(palette, cx))
            .child(table)
            .into_any_element()
    }

    fn period_picker(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let open = self.popup == Some(Popup::Period);
        let current = self.period;
        let menu = open.then(|| {
            let mut menu = popup_menu(palette, cx.listener(|this, _, _, cx| this.popup_clicked_out(cx)))
                .keyed(self.popup_focus.root(), self.popup_keys(cx));
            for period in [
                AnalyticsPeriod::LastSevenDays,
                AnalyticsPeriod::LastThirtyDays,
                AnalyticsPeriod::LastNinetyDays,
            ] {
                let key = format!("teams-period-{}", period.wire());
                menu = menu.row(
                    key.clone(),
                    menu_row(key, palette)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.period = period;
                            this.close_popup(window, cx);
                            this.read_analytics(cx);
                        }))
                        .child(div().flex_1().child(period.label()))
                        .when(period == current, |row| row.child(Glyph::CHECK.icon().size(px(14.0)))),
                    palette,
                );
            }
            menu
        });
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_sm().child("Time period"))
            .child(
                div()
                    .child(
                        keyboard::stop(
                            "teams-period",
                            div()
                                .id("teams-period")
                                .debug_selector(|| "teams-period".to_owned())
                                .w(px(240.0))
                                .px_3()
                                .py_1p5()
                                .flex()
                                .items_center()
                                .justify_between()
                                .rounded_md()
                                .border_1()
                                .border_color(rgb(palette.border))
                                .cursor_pointer()
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.toggle_popup(Popup::Period, window, cx)),
                                )
                                .child(current.label())
                                .child(Glyph::CHEVRON_DOWN.icon().size(px(14.0))),
                            palette,
                        )
                        .edge(),
                    )
                    .children(menu),
            )
            .into_any_element()
    }
}

/// A tab of a page's header: its word, a line under the chosen one.
pub(crate) fn tab_button(id: String, label: &'static str, chosen: bool, palette: Palette) -> gpui::Stateful<gpui::Div> {
    let selector = id.clone();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .h_full()
        .px_2()
        .flex()
        .items_center()
        .flex_none()
        .cursor_pointer()
        .border_b_2()
        .border_color(rgb(if chosen { palette.brand } else { palette.background }))
        .when(chosen, |tab| tab.font_weight(gpui::FontWeight::BOLD))
        .when(!chosen, |tab| tab.text_color(rgb(palette.muted_foreground)))
        .hover(move |tab| tab.text_color(rgb(palette.foreground)))
        .child(label)
}

/// A filter chip's face: a pill, tinted while chosen.
pub(crate) fn chip_base(id: String, chosen: bool, palette: Palette) -> gpui::Stateful<gpui::Div> {
    let selector = id.clone();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .px_4()
        .py_1p5()
        .rounded_full()
        .flex_none()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .cursor_pointer()
        .border_1()
        .border_color(rgb(if chosen { palette.brand } else { palette.background }))
        .when(chosen, |chip| chip.text_color(rgb(palette.brand)))
        .hover(move |chip| chip.bg(rgb(palette.hover)))
}

/// A popup of menu rows under what opened it, over everything.
pub(crate) struct PopupMenu(gpui::Div);

impl PopupMenu {
    /// Takes the keyboard on `root` with `keys` ([`keyboard::menu_keys`]).
    pub(crate) fn keyed(
        self,
        root: &FocusHandle,
        keys: impl Fn(&KeyDownEvent, &mut Window, &mut gpui::App) + 'static,
    ) -> Self {
        Self(self.0.track_focus(root).on_key_down(keys))
    }

    /// A row the keyboard moves to, `key` unique in the menu.
    pub(crate) fn row(self, key: impl Into<SharedString>, row: gpui::Stateful<gpui::Div>, palette: Palette) -> Self {
        Self(self.0.child(keyboard::stop(ElementId::Name(key.into()), row, palette)))
    }
}

impl IntoElement for PopupMenu {
    type Element = gpui::Deferred;

    fn into_element(self) -> Self::Element {
        gpui::deferred(gpui::anchored().snap_to_window_with_margin(px(8.0)).child(self.0)).with_priority(1)
    }
}

/// A [`PopupMenu`]: `close` is told a click outside.
pub(crate) fn popup_menu(
    palette: Palette,
    close: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
) -> PopupMenu {
    PopupMenu(
        div()
            .occlude()
            .min_w(px(200.0))
            .mt_1()
            .p_1()
            .flex()
            .flex_col()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .shadow_lg()
            .text_color(rgb(palette.foreground))
            .on_mouse_down_out(close),
    )
}

/// A team's line of facts: Private, N people, N channels, its description, split by hairlines.
pub(crate) fn team_facts(team: &Team, palette: Palette) -> gpui::Div {
    let mut facts = vec![
        if team.about.public { "Public" } else { "Private" }.to_owned(),
        format!("{} people", team.about.people),
        format!("{} channels", team.channels.len()),
    ];
    if !team.about.description.is_empty() {
        facts.push(team.about.description.clone());
    }
    let last = facts.len() - 1;
    div()
        .min_w_0()
        .flex()
        .items_center()
        .gap_2()
        .text_xs()
        .text_color(rgb(palette.muted_foreground))
        .children(facts.into_iter().enumerate().map(|(at, fact)| {
            div()
                .when(at < last, |fact| {
                    fact.flex_none().pr_2().border_r_1().border_color(rgb(palette.border))
                })
                .when(at == last, |fact| fact.min_w_0().truncate())
                .child(fact)
        }))
}

/// The account's role on a team, as the page words it.
#[must_use]
pub fn role_of(team: &Team) -> &'static str {
    if team.owner {
        "Owner"
    } else if team.about.guest {
        "Guest"
    } else {
        "Member"
    }
}

/// A sentence in the middle of what is left of the page.
pub(crate) fn centered(words: &'static str, palette: Palette) -> AnyElement {
    div()
        .debug_selector(|| "teams-page-note".to_owned())
        .flex_1()
        .min_h_0()
        .flex()
        .items_center()
        .justify_center()
        .text_color(rgb(palette.muted_foreground))
        .child(words)
        .into_any_element()
}

/// A line of a table of even columns.
pub(crate) fn table_row(cells: Vec<String>, palette: Palette) -> gpui::Div {
    div()
        .py_2()
        .flex()
        .items_center()
        .gap_3()
        .border_b_1()
        .border_color(rgb(palette.border))
        .children(
            cells
                .into_iter()
                .map(|cell| div().flex_1().min_w_0().truncate().child(cell)),
        )
}

impl Render for TeamsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let body = match self.tab {
            PageTab::Teams => div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(self.toolbar(palette, window, cx))
                .child(self.rows(window, palette, cx))
                .into_any_element(),
            PageTab::PendingInvites => self.invites_tab(palette),
            PageTab::Analytics => self.analytics_tab(palette, cx),
        };
        div()
            .debug_selector(|| "teams-page".to_owned())
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(palette.background))
            .text_color(rgb(palette.foreground))
            .child(self.header(palette, cx))
            .child(body)
    }
}

/// The hover group of a team's row, which shows its "...".
fn row_group(index: usize) -> SharedString {
    SharedString::from(format!("teams-row-group-{index}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use teams_store::TeamAbout;

    fn team(name: &str, owner: bool, hidden: bool, archived: bool) -> Team {
        Team {
            id: name.to_owned(),
            name: name.to_owned(),
            owner,
            archived,
            about: TeamAbout {
                hidden,
                description: format!("about {name}"),
                ..TeamAbout::default()
            },
            ..Team::default()
        }
    }

    fn names<'a>(listed: &[&'a Team]) -> Vec<&'a str> {
        listed.iter().map(|team| team.name.as_str()).collect()
    }

    #[test]
    fn the_filters_sort_and_search_list_what_teams_lists() {
        let teams = vec![
            team("Zeta", true, false, false),
            team("alpha", false, true, true),
            team("Beta", false, false, false),
        ];
        assert_eq!(
            names(&listed(&teams, Filter::All, Sort::AToZ, "")),
            ["alpha", "Beta", "Zeta"]
        );
        assert_eq!(
            names(&listed(&teams, Filter::All, Sort::ZToA, "")),
            ["Zeta", "Beta", "alpha"]
        );
        assert_eq!(names(&listed(&teams, Filter::Owned, Sort::AToZ, "")), ["Zeta"]);
        assert_eq!(
            names(&listed(&teams, Filter::ChannelsHidden, Sort::AToZ, "")),
            ["alpha"]
        );
        assert_eq!(
            names(&listed(&teams, Filter::ChannelsShown, Sort::AToZ, "")),
            ["Beta", "Zeta"]
        );
        assert_eq!(names(&listed(&teams, Filter::Archived, Sort::AToZ, "")), ["alpha"]);
        assert_eq!(
            names(&listed(&teams, Filter::All, Sort::Membership, "")),
            ["Zeta", "alpha", "Beta"]
        );
        assert_eq!(names(&listed(&teams, Filter::All, Sort::AToZ, "ABOUT b")), ["Beta"]);
    }
}
