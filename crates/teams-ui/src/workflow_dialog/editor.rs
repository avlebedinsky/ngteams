//! Native definition configuration and lifecycle actions. Service properties survive round trips. An edit never
//! treats a catalog card as an existing workflow, and an operation remains bound to the environment opened with it.
use super::*;

impl WorkflowDialog {
    pub(super) fn configure_template(&mut self, card: Card, window: &mut Window, cx: &mut Context<Self>) {
        self.load_template(card.id, None, window, cx);
    }

    pub(super) fn build(&mut self, kind: BuildKind, window: &mut Window, cx: &mut Context<Self>) {
        let id = match kind {
            BuildKind::Manual | BuildKind::Scheduled => "805739fdad754e799502b6379900922e",
            BuildKind::Webhook => "d271a6f01c2545a28348d8f2cddf4c8f",
        };
        self.load_template(id.to_owned(), Some(kind), window, cx);
    }

    fn load_template(&mut self, id: String, kind: Option<BuildKind>, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        let generation = self.generation;
        let window = window.window_handle();
        let reply = self.core.workflow_request(WorkflowRequest::Template { id });
        self.tasks
            .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let answer = reply.await.unwrap_or(Err(WorkflowFailure::Connection));
                let _ = window.update(cx, |_, window, cx| {
                    let _ = this.update(cx, |this, cx| this.template_read(generation, answer, kind, window, cx));
                });
            }));
        cx.notify();
    }

    fn template_read(
        &mut self,
        generation: u64,
        answer: WorkflowResponse,
        kind: Option<BuildKind>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.generation != generation {
            return;
        }
        self.busy = false;
        match answer {
            Ok(value) => match Card::from_flow(&value) {
                Some(mut card) => {
                    if matches!(kind, Some(BuildKind::Manual)) {
                        card.properties["definition"]["triggers"] = serde_json::json!({"manual": {"type": "Request", "kind": "Button", "inputs": {"schema": {"type": "object", "properties": {}}}}});
                    }
                    if kind.is_some() {
                        "New channel workflow".clone_into(&mut card.name);
                    }
                    self.edit(card, false, window, cx);
                }
                None => self.error = Some(model::problem(&WorkflowFailure::InvalidResponse).to_owned()),
            },
            Err(error) => self.error = Some(model::problem(&error).to_owned()),
        }
        cx.notify();
    }

    pub(super) fn edit(&mut self, card: Card, existing: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.core.read_only() || self.busy {
            return;
        }
        self.error = None;
        let mut properties = serde_json::json!({"displayName": card.name, "definition": card.properties.get("definition"), "connectionReferences": card.properties.get("connectionReferences"), "state": card.properties.get("state").cloned().unwrap_or(Value::String("Stopped".to_owned()))});
        if !existing {
            form::apply_channel(
                &mut properties,
                self.team.group_id.as_deref().unwrap_or(&self.team.id),
                &self.channel.id,
            );
        }
        self.prepare_bindings(&properties);
        self.prepare_fields(&mut properties, window, cx);
        let name = card.name.clone();
        self.name.update(cx, |state, cx| state.set_value(name, window, cx));
        self.definition.update(cx, |state, cx| {
            state.set_value(
                serde_json::to_string_pretty(&properties).unwrap_or_default(),
                window,
                cx,
            );
        });
        self.screen = Screen::Edit {
            flow: existing.then_some(card),
        };
        cx.notify();
    }

    pub(super) fn open_flow(&mut self, card: Card, cx: &mut Context<Self>) {
        let Some(environment) = self.environment.clone() else {
            return;
        };
        let generation = self.generation;
        self.busy = true;
        self.error = None;
        let reply = self.core.workflow_request(WorkflowRequest::Get {
            environment,
            flow: card.id,
        });
        self.tasks
            .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let answer = reply.await.unwrap_or(Err(WorkflowFailure::Connection));
                let _ = this.update(cx, |this, cx| {
                    if this.generation != generation {
                        return;
                    }
                    this.busy = false;
                    match answer {
                        Ok(value) => match Card::from_flow(&value) {
                            Some(card) => this.screen = Screen::Detail(card),
                            None => this.error = Some(model::problem(&WorkflowFailure::InvalidResponse).to_owned()),
                        },
                        Err(error) => this.error = Some(model::problem(&error).to_owned()),
                    }
                    cx.notify();
                });
            }));
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if self.core.read_only() || self.busy {
            return;
        }
        let Some(environment) = self.environment.clone() else {
            self.error = Some("No Power Automate environment is available for this account.".to_owned());
            cx.notify();
            return;
        };
        let name = self.name.read(cx).value().trim().to_owned();
        if name.is_empty() {
            self.error = Some("Enter a workflow name.".to_owned());
            cx.notify();
            return;
        }
        let mut properties = match self.configured(cx) {
            Ok(properties) => properties,
            Err(error) => {
                self.error = Some(error.to_owned());
                cx.notify();
                return;
            }
        };
        if !properties.is_object()
            || !properties.get("definition").is_some_and(Value::is_object)
            || !properties.pointer("/definition/triggers").is_some_and(Value::is_object)
            || !properties.pointer("/definition/actions").is_some_and(Value::is_object)
        {
            self.error = Some("The workflow needs a definition containing triggers and actions.".to_owned());
            cx.notify();
            return;
        }
        if let Err(error) = self.bind_connections(&mut properties) {
            self.error = Some(error.to_owned());
            cx.notify();
            return;
        }
        properties["displayName"] = Value::String(name);
        let request = match &self.screen {
            Screen::Edit { flow: Some(card) } => WorkflowRequest::Update {
                environment,
                flow: card.id.clone(),
                expected_modified: card
                    .properties
                    .get("lastModifiedTime")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                properties,
            },
            Screen::Edit { flow: None } => {
                properties["state"] = Value::String("Stopped".to_owned());
                WorkflowRequest::Create {
                    environment,
                    properties,
                }
            }
            _ => return,
        };
        self.mutate(request, "Workflow saved", cx);
    }

    fn mutate(&mut self, request: WorkflowRequest, notice: &'static str, cx: &mut Context<Self>) {
        if self.busy || self.core.read_only() || self.uncertain.is_some() {
            return;
        }
        self.busy = true;
        self.error = None;
        let generation = self.generation;
        let reply = self.core.workflow_request(request);
        self.tasks
            .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let answer = reply.await.unwrap_or(Err(WorkflowFailure::Connection));
                let _ = this.update(cx, |this, cx| {
                    if this.generation != generation {
                        return;
                    }
                    this.busy = false;
                    this.mutation_read(answer, notice, cx);
                    cx.notify();
                });
            }));
        cx.notify();
    }

    fn mutation_read(&mut self, answer: WorkflowResponse, notice: &str, cx: &mut Context<Self>) {
        match answer {
            Ok(_) => {
                self.notice = Some(notice.to_owned());
                self.screen = Screen::Browse;
                self.load_workflows(cx);
            }
            Err(error) => {
                if matches!(error, WorkflowFailure::Connection | WorkflowFailure::Service(500..=599)) {
                    self.uncertain = Some(error.clone());
                }
                self.error = Some(model::problem(&error).to_owned());
            }
        }
    }

    fn history(&mut self, card: Card, cx: &mut Context<Self>) {
        self.history_read(card, None, cx);
    }

    fn history_read(&mut self, card: Card, next: Option<String>, cx: &mut Context<Self>) {
        let Some(environment) = self.environment.clone() else {
            return;
        };
        self.busy = true;
        self.error = None;
        let generation = self.generation;
        let append = next.is_some();
        let request = next.map_or_else(
            || WorkflowRequest::History {
                environment: environment.clone(),
                flow: card.id.clone(),
            },
            |next| WorkflowRequest::HistoryPage {
                environment: environment.clone(),
                flow: card.id.clone(),
                next,
            },
        );
        let reply = self.core.workflow_request(request);
        self.tasks
            .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let answer = reply.await.unwrap_or(Err(WorkflowFailure::Connection));
                let _ = this.update(cx, |this, cx| {
                    if generation != this.generation {
                        return;
                    }
                    this.busy = false;
                    this.history_received(answer, card, append);
                    cx.notify();
                });
            }));
        cx.notify();
    }

    fn history_received(&mut self, answer: WorkflowResponse, card: Card, append: bool) {
        match answer.and_then(|value| model::rows(&value).map(|rows| (rows.to_vec(), model::next(&value)))) {
            Ok((history, next)) => {
                if !append {
                    self.history.clear();
                }
                self.history.extend(history);
                self.history_next = next;
                self.screen = Screen::History(card);
            }
            Err(error) => self.error = Some(model::problem(&error).to_owned()),
        }
    }

    pub(super) fn editor(&self, palette: Palette, cx: &Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(div().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).child(
                if matches!(self.screen, Screen::Edit { flow: Some(_) }) {
                    "Edit workflow"
                } else {
                    "Configure workflow"
                },
            ))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(palette.muted_foreground))
                    .child(format!("Destination: {} / {}", self.team.name, self.channel.name)),
            )
            .child("Workflow name")
            .child(selected_by("workflow-name", Input::new(&self.name)))
            .child(self.connections_view(palette, cx))
            .child(self.config_fields(palette))
            .child(selected_by(
                "workflow-definition-toggle",
                Button::new("workflow-definition-toggle")
                    .ghost()
                    .label(if matches!(self.editor_mode, EditorMode::Form) {
                        "Show definition"
                    } else {
                        "Show configuration"
                    })
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_definition(window, cx))),
            ))
            .when(matches!(self.editor_mode, EditorMode::Definition), |view| {
                view.child(
                    div()
                        .min_h(px(320.0))
                        .child(selected_by("workflow-definition", Textarea::new(&self.definition))),
                )
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_3()
                    .child(selected_by(
                        "workflow-edit-cancel",
                        Button::new("workflow-edit-cancel")
                            .label("Cancel")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.screen = Screen::Browse;
                                this.error = None;
                                cx.notify();
                            })),
                    ))
                    .child(selected_by(
                        "workflow-save",
                        Button::new("workflow-save")
                            .primary()
                            .label("Save workflow")
                            .disabled(self.busy || self.uncertain.is_some())
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    )),
            )
            .into_any_element()
    }

    fn toggle_definition(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.editor_mode, EditorMode::Form) {
            match self.configured(cx) {
                Ok(properties) => {
                    self.definition.update(cx, |state, cx| {
                        state.set_value(
                            serde_json::to_string_pretty(&properties).unwrap_or_default(),
                            window,
                            cx,
                        );
                    });
                    self.editor_mode = EditorMode::Definition;
                }
                Err(error) => self.error = Some(error.to_owned()),
            }
        } else if let Ok(mut properties) = self.configured(cx) {
            self.prepare_bindings(&properties);
            self.prepare_fields(&mut properties, window, cx);
        } else {
            self.error = Some("Fix the definition before returning to configuration.".to_owned());
        }
        cx.notify();
    }

    pub(super) fn detail(&self, card: &Card, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let history = card.clone();
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(card.name.clone()),
            )
            .child(
                div()
                    .text_color(rgb(palette.muted_foreground))
                    .child(card.description.clone()),
            )
            .child(format!(
                "Status: {}",
                card.properties
                    .get("state")
                    .and_then(Value::as_str)
                    .unwrap_or("Unknown")
            ))
            .child(selected_by(
                "workflow-history",
                Button::new("workflow-history")
                    .label("Run history")
                    .icon(Glyph::CLOCK.icon())
                    .disabled(self.busy)
                    .on_click(cx.listener(move |this, _, _, cx| this.history(history.clone(), cx))),
            ))
            .children(self.webhook_copy(card, cx))
            .when(!self.core.read_only(), |view| {
                view.child(self.detail_controls(card, cx))
            })
            .into_any_element()
    }

    fn detail_controls(&self, card: &Card, cx: &Context<Self>) -> AnyElement {
        let (editing, changing, running, deleting) = (card.clone(), card.clone(), card.clone(), card.clone());
        div()
            .flex()
            .gap_3()
            .child(selected_by(
                "workflow-edit",
                Button::new("workflow-edit")
                    .label("Edit")
                    .disabled(self.busy || self.uncertain.is_some())
                    .on_click(cx.listener(move |this, _, window, cx| this.edit(editing.clone(), true, window, cx))),
            ))
            .child(selected_by(
                "workflow-toggle",
                Button::new("workflow-toggle")
                    .label(if card.enabled() { "Turn off" } else { "Turn on" })
                    .disabled(self.busy || self.uncertain.is_some())
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle(&changing, cx))),
            ))
            .child(selected_by(
                "workflow-run",
                Button::new("workflow-run")
                    .label("Run")
                    .icon(Glyph::PLAY.icon())
                    .disabled(self.busy || self.uncertain.is_some())
                    .on_click(cx.listener(move |this, _, _, cx| this.run(&running, cx))),
            ))
            .child(selected_by(
                "workflow-delete",
                Button::new("workflow-delete")
                    .danger()
                    .label("Delete")
                    .disabled(self.busy || self.uncertain.is_some())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.screen = Screen::Delete(deleting.clone());
                        cx.notify();
                    })),
            ))
            .into_any_element()
    }

    fn toggle(&mut self, card: &Card, cx: &mut Context<Self>) {
        let Some(environment) = self.environment.clone() else {
            return;
        };
        self.mutate(
            WorkflowRequest::SetEnabled {
                environment,
                flow: card.id.clone(),
                enabled: !card.enabled(),
            },
            "Workflow updated",
            cx,
        );
    }

    fn run(&mut self, card: &Card, cx: &mut Context<Self>) {
        let Some(environment) = self.environment.clone() else {
            return;
        };
        let triggers = card
            .properties
            .pointer("/definition/triggers")
            .and_then(Value::as_object);
        let trigger = triggers
            .filter(|triggers| {
                triggers.len() == 1
                    && !triggers.values().any(|trigger| {
                        trigger
                            .pointer("/inputs/schema/required")
                            .and_then(Value::as_array)
                            .is_some_and(|required| !required.is_empty())
                    })
            })
            .and_then(|triggers| triggers.keys().next().cloned());
        if let Some(trigger) = trigger {
            self.mutate(
                WorkflowRequest::Run {
                    environment,
                    flow: card.id.clone(),
                    trigger,
                },
                "Workflow run requested",
                cx,
            );
        } else {
            self.error = Some("This workflow needs trigger inputs or a different trigger. Required-input runs are not supported by the available management operation.".to_owned());
            cx.notify();
        }
    }

    pub(super) fn history_view(&self, card: &Card, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let mut view = div().flex().flex_col().gap_3().child(
            div()
                .text_lg()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(format!("{} · Run history", card.name)),
        );
        if self.history.is_empty() {
            view = view.child(state(
                "No runs yet",
                "Runs appear here after this workflow has been triggered.",
                palette,
            ));
        }
        for run in &self.history {
            let status = run
                .pointer("/properties/status")
                .and_then(Value::as_str)
                .unwrap_or("Unknown");
            let started = run
                .pointer("/properties/startTime")
                .and_then(Value::as_str)
                .unwrap_or("Time unavailable");
            let ended = run
                .pointer("/properties/endTime")
                .and_then(Value::as_str)
                .unwrap_or("Running");
            let (card, run) = (card.clone(), run.clone());
            view = view.child(selected_by(
                format!(
                    "workflow-run-{}",
                    run.get("name").and_then(Value::as_str).unwrap_or_default()
                ),
                Button::new(format!(
                    "workflow-history-{}",
                    run.get("name").and_then(Value::as_str).unwrap_or_default()
                ))
                .outline()
                .label(format!("{status} · {started} · {ended}"))
                .on_click(cx.listener(move |this, _, _, cx| this.run_details(card.clone(), run.clone(), cx))),
            ));
        }
        if let Some(next) = &self.history_next {
            let (card, next) = (card.clone(), next.clone());
            view = view.child(selected_by(
                "workflow-more-history",
                Button::new("workflow-more-history")
                    .label("Load more runs")
                    .disabled(self.busy)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.history_read(card.clone(), Some(next.clone()), cx)),
                    ),
            ));
        }
        view.into_any_element()
    }

    pub(super) fn delete_view(&self, card: &Card, cx: &Context<Self>) -> AnyElement {
        let chosen = card.clone();
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(div().text_lg().child("Delete workflow?"))
            .child(format!("{} will be deleted. This cannot be undone.", card.name))
            .child(selected_by(
                "workflow-delete-confirm",
                Button::new("workflow-delete-confirm")
                    .danger()
                    .label("Delete workflow")
                    .disabled(self.busy)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(environment) = this.environment.clone() {
                            this.mutate(
                                WorkflowRequest::Delete {
                                    environment,
                                    flow: chosen.id.clone(),
                                },
                                "Workflow deleted",
                                cx,
                            );
                        }
                    })),
            ))
            .into_any_element()
    }
}
