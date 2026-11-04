//! The Workflows app's own pages in the main window, as browser Teams shows them (`docs/protocol/workflows.md`, "The
//! Workflows app"): Home (Find templates with its suggestions, Build from scratch, "Create a workflow" with the app's
//! sixteen featured templates eight at a time, and "Your workflows" with Refresh and See all), Templates (the whole
//! gallery by kind — All, Take action, Get updates, Schedule — and by app, under Filter) and "Your workflows" (Find
//! workflows; All, Active, Inactive). A template opens the same card editor as the channel's dialog, with no team
//! chosen; a workflow opens the same Overview.
use std::collections::BTreeSet;

use super::*;
use gpui_component::checkbox::Checkbox;
use model::TemplateKind;

/// The content's width on the app's pages, px, as Teams lays them out.
const PAGE_WIDTH: f32 = 1120.0;
/// The card editor's and the builder's width, px.
const EDITOR_WIDTH: f32 = 760.0;
/// Home's templates a page: two rows of four.
const HOME_PAGE: usize = 8;
/// The most workflows Home lists, as the app reads them.
const HOME_FLOWS: usize = 5;
/// The template gallery the app's Home reads, and the one its Templates and search read.
const HOME_GALLERY: &str = "neptune_teams_pa_home";
const ALL_GALLERY: &str = "home_featured";
/// The most templates Find templates suggests, as Teams lists them.
const SUGGESTED: usize = 4;

/// The Workflows app's tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppTab {
    Home,
    Templates,
    Chat,
}

/// "Your workflows"' filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FlowFilter {
    All,
    Active,
    Inactive,
}

/// A gallery read: its templates, or why there are none.
#[derive(Default)]
pub(super) enum Gallery {
    #[default]
    Loading,
    Ready(Vec<Card>),
    Failed(WorkflowFailure),
}

/// What the app keeps between renders.
pub(super) struct AppState {
    pub(super) tab: AppTab,
    pub(super) home: Gallery,
    pub(super) all: Gallery,
    /// Home's page of templates.
    pub(super) page: usize,
    /// The Templates tab's kind (none: All) and the apps its Filter keeps.
    pub(super) kind: Option<TemplateKind>,
    pub(super) apps: BTreeSet<&'static str>,
    pub(super) filter_open: bool,
    /// "Find templates" and what the gallery found for its text.
    pub(super) find: Entity<InputState>,
    pub(super) found: Option<(String, Gallery)>,
    pub(super) asked: u64,
    pub(super) suggesting: bool,
    /// "Find workflows" and its filter.
    pub(super) find_flows: Entity<InputState>,
    pub(super) flows_suggesting: bool,
    pub(super) flows: FlowFilter,
    _subscriptions: Vec<Subscription>,
}

impl AppState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<WorkflowDialog>) -> Self {
        let find = cx.new(|cx| InputState::new(window, cx).placeholder("Find templates"));
        let find_flows = cx.new(|cx| InputState::new(window, cx).placeholder("Find workflows"));
        let subscriptions = vec![
            cx.subscribe(&find, |this: &mut WorkflowDialog, _, event, cx| match event {
                InputEvent::Change => this.find_templates(cx),
                InputEvent::Focus => this.suggest(true, cx),
                InputEvent::Blur => this.suggest(false, cx),
                InputEvent::PressEnter { .. } => {}
            }),
            cx.subscribe(&find_flows, |this: &mut WorkflowDialog, _, event, cx| {
                if let Some(app) = &mut this.app {
                    app.flows_suggesting = match event {
                        InputEvent::Change | InputEvent::Focus => true,
                        InputEvent::Blur => false,
                        InputEvent::PressEnter { .. } => app.flows_suggesting,
                    };
                }
                cx.notify();
            }),
        ];
        Self {
            tab: AppTab::Home,
            home: Gallery::Loading,
            all: Gallery::Loading,
            page: 0,
            kind: None,
            apps: BTreeSet::new(),
            filter_open: false,
            find,
            found: None,
            asked: 0,
            suggesting: false,
            find_flows,
            flows_suggesting: false,
            flows: FlowFilter::All,
            _subscriptions: subscriptions,
        }
    }
}

