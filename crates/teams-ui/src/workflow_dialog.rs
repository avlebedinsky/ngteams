//! Teams' channel Workflows modal, backed by the active account's first-party Power Automate service.
//! The catalog, environments and workflows have separate loading/errors: an inaccessible service is never an empty list.
mod art;
mod combo;
mod connections;
mod editor;
mod fields;
mod form;
mod markup;
mod model;
mod overview;
mod owners;
mod rows;
mod run_inputs;
mod runs;
mod service;
mod simple;
mod view;
mod zones;

use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, AsyncApp, Context, Entity, EventEmitter, FocusHandle, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, WeakEntity, Window, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::{Disableable as _, FocusTrapElement as _, Selectable as _};
use serde_json::Value;
use teams_proto::{WorkflowFailure, WorkflowRequest, WorkflowResponse};
use teams_store::{Team, TeamChannel};

use crate::bridge::Core;
use crate::icons::{Glyph, icon_button, selected_by};
use crate::team_dialogs::TeamDialogEvent;
use crate::theme::{self, Palette};
use connections::Binding;
use form::{ConfigField, EditorMode};
use model::Card;
use run_inputs::RunField;

/// Where a change's success leads: back to the list with a notice, to the changed workflow's Overview, staying on the
/// Overview with the workflow read again (Rename, Turn off/on, as Teams does), or its owners read again.
#[derive(Clone, Copy)]
enum Outcome {
    Notice(&'static str),
    Overview,
    Stay,
    Owners,
    /// Back on the workflow list, read again (an edit or a change made from the list's menu).
    List,
}

/// The Overview change under way, whose button shows it working.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Change {
    Toggle,
    Rename,
    Owners,
}

/// Which list a row's menu was used in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Listing {
    Channel,
    Home,
}

#[derive(Clone, Copy)]
enum BuildKind {
    Manual,
    Scheduled,
    Webhook,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Channel,
    Workflows,
    Ai,
}

#[derive(Clone)]
enum Screen {
    Browse,
    Home,
    Build,
    Edit {
        flow: Option<Card>,
    },
    Detail(Card),
    RunDetails {
        card: Card,
        run: Value,
        actions: Vec<Value>,
    },
    RunInputs {
        card: Card,
        trigger: String,
        schema: Value,
    },
    Delete(Card),
    /// The delete question asked from the workflow list's menu, over the list.
    DeleteListed(Card),
}

#[derive(Default)]
struct Loading {
    catalog: bool,
    workflows: bool,
}

/// The channel's native workflow manager. The Shell hosts it with its standard modal and focus restoration.
pub struct WorkflowDialog {
    core: Arc<dyn Core>,
    team: Team,
    channel: TeamChannel,
    focus: FocusHandle,
    search: Entity<InputState>,
    name: Entity<InputState>,
    definition: Entity<TextareaState>,
    draft: Value,
    fields: Vec<ConfigField>,
    editor_mode: EditorMode,
    bindings: Vec<Binding>,
    connections: Vec<Value>,
    connection_error: Option<WorkflowFailure>,
    copilot_eligibility: Option<Result<bool, WorkflowFailure>>,
    _subscriptions: Vec<Subscription>,
    tasks: Vec<Task<()>>,
    generation: u64,
    catalog: Vec<Card>,
    environments: Vec<(String, String)>,
    environment: Option<String>,
    workflows: Vec<Card>,
    run_fields: Vec<RunField>,
    history: Vec<Value>,
    history_next: Option<String>,
    loading: Loading,
    busy: bool,
    uncertain: Option<WorkflowFailure>,
    reconciling: bool,
    catalog_error: Option<WorkflowFailure>,
    workflows_error: Option<WorkflowFailure>,
    error: Option<String>,
    notice: Option<String>,
    continuation: Option<String>,
    tab: Tab,
    screen: Screen,
    all_templates: bool,
    offset: usize,
    teams: Vec<Team>,
    simple: Option<simple::Simple>,
    webhook_link: Option<String>,
    rename: Option<(Entity<InputState>, Subscription)>,
    changing: Option<Change>,
    owners: Option<Result<Vec<owners::Owner>, WorkflowFailure>>,
    owner_search: Entity<InputState>,
    owner_query: u64,
    owner_found: Option<owners::Found>,
    /// The listed workflow a change from its row is under way for (its row shows it working).
    acting: Option<String>,
    /// The list (the channel's or Workflows Home's) a row was acted on in: the change returns there.
    listing: Listing,
    /// Each listed schedule's next run, as shown.
    next_runs: std::collections::HashMap<String, String>,
    scroll: gpui::ScrollHandle,
    /// The kind of screen the page was last shown for: a new kind starts at its top.
    scrolled_for: Option<u8>,
}

impl EventEmitter<TeamDialogEvent> for WorkflowDialog {}

