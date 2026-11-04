//! Teams' channel Workflows modal, backed by the active account's first-party Power Automate service.
//! The catalog, environments and workflows have separate loading/errors: an inaccessible service is never an empty list.
mod connections;
mod editor;
mod form;
mod model;
mod run_inputs;
mod runs;
mod service;
mod view;

use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, AsyncApp, Context, Entity, EventEmitter, FocusHandle, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _, Subscription,
    Task, WeakEntity, Window, div, px, rgb,
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
    History(Card),
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
        let subscriptions = vec![cx.subscribe(&search, |_, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        })];
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
        };
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