impl WorkflowDialog {
    /// The app's tab shown, when this is the Workflows app.
    #[must_use]
    pub fn app_tab(&self) -> Option<AppTab> {
        self.app.as_ref().map(|app| app.tab)
    }

    /// Shows one of the app's tabs, as its header's tabs do: Home and Templates are pages here, Chat is the app's
    /// (its bot's chat, drawn by the page around this).
    pub fn show_tab(&mut self, tab: AppTab, cx: &mut Context<Self>) {
        let Some(app) = &mut self.app else {
            return;
        };
        app.filter_open = false;
        // The bot's chat is drawn over the pages, which stay as they are (an open card editor keeps what was typed).
        if tab == AppTab::Chat {
            cx.notify();
            return;
        }
        app.tab = tab;
        if !self.busy {
            self.simple = None;
            self.error = None;
            self.screen = match tab {
                AppTab::Templates => Screen::Templates,
                AppTab::Home | AppTab::Chat => Screen::Home,
            };
        }
        cx.notify();
    }

    /// The app's "Reload app": the app starts again on its tab, its templates and workflows read again. Not while a
    /// change is under way, whose answer the reload would drop.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if let Some(app) = &mut self.app {
            app.home = Gallery::Loading;
            app.all = Gallery::Loading;
        }
        self.simple = None;
        self.error = None;
        self.screen = self.start_screen();
        self.refresh(cx);
    }

    /// The app is shown again: a list it shows (Home's, Templates' or "Your workflows") is read again, as changes made
    /// elsewhere (the channel's Workflows) may have left it behind. An open card editor or Overview stays as it is.
    pub fn reread_workflows(&mut self, cx: &mut Context<Self>) {
        if !self.busy && matches!(self.screen, Screen::Home | Screen::Templates | Screen::AllWorkflows) {
            self.load_workflows(cx);
        }
    }

    /// Home's "See all": the app's "Your workflows".
    pub fn show_all_workflows(&mut self, cx: &mut Context<Self>) {
        if self.app.is_some() && !self.busy {
            if let Some(app) = &mut self.app {
                app.tab = AppTab::Home;
            }
            self.screen = Screen::AllWorkflows;
            cx.notify();
        }
    }

    /// Opens a template the app's gallery listed, as a click on its card does.
    pub fn open_app_template(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let card = self.app.as_ref().and_then(|app| {
            [&app.home, &app.all].into_iter().find_map(|gallery| match gallery {
                Gallery::Ready(cards) => cards.iter().find(|card| card.id == id).cloned(),
                _ => None,
            })
        });
        if let Some(card) = card {
            self.configure_template(card, window, cx);
        }
    }

    /// Home's next page of templates, as its next arrow shows it.
    pub fn next_templates(&mut self, cx: &mut Context<Self>) {
        if let Some(app) = &mut self.app
            && let Gallery::Ready(cards) = &app.home
            && (app.page + 1) * HOME_PAGE < cards.len()
        {
            app.page += 1;
            cx.notify();
        }
    }

    /// Opens the Templates tab's Filter, as a click on it does.
    pub fn open_filter(&mut self, cx: &mut Context<Self>) {
        if let Some(app) = &mut self.app {
            app.filter_open = true;
            cx.notify();
        }
    }

    /// Types `text` into "Find templates", as the user does.
    pub fn find_template(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(app) = &self.app {
            let find = app.find.clone();
            find.update(cx, |state, cx| {
                state.set_value(text.to_owned(), window, cx);
                state.focus(window, cx);
            });
        }
        self.find_templates(cx);
        self.suggest(true, cx);
    }

    /// Reads the app's two galleries: Home's featured templates and the whole gallery its Templates shows.
    pub(super) fn load_gallery(&mut self, cx: &mut Context<Self>) {
        let generation = self.generation;
        for (category, top, home) in [(HOME_GALLERY, 16, true), (ALL_GALLERY, 120, false)] {
            let reply = self.core.workflow_request(WorkflowRequest::Gallery {
                category: category.to_owned(),
                search: None,
                top,
            });
            self.tasks
                .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                    let answer = reply.await.unwrap_or(Err(WorkflowFailure::Connection));
                    let _ = this.update(cx, |this, cx| this.gallery_read(generation, home, answer, cx));
                }));
        }
    }

    fn gallery_read(&mut self, generation: u64, home: bool, answer: WorkflowResponse, cx: &mut Context<Self>) {
        if self.generation != generation {
            return;
        }
        let read = match answer.and_then(|value| model::gallery(&value)) {
            Ok(cards) => Gallery::Ready(cards),
            Err(error) => Gallery::Failed(error),
        };
        match &mut self.app {
            Some(app) if home => app.home = read,
            Some(app) => app.all = read,
            None => {}
        }
        cx.notify();
    }

    /// Checks or unchecks an app in the Templates tab's Filter.
    fn filter_app(&mut self, api: &'static str, on: bool, cx: &mut Context<Self>) {
        if let Some(app) = &mut self.app {
            if on {
                app.apps.insert(api);
            } else {
                app.apps.remove(api);
            }
            cx.notify();
        }
    }

    /// Opens a workflow "Find workflows" suggested.
    fn open_found_flow(&mut self, card: Card, cx: &mut Context<Self>) {
        if let Some(app) = &mut self.app {
            app.flows_suggesting = false;
        }
        self.listing = Listing::All;
        self.open_flow(card, cx);
    }

    /// Asks the gallery for the templates matching "Find templates"' text, as the app does while it is typed.
    fn find_templates(&mut self, cx: &mut Context<Self>) {
        let Some(app) = &mut self.app else {
            return;
        };
        let text = app.find.read(cx).value().trim().to_owned();
        app.asked += 1;
        let asked = app.asked;
        if text.is_empty() {
            app.found = None;
            cx.notify();
            return;
        }
        app.found = Some((text.clone(), Gallery::Loading));
        app.suggesting = true;
        let reply = self.core.workflow_request(WorkflowRequest::Gallery {
            category: ALL_GALLERY.to_owned(),
            search: Some(text.clone()),
            top: 10,
        });
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = reply.await.unwrap_or(Err(WorkflowFailure::Connection));
            let _ = this.update(cx, |this, cx| {
                if let Some(app) = &mut this.app
                    && app.asked == asked
                {
                    let read = match answer.and_then(|value| model::found(&value)) {
                        Ok(cards) => Gallery::Ready(cards),
                        Err(error) => Gallery::Failed(error),
                    };
                    app.found = Some((text, read));
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn suggest(&mut self, shown: bool, cx: &mut Context<Self>) {
        if let Some(app) = &mut self.app {
            app.suggesting = shown;
            cx.notify();
        }
    }

    /// "Explore templates" under the suggestions: the Templates tab, the search cleared.
    fn explore_templates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(app) = &mut self.app {
            app.found = None;
            app.suggesting = false;
            let find = app.find.clone();
            find.update(cx, |state, cx| state.set_value(String::new(), window, cx));
        }
        self.show_tab(AppTab::Templates, cx);
    }
}

/// The app's pages.
impl WorkflowDialog {
    /// The page in the main window: one scrolling column, the question asked over it.
    pub(super) fn app_page(&self, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let editing = matches!(self.screen, Screen::Edit { .. } | Screen::Build { .. });
        let footer = matches!(self.screen, Screen::RunDetails { .. } | Screen::RunInputs { .. });
        div()
            .id("workflow-app")
            .debug_selector(|| "workflow-app".to_owned())
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(if editing { palette.background } else { palette.surface }))
            .text_color(rgb(palette.foreground))
            .key_context("WorkflowDialog")
            .track_focus(&self.focus)
            .child(
                div()
                    .id("workflow-scroll")
                    .debug_selector(|| "workflow-scroll".to_owned())
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(
                        div().w_full().flex().justify_center().px_8().pt_8().pb_12().child(
                            div()
                                .w_full()
                                .max_w(px(if editing { EDITOR_WIDTH } else { PAGE_WIDTH }))
                                .child(self.app_body(palette, cx)),
                        ),
                    ),
            )
            .child(self.status(cx))
            .when(footer, |page| page.child(self.footer(palette, cx)))
            .children(self.prompt(palette, cx))
            .into_any_element()
    }

    fn app_body(&self, palette: Palette, cx: &Context<Self>) -> AnyElement {
        match &self.screen {
            Screen::Detail(card) | Screen::Delete(card) => div()
                .flex()
                .flex_col()
                .gap_4()
                .child(self.page_back("workflow-details-back", "Workflow details", cx))
                .child(self.detail(card, palette, cx))
                .into_any_element(),
            _ => self.body(palette, cx),
        }
    }

    /// A page's title with Teams' back chevron, leading back to the list it came from.
    fn page_back(&self, id: &'static str, title: &'static str, cx: &Context<Self>) -> AnyElement {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(selected_by(
                id,
                icon_button(id, Glyph::CHEVRON_LEFT, "Back")
                    .ghost()
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.error = None;
                        this.screen = match this.screen {
                            Screen::Detail(_) | Screen::Delete(_) => this.list_screen_or_start(),
                            _ => this.start_screen(),
                        };
                        cx.notify();
                    })),
            ))
            .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
            .into_any_element()
    }

    /// Where an Overview's Back leads in the app: the list its row was in, else the tab.
    fn list_screen_or_start(&self) -> Screen {
        match self.listing {
            Listing::All => Screen::AllWorkflows,
            _ => self.start_screen(),
        }
    }

    /// Home: find and build, eight of the app's featured templates, and the account's latest workflows.
    pub(super) fn app_home(&self, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let Some(app) = &self.app else {
            return div().into_any_element();
        };
        let mut section = div()
            .flex()
            .flex_col()
            .gap_4()
            .child(self.app_search_row(cx))
            .child(Self::home_heading(app, cx));
        section = match &app.home {
            Gallery::Loading => section.child(state("Loading templates…", "", palette)),
            Gallery::Failed(error) => {
                section.child(state("Templates could not be loaded", model::problem(error), palette))
            }
            Gallery::Ready(cards) => section.child(Self::template_grid(
                &cards
                    .iter()
                    .skip(app.page * HOME_PAGE)
                    .take(HOME_PAGE)
                    .collect::<Vec<_>>(),
                palette,
                cx,
            )),
        };
        div()
            .flex()
            .flex_col()
            .gap_8()
            .child(section)
            .child(self.home_workflows(palette, cx))
            .into_any_element()
    }

    /// "Create a workflow", with "All templates" and the two arrows through Home's pages.
    fn home_heading(app: &AppState, cx: &Context<Self>) -> AnyElement {
        let pages = match &app.home {
            Gallery::Ready(cards) => cards.len().div_ceil(HOME_PAGE).max(1),
            _ => 1,
        };
        div()
            .flex()
            .items_center()
            .justify_between()
            .child(div().font_weight(gpui::FontWeight::BOLD).child("Create a workflow"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(selected_by(
                        "workflow-all-templates",
                        Button::new("workflow-all-templates")
                            .ghost()
                            .label("All templates")
                            .text_color(rgb(theme::current(cx).brand))
                            .on_click(cx.listener(|this, _, _, cx| this.show_tab(AppTab::Templates, cx))),
                    ))
                    .child(selected_by(
                        "workflow-previous",
                        icon_button("workflow-previous", Glyph::CHEVRON_LEFT, "Previous templates")
                            .disabled(app.page == 0)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(app) = &mut this.app {
                                    app.page = app.page.saturating_sub(1);
                                }
                                cx.notify();
                            })),
                    ))
                    .child(selected_by(
                        "workflow-next",
                        icon_button("workflow-next", Glyph::CHEVRON_RIGHT, "Next templates")
                            .disabled(app.page + 1 >= pages)
                            .on_click(cx.listener(|this, _, _, cx| this.next_templates(cx))),
                    )),
            )
            .into_any_element()
    }

    /// Find templates (with its suggestions) and Build from scratch.
    fn app_search_row(&self, cx: &Context<Self>) -> AnyElement {
        let Some(app) = &self.app else {
            return div().into_any_element();
        };
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_4()
            .child(
                div()
                    .relative()
                    .w(px(700.0))
                    .child(selected_by(
                        "workflow-search",
                        Input::new(&app.find).prefix(Glyph::SEARCH.icon()).cleanable(true),
                    ))
                    .when_some(
                        app.found
                            .as_ref()
                            .filter(|_| app.suggesting)
                            .map(|(_, found)| Self::suggestions(found, cx)),
                        |find, list| {
                            find.child(
                                gpui::deferred(div().absolute().top(px(40.0)).left_0().right_0().child(list))
                                    .with_priority(1),
                            )
                        },
                    ),
            )
            .when(!self.core.read_only(), |row| {
                row.child(selected_by(
                    "workflow-build",
                    Button::new("workflow-build")
                        .outline()
                        .label("Build from scratch")
                        .icon(Glyph::PLUS.icon())
                        .on_click(cx.listener(|this, _, _, cx| this.start_build(cx))),
                ))
            })
            .into_any_element()
    }

    /// The templates the gallery found for the text typed, then "Explore templates".
    fn suggestions(found: &Gallery, cx: &Context<Self>) -> AnyElement {
        let palette = theme::current(cx);
        let mut list = div()
            .debug_selector(|| "workflow-suggestions".to_owned())
            .flex()
            .flex_col()
            .py_2()
            .rounded_md()
            .bg(rgb(palette.surface))
            .border_1()
            .border_color(rgb(palette.border))
            .shadow_lg()
            .occlude()
            .on_mouse_down(gpui::MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            })
            .child(
                div()
                    .px_5()
                    .py_2()
                    .text_sm()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("Templates"),
            );
        list = match found {
            Gallery::Loading => list.child(div().px_5().py_2().child("Searching…")),
            Gallery::Failed(error) => list.child(div().px_5().py_2().child(model::problem(error))),
            Gallery::Ready(cards) if cards.is_empty() => {
                list.child(div().px_5().py_2().child("We didn't find any matches"))
            }
            Gallery::Ready(cards) => list.children(cards.iter().take(SUGGESTED).map(|card| {
                let chosen = card.clone();
                let selector = format!("workflow-suggestion-{}", card.id);
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_5()
                    .py_1p5()
                    .cursor_pointer()
                    .hover(|row| row.bg(rgb(palette.hover)))
                    .child(Self::kind_mark(card))
                    .child(div().truncate().child(card.name.clone()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.suggest(false, cx);
                        this.configure_template(chosen.clone(), window, cx);
                    }))
            })),
        };
        list.child(div().my_2().h(px(1.0)).bg(rgb(palette.border)))
            .child(
                div()
                    .id("workflow-explore-templates")
                    .debug_selector(|| "workflow-explore-templates".to_owned())
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_5()
                    .py_1p5()
                    .cursor_pointer()
                    .hover(|row| row.bg(rgb(palette.hover)))
                    .child(Glyph::SEARCH.icon().size(px(16.0)))
                    .child("Explore templates")
                    .on_click(cx.listener(|this, _, window, cx| this.explore_templates(window, cx))),
            )
            .into_any_element()
    }

    /// Templates four to a row, the last row's empty places kept so every card is as wide.
    pub(super) fn template_grid(cards: &[&Card], palette: Palette, cx: &Context<Self>) -> AnyElement {
        let mut grid = div().flex().flex_col().gap_5();
        for row in cards.chunks(4) {
            grid = grid.child(
                div()
                    .flex()
                    .gap_5()
                    .children(row.iter().map(|card| Self::template_card(card, 136.0, palette, cx)))
                    .children((row.len()..4).map(|_| div().flex_1())),
            );
        }
        grid.into_any_element()
    }

    /// Home's "Your workflows": the latest few, See all, Refresh, or the app's empty state.
    fn home_workflows(&self, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let cards: Vec<_> = self
            .workflows
            .iter()
            .filter(|card| !card.is_ai())
            .take(HOME_FLOWS)
            .collect();
        let listed = !cards.is_empty();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().font_weight(gpui::FontWeight::BOLD).child("Your workflows"))
                    .when(listed, |heading| {
                        heading.child(selected_by(
                            "workflow-see-all",
                            Button::new("workflow-see-all")
                                .ghost()
                                .label("See all")
                                .text_color(rgb(palette.brand))
                                .on_click(cx.listener(|this, _, _, cx| this.show_all_workflows(cx))),
                        ))
                    }),
            )
            .child(div().flex().justify_end().child(Self::refresh_button(self.busy, cx)))
            .child(self.flow_rows(&cards, palette, cx))
            .into_any_element()
    }

    fn refresh_button(busy: bool, cx: &Context<Self>) -> AnyElement {
        selected_by(
            "workflow-refresh",
            Button::new("workflow-refresh")
                .ghost()
                .icon(Glyph::REFRESH.icon())
                .label("Refresh")
                .disabled(busy)
                .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
        )
        .into_any_element()
    }

    /// Workflow rows, or what the list holds instead: loading, an error, or the app's empty state.
    fn flow_rows(&self, cards: &[&Card], palette: Palette, cx: &Context<Self>) -> AnyElement {
        if self.loading.workflows && self.workflows.is_empty() {
            return state("Loading your workflows…", "", palette).into_any_element();
        }
        if let Some(error) = &self.workflows_error {
            return state("Your workflows could not be loaded", model::problem(error), palette).into_any_element();
        }
        if cards.is_empty() {
            return Self::inspiration(palette);
        }
        div()
            .flex()
            .flex_col()
            .children(cards.iter().map(|card| self.workflow_row(card, palette, cx)))
            .into_any_element()
    }

    /// The app's empty list: "Need more automation inspiration?".
    fn inspiration(palette: Palette) -> AnyElement {
        div()
            .debug_selector(|| "workflow-inspiration".to_owned())
            .flex()
            .flex_col()
            .items_center()
            .gap_2()
            .py_4()
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("Need more automation inspiration?"),
            )
            .child(
                div()
                    .text_color(rgb(palette.foreground))
                    .child("Explore how our templates can automate your repetitive tasks"),
            )
            // Teams' illustration lives in the app's own bundle; the app's icon on its white tile stands in for it
            .child(
                div()
                    .mt_4()
                    .size(px(120.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_xl()
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(0x00ff_ffff))
                    .child(gpui::img(art::workflows()).size(px(96.0))),
            )
            .into_any_element()
    }

    /// Templates: find and build, the kinds, Filter, and every template of the gallery that passes them.
    pub(super) fn app_templates(&self, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let Some(app) = &self.app else {
            return div().into_any_element();
        };
        let body = match &app.all {
            Gallery::Loading => state("Loading templates…", "", palette).into_any_element(),
            Gallery::Failed(error) => {
                state("Templates could not be loaded", model::problem(error), palette).into_any_element()
            }
            Gallery::Ready(cards) => {
                let shown: Vec<_> = cards
                    .iter()
                    .filter(|card| app.kind.is_none_or(|kind| card.is_kind(kind)))
                    .filter(|card| app.apps.is_empty() || card.connectors().iter().any(|api| app.apps.contains(api)))
                    .collect();
                if shown.is_empty() {
                    state(
                        "No matching templates",
                        "Try another kind or app, or build a workflow from scratch.",
                        palette,
                    )
                    .into_any_element()
                } else {
                    Self::template_grid(&shown, palette, cx)
                }
            }
        };
        div()
            .flex()
            .flex_col()
            .gap_6()
            .child(self.app_search_row(cx))
            .child(Self::kinds_row(app, palette, cx))
            .child(body)
            .into_any_element()
    }

    /// The Templates tab's kinds as Teams' pills, and its Filter.
    fn kinds_row(app: &AppState, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let kinds = [
            (None, "All", "workflow-kind-all"),
            (Some(TemplateKind::TakeAction), "Take action", "workflow-kind-action"),
            (Some(TemplateKind::GetUpdates), "Get updates", "workflow-kind-updates"),
            (Some(TemplateKind::Schedule), "Schedule", "workflow-kind-schedule"),
        ];
        div()
            .flex()
            .items_center()
            .justify_between()
            .child(div().flex().gap_2().children(kinds.map(|(kind, label, id)| {
                Self::pill(id, label, app.kind == kind, palette, cx).on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(app) = &mut this.app {
                        app.kind = kind;
                    }
                    cx.notify();
                }))
            })))
            .child(Self::filter(app, palette, cx))
            .into_any_element()
    }

    /// A pill of Teams' filters: brand-tinted and outlined while chosen.
    pub(super) fn pill(
        id: &'static str,
        label: &'static str,
        chosen: bool,
        palette: Palette,
        _cx: &Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .debug_selector(move || id.to_owned())
            .min_w(px(72.0))
            .flex()
            .justify_center()
            .px_3()
            .py_1()
            .rounded_full()
            .border_1()
            .cursor_pointer()
            .map(|pill| {
                if chosen {
                    pill.border_color(rgb(palette.brand))
                        .bg(rgb(palette.selected))
                        .text_color(rgb(palette.brand))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                } else {
                    pill.border_color(rgb(palette.border))
                        .hover(move |pill| pill.bg(rgb(palette.hover)))
                }
            })
            .child(label)
    }

    /// Filter: the apps the gallery's templates use, each with its icon and a check; "Filter (n)" while some are
    /// chosen.
    fn filter(app: &AppState, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let label = if app.apps.is_empty() {
            "Filter".to_owned()
        } else {
            format!("Filter ({})", app.apps.len())
        };
        let used: Vec<&'static str> = match &app.all {
            Gallery::Ready(cards) => model::CONNECTORS
                .iter()
                .map(|(api, _)| *api)
                .filter(|api| cards.iter().any(|card| card.connectors().contains(api)))
                .collect(),
            _ => Vec::new(),
        };
        div()
            .relative()
            .child(
                div()
                    .id("workflow-filter")
                    .debug_selector(|| "workflow-filter".to_owned())
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .when(!app.apps.is_empty() || app.filter_open, |face| {
                        face.text_color(rgb(palette.brand))
                    })
                    .hover(move |face| face.bg(rgb(palette.hover)))
                    .child(label)
                    .child(Glyph::LIST.icon().size(px(14.0)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(app) = &mut this.app {
                            app.filter_open = !app.filter_open;
                        }
                        cx.notify();
                    })),
            )
            .when(app.filter_open, |filter| {
                filter.child(
                    gpui::deferred(
                        div()
                            .absolute()
                            .top(px(32.0))
                            .right_0()
                            .w(px(250.0))
                            .child(Self::filter_list(&used, &app.apps, palette, cx)),
                    )
                    .with_priority(1),
                )
            })
            .into_any_element()
    }

    fn filter_list(
        used: &[&'static str],
        chosen: &BTreeSet<&'static str>,
        palette: Palette,
        cx: &Context<Self>,
    ) -> AnyElement {
        div()
            .id("workflow-filter-list")
            .debug_selector(|| "workflow-filter-list".to_owned())
            .max_h(px(232.0))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .p_1()
            .rounded_md()
            .bg(rgb(palette.surface))
            .border_1()
            .border_color(rgb(palette.border))
            .shadow_lg()
            .occlude()
            .children(used.iter().map(|api| {
                let api = *api;
                let name = model::CONNECTORS
                    .iter()
                    .find(|(known, _)| *known == api)
                    .map_or(api, |(_, name)| *name);
                let id = SharedString::from(format!("workflow-filter-{api}"));
                let weak = cx.entity().downgrade();
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1p5()
                    .rounded_md()
                    .hover(|row| row.bg(rgb(palette.hover)))
                    .child(selected_by(
                        id.clone(),
                        Checkbox::new(id)
                            .checked(chosen.contains(api))
                            .on_click(move |on, _, cx| {
                                let on = *on;
                                let _ = weak.update(cx, |this, cx| this.filter_app(api, on, cx));
                            }),
                    ))
                    .child(gpui::img(art::connector(api)).size(px(16.0)))
                    .child(name)
            }))
            .into_any_element()
    }

    /// "Your workflows": every workflow of the account, found by name and filtered by state.
    pub(super) fn app_all_workflows(&self, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let Some(app) = &self.app else {
            return div().into_any_element();
        };
        let cards: Vec<_> = self
            .workflows
            .iter()
            .filter(|card| !card.is_ai())
            .filter(|card| match app.flows {
                FlowFilter::All => true,
                FlowFilter::Active => card.enabled(),
                FlowFilter::Inactive => !card.enabled(),
            })
            .collect();
        let filters = [
            (FlowFilter::All, "All", "workflow-flows-all"),
            (FlowFilter::Active, "Active", "workflow-flows-active"),
            (FlowFilter::Inactive, "Inactive", "workflow-flows-inactive"),
        ];
        div()
            .flex()
            .flex_col()
            .gap_6()
            .child(self.page_back("workflow-all-back", "Your workflows", cx))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(self.find_flows(app, palette, cx))
                            .children(filters.map(|(filter, label, id)| {
                                Self::pill(id, label, app.flows == filter, palette, cx).on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        if let Some(app) = &mut this.app {
                                            app.flows = filter;
                                        }
                                        cx.notify();
                                    },
                                ))
                            })),
                    )
                    .child(Self::refresh_button(self.busy, cx)),
            )
            .child(self.flow_rows(&cards, palette, cx))
            // every workflow of the account: its later pages too
            .children(self.load_more(cx))
            .into_any_element()
    }

    /// "Find workflows", suggesting the account's workflows whose names hold the text.
    fn find_flows(&self, app: &AppState, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let text = app.find_flows.read(cx).value().trim().to_lowercase();
        let matches: Vec<_> = self
            .workflows
            .iter()
            .filter(|card| !text.is_empty() && card.name.to_lowercase().contains(&text))
            .take(8)
            .cloned()
            .collect();
        let list = (app.flows_suggesting && !text.is_empty()).then(|| {
            let mut list = div()
                .debug_selector(|| "workflow-flow-suggestions".to_owned())
                .flex()
                .flex_col()
                .py_2()
                .rounded_md()
                .bg(rgb(palette.surface))
                .border_1()
                .border_color(rgb(palette.border))
                .shadow_lg()
                .occlude()
                .on_mouse_down(gpui::MouseButton::Left, |_, window, cx| {
                    window.prevent_default();
                    cx.stop_propagation();
                });
            if matches.is_empty() {
                list = list.child(
                    div()
                        .px_5()
                        .py_2()
                        .text_sm()
                        .font_weight(gpui::FontWeight::BOLD)
                        .child("We didn't find any matches"),
                );
            } else {
                list = list
                    .child(
                        div()
                            .px_5()
                            .py_2()
                            .text_sm()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child("Your workflows"),
                    )
                    .children(matches.into_iter().map(|card| {
                        let selector = format!("workflow-flow-suggestion-{}", card.id);
                        div()
                            .id(SharedString::from(selector.clone()))
                            .debug_selector(move || selector.clone())
                            .px_6()
                            .py_1p5()
                            .cursor_pointer()
                            .hover(|row| row.bg(rgb(palette.hover)))
                            .child(card.name.clone())
                            .on_click(cx.listener(move |this, _, _, cx| this.open_found_flow(card.clone(), cx)))
                    }));
            }
            list
        });
        div()
            .relative()
            .w(px(320.0))
            .child(selected_by(
                "workflow-find-flows",
                Input::new(&app.find_flows).prefix(Glyph::SEARCH.icon()).cleanable(true),
            ))
            .when_some(list, |find, list| {
                find.child(
                    gpui::deferred(div().absolute().top(px(36.0)).left_0().right_0().child(list)).with_priority(1),
                )
            })
            .into_any_element()
    }
}