impl WorkflowDialog {
    /// Opens with the channel's exact service ids, never a name used as a destination.
    pub fn new(
        core: Arc<dyn Core>,
        team: Team,
        channel: TeamChannel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Find templates"));
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Workflow name"));
        let definition = cx.new(|cx| TextareaState::new(window, cx).placeholder("Workflow definition"));
        let owner_search = cx.new(|cx| InputState::new(window, cx).placeholder("Enter names, email or user groups"));
        let subscriptions = vec![
            cx.subscribe(&search, |_, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.subscribe(&owner_search, |this, _, event, cx| match event {
                InputEvent::Change => this.search_owners(cx),
                InputEvent::Blur => {
                    this.owner_found = None;
                    cx.notify();
                }
                _ => {}
            }),
        ];
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let mut this = Self {
            core,
            team,
            channel,
            focus,
            search,
            name,
            definition,
            draft: Value::Null,
            fields: Vec::new(),
            editor_mode: EditorMode::Form,
            bindings: Vec::new(),
            connections: Vec::new(),
            connection_error: None,
            copilot_eligibility: None,
            _subscriptions: subscriptions,
            tasks: Vec::new(),
            generation: 0,
            catalog: Vec::new(),
            environments: Vec::new(),
            environment: None,
            workflows: Vec::new(),
            run_fields: Vec::new(),
            history: Vec::new(),
            history_next: None,
            loading: Loading::default(),
            busy: false,
            uncertain: None,
            reconciling: false,
            catalog_error: None,
            workflows_error: None,
            error: None,
            notice: None,
            continuation: None,
            tab: Tab::Channel,
            screen: Screen::Browse,
            all_templates: false,
            offset: 0,
            teams: Vec::new(),
            simple: None,
            webhook_link: None,
            rename: None,
            changing: None,
            owners: None,
            owner_search,
            owner_query: 0,
            owner_found: None,
            acting: None,
            listing: Listing::Channel,
            next_runs: std::collections::HashMap::new(),
            scroll: gpui::ScrollHandle::new(),
            scrolled_for: None,
        };
        this.load_teams(window, cx);
        this.refresh(cx);
        this
    }

    /// Opens a template that the current catalog actually offered, for keyboard commands and deep links.
    pub fn select_template(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(card) = self.catalog.iter().find(|card| card.id == id).cloned() {
            self.configure_template(card, window, cx);
        }
    }

    /// Configures a run of a workflow already loaded for this account and environment.
    pub fn prepare_workflow_run(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(card) = self.workflows.iter().find(|card| card.id == id).cloned() {
            self.prepare_run(&card, window, cx);
        }
    }

    /// The account's teams and channels, from the store, for the card editor's choices.
    fn load_teams(&mut self, window: &Window, cx: &mut Context<Self>) {
        let reply = self.core.teams();
        let window = window.window_handle();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            if let Ok(Some(teams)) = reply.await {
                let _ = window.update(cx, |_, window, cx| {
                    this.update(cx, |this, cx| this.teams_read(teams, window, cx))
                });
            }
        })
        .detach();
    }

    fn teams_read(&mut self, teams: Vec<Team>, window: &mut Window, cx: &mut Context<Self>) {
        self.teams = teams;
        // A card editor opened before the teams were read shows its choices' names now.
        self.show_choices(window, cx);
        cx.notify();
    }

    /// Opens the Overview of a workflow already loaded for this account and environment.
    pub fn open_workflow(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(card) = self.workflows.iter().find(|card| card.id == id).cloned() {
            self.open_flow(card, cx);
        }
    }

    /// Folds the card editor's card to its title, as its "Collapse all" does.
    pub fn collapse_all(&mut self, cx: &mut Context<Self>) {
        if let Some(simple) = &mut self.simple {
            simple.collapse();
            cx.notify();
        }
    }

    /// Asks to delete the workflow whose Overview is open, as its "..." menu's Delete does.
    pub fn ask_delete(&mut self, cx: &mut Context<Self>) {
        if let Screen::Detail(card) = &self.screen
            && !self.core.read_only()
        {
            self.screen = Screen::Delete(card.clone());
            cx.notify();
        }
    }

    /// Starts renaming the workflow whose Overview is open, as its Rename does.
    pub fn rename_open_workflow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Screen::Detail(card) = &self.screen {
            let card = card.clone();
            self.start_rename(&card, window, cx);
        }
    }

    /// Opens the card editor's team list, as a click in its field does.
    pub fn open_team_choices(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.team_field() {
            let input = self.combo_mut(index).map(|combo| combo.input.clone());
            if let Some(input) = input {
                input.update(cx, |state, cx| state.focus(window, cx));
            }
        }
    }

    /// Chooses a team in the card editor by its group id, as picking it in the team list does.
    pub fn choose_team(&mut self, group: &str, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.team_by_group(group).map(|team| team.name.clone());
        if let (Some(index), Some(name)) = (self.team_field(), name) {
            self.choose(index, group.to_owned(), name, window, cx);
        }
    }

    /// The card editor's team field.
    fn team_field(&self) -> Option<usize> {
        self.simple
            .as_ref()?
            .params
            .iter()
            .position(|param| param.kind == markup::Kind::TeamsGroup)
    }

    /// Searches the directory for a co-owner, as typing `text` in "Add a co-owner" does.
    pub fn search_co_owner(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.owner_search.update(cx, |state, cx| {
            state.set_value(text.to_owned(), window, cx);
            state.focus(window, cx);
        });
        self.search_owners(cx);
    }

    /// Scrolls the dialog's page to its end.
    pub fn scroll_to_end(&mut self, cx: &mut Context<Self>) {
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    /// Opens Workflows Home, as the footer's button does.
    pub fn show_home(&mut self, cx: &mut Context<Self>) {
        self.screen = Screen::Home;
        self.tab = Tab::Workflows;
        cx.notify();
    }

    /// Refreshes both service sources. A generation rejects late results from an older refresh.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.reconciling = self.uncertain.is_some();
        self.generation += 1;
        let generation = self.generation;
        self.loading.catalog = true;
        self.loading.workflows = true;
        self.catalog_error = None;
        self.workflows_error = None;
        self.error = None;
        self.tasks.clear();
        self.load_copilot_access(cx);
        let catalog = self.core.workflow_request(WorkflowRequest::Catalog);
        self.tasks
            .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let answer = catalog.await.unwrap_or(Err(WorkflowFailure::Connection));
                let _ = this.update(cx, |this, cx| {
                    if this.generation != generation {
                        return;
                    }
                    this.loading.catalog = false;
                    match answer.and_then(|value| model::templates(&value)) {
                        Ok(cards) => this.catalog = cards,
                        Err(error) => this.catalog_error = Some(error),
                    }
                    cx.notify();
                });
            }));
        let environments = self.core.workflow_request(WorkflowRequest::Environments);
        self.tasks
            .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let answer = environments.await.unwrap_or(Err(WorkflowFailure::Connection));
                let _ = this.update(cx, |this, cx| this.environments_read(generation, answer, cx));
            }));
        cx.notify();
    }

    fn environments_read(&mut self, generation: u64, answer: WorkflowResponse, cx: &mut Context<Self>) {
        if generation != self.generation {
            return;
        }
        let parsed = answer.and_then(|value| {
            model::rows(&value).map(|rows| {
                rows.iter()
                    .filter_map(|row| {
                        Some((
                            row.get("name")?.as_str()?.to_owned(),
                            row.pointer("/properties/displayName")?.as_str()?.to_owned(),
                        ))
                    })
                    .collect::<Vec<_>>()
            })
        });
        match parsed {
            Ok(environments) => {
                self.environments = environments;
                self.environment = self
                    .environments
                    .iter()
                    .find(|(id, _)| id.starts_with("Default-"))
                    .or_else(|| self.environments.first())
                    .map(|(id, _)| id.clone());
                if self.environment.is_some() {
                    self.load_workflows(cx);
                    self.load_connections(cx);
                } else {
                    self.loading.workflows = false;
                }
            }
            Err(error) => {
                self.loading.workflows = false;
                self.workflows_error = Some(error);
            }
        }
        cx.notify();
    }

    fn load_copilot_access(&mut self, cx: &mut Context<Self>) {
        self.copilot_eligibility = None;
        let generation = self.generation;
        let reply = self.core.workflow_request(WorkflowRequest::CopilotEligibility);
        self.tasks
            .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let answer = reply
                    .await
                    .unwrap_or(Err(WorkflowFailure::Connection))
                    .and_then(|value| model::copilot_access(&value));
                let _ = this.update(cx, |this, cx| {
                    if this.generation == generation {
                        this.copilot_eligibility = Some(answer);
                        cx.notify();
                    }
                });
            }));
    }

    fn load_workflows(&mut self, cx: &mut Context<Self>) {
        self.read_workflows(None, cx);
    }

    fn read_workflows(&mut self, next: Option<String>, cx: &mut Context<Self>) {
        let Some(environment) = self.environment.clone() else {
            return;
        };
        let generation = self.generation;
        let append = next.is_some();
        self.loading.workflows = true;
        self.workflows_error = None;
        let core = Arc::clone(&self.core);
        self.tasks
            .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let answer = service::workflows(&core, &environment, next).await;
                let _ = this.update(cx, |this, cx| {
                    if generation != this.generation {
                        return;
                    }
                    this.loading.workflows = false;
                    this.workflows_read(answer, append);
                    this.load_next_runs(cx);
                    cx.notify();
                });
            }));
    }
    fn workflows_read(&mut self, answer: Result<service::WorkflowPage, WorkflowFailure>, append: bool) {
        match answer {
            Ok(page) => {
                if self.reconciling {
                    self.reconciling = false;
                    self.uncertain = None;
                    self.notice = Some(
                        "Latest workflows loaded. Review status and history before retrying an action.".to_owned(),
                    );
                }
                if !append {
                    self.workflows.clear();
                }
                for card in page.cards {
                    if !self.workflows.iter().any(|existing| existing.id == card.id) {
                        self.workflows.push(card);
                    }
                }
                self.continuation = page.next;
            }
            Err(error) => self.workflows_error = Some(error),
        }
    }
}

fn state(title: &str, description: &str, palette: Palette) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .min_h(px(105.0))
        .p_4()
        .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(title.to_owned()))
        .child(
            div()
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .child(description.to_owned()),
        )
}
