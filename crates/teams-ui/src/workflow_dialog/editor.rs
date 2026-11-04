//! Native definition configuration and lifecycle actions. Service properties survive round trips. An edit never
//! treats a catalog card as an existing workflow, and an operation remains bound to the environment opened with it.
use super::*;

impl WorkflowDialog {
    pub(super) fn configure_template(&mut self, card: Card, window: &mut Window, cx: &mut Context<Self>) {
        self.load_template(card.id, window, cx);
    }

    fn load_template(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
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
                    let _ = this.update(cx, |this, cx| this.template_read(generation, answer, window, cx));
                });
            }));
        cx.notify();
    }

    fn template_read(
        &mut self,
        generation: u64,
        answer: WorkflowResponse,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.generation != generation {
            return;
        }
        self.busy = false;
        match answer {
            Ok(value) => match Card::from_flow(&value) {
                Some(card) if let Some(simple) = simple::Simple::open(&card, false) => {
                    self.open_simple(simple, window, cx);
                }
                Some(card) => self.edit(card, false, window, cx),
                None => self.error = Some(model::problem(&WorkflowFailure::InvalidResponse).to_owned()),
            },
            Err(error) => self.error = Some(model::problem(&error).to_owned()),
        }
        cx.notify();
    }

    pub(super) fn edit(&mut self, card: Card, existing: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_from(card, existing, false, window, cx);
    }

    /// Edits a workflow; from the list's menu, Save and Back return to the list, as in Teams.
    pub(super) fn edit_from(
        &mut self,
        card: Card,
        existing: bool,
        from_list: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.core.read_only() || self.busy {
            return;
        }
        if existing && let Some(mut simple) = simple::Simple::open(&card, true) {
            simple.from_list = from_list;
            self.open_simple(simple, window, cx);
            return;
        }
        self.simple = None;
        self.error = None;
        let mut properties = serde_json::json!({"displayName": card.name, "definition": card.properties.get("definition"), "connectionReferences": card.properties.get("connectionReferences"), "state": card.properties.get("state").cloned().unwrap_or(Value::String("Stopped".to_owned()))});
        if !existing && let Some((team, channel)) = &self.place {
            form::apply_channel(
                &mut properties,
                team.group_id.as_deref().unwrap_or(&team.id),
                &channel.id,
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
                            Some(card) => this.show_overview(card, cx),
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
        if matches!(self.editor_mode, EditorMode::Definition) {
            self.prepare_bindings(&properties);
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

    pub(super) fn mutate(&mut self, request: WorkflowRequest, notice: &'static str, cx: &mut Context<Self>) {
        self.mutate_then(request, Outcome::Notice(notice), cx);
    }

    pub(super) fn mutate_then(&mut self, request: WorkflowRequest, outcome: Outcome, cx: &mut Context<Self>) {
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
                    this.mutation_read(answer, outcome, cx);
                    cx.notify();
                });
            }));
        cx.notify();
    }

    /// A row's change keeps its spinner until the list is read again, as Teams' row does; any other ends it.
    fn settle_row(&mut self, outcome: Outcome, done: bool) {
        if !(matches!(outcome, Outcome::List) && done) {
            self.acting = None;
        }
    }

    fn mutation_read(&mut self, answer: WorkflowResponse, outcome: Outcome, cx: &mut Context<Self>) {
        self.changing = None;
        self.settle_row(outcome, answer.is_ok());
        match answer {
            // Teams shows the saved workflow's Overview after Save.
            Ok(value)
                if matches!(outcome, Outcome::Overview)
                    && let Some(card) = Card::from_flow(&value) =>
            {
                self.simple = None;
                self.show_overview(card, cx);
                self.load_workflows(cx);
            }
            // Rename and Turn off/on keep the Overview, showing the workflow as it now is.
            Ok(value) if matches!(outcome, Outcome::Stay) => {
                self.rename = None;
                let open = match &self.screen {
                    Screen::Detail(card) => Some(card.id.clone()),
                    _ => None,
                };
                match Card::from_flow(&value).filter(|card| Some(&card.id) == open.as_ref()) {
                    Some(card) => self.screen = Screen::Detail(card),
                    None => {
                        if let Some(flow) = open {
                            self.reread_flow(flow, cx);
                        }
                    }
                }
                self.load_workflows(cx);
            }
            Ok(_) if matches!(outcome, Outcome::Owners) => {
                if let Screen::Detail(card) = &self.screen {
                    let card = card.clone();
                    self.load_owners(&card, cx);
                }
            }
            Ok(_) => self.back_to_list(outcome, cx),
            Err(error) => {
                if matches!(error, WorkflowFailure::Connection | WorkflowFailure::Service(500..=599)) {
                    self.uncertain = Some(error.clone());
                }
                self.error = Some(model::problem(&error).to_owned());
            }
        }
    }

    /// After a change that leaves the Overview or editor: the list it came from (a row's list) or the channel's, with
    /// its notice, read again.
    fn back_to_list(&mut self, outcome: Outcome, cx: &mut Context<Self>) {
        self.simple = None;
        if let Outcome::Notice(notice) = outcome {
            self.notice = Some(notice.to_owned());
        }
        self.screen = if matches!(outcome, Outcome::List) {
            self.list_screen()
        } else {
            self.start_screen()
        };
        self.load_workflows(cx);
    }

    /// Opens a workflow's Overview and reads its run history into it.
    fn show_overview(&mut self, card: Card, cx: &mut Context<Self>) {
        self.history.clear();
        self.history_next = None;
        self.webhook_link = None;
        self.rename = None;
        self.owners = None;
        self.owner_found = None;
        self.screen = Screen::Detail(card.clone());
        self.next_runs_of(std::slice::from_ref(&card), cx);
        self.load_owners(&card, cx);
        self.history(card, cx);
    }

    /// Reads the open workflow again, as Teams does after Turn off or on, and shows it as it now is.
    fn reread_flow(&mut self, flow: String, cx: &mut Context<Self>) {
        let Some(environment) = self.environment.clone() else {
            return;
        };
        let generation = self.generation;
        let reply = self.core.workflow_request(WorkflowRequest::Get {
            environment,
            flow: flow.clone(),
        });
        self.tasks
            .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let answer = reply.await.unwrap_or(Err(WorkflowFailure::Connection));
                let _ = this.update(cx, |this, cx| {
                    if this.generation != generation {
                        return;
                    }
                    let open = matches!(&this.screen, Screen::Detail(card) if card.id == flow);
                    match answer.map(|value| Card::from_flow(&value)) {
                        Ok(Some(card)) if open => this.screen = Screen::Detail(card),
                        Ok(_) => {}
                        Err(error) => this.error = Some(model::problem(&error).to_owned()),
                    }
                    cx.notify();
                });
            }));
    }

    pub(super) fn history(&mut self, card: Card, cx: &mut Context<Self>) {
        self.history_read(card, None, cx);
    }

    pub(super) fn history_read(&mut self, card: Card, next: Option<String>, cx: &mut Context<Self>) {
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
                    this.history_received(answer, &card, append);
                    cx.notify();
                });
            }));
        cx.notify();
    }

    fn history_received(&mut self, answer: WorkflowResponse, card: &Card, append: bool) {
        // A late answer for a workflow no longer open is dropped.
        if !matches!(&self.screen, Screen::Detail(open) | Screen::Delete(open) if open.id == card.id) {
            return;
        }
        match answer.and_then(|value| model::rows(&value).map(|rows| (rows.to_vec(), model::next(&value)))) {
            Ok((history, next)) => {
                if !append {
                    self.history.clear();
                }
                self.history.extend(history);
                self.history_next = next;
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
            .when_some(self.place.as_ref(), |view, (team, channel)| {
                view.child(
                    div()
                        .text_sm()
                        .text_color(rgb(palette.muted_foreground))
                        .child(format!("Destination: {} / {}", team.name, channel.name)),
                )
            })
            .child("Workflow name")
            .child(selected_by("workflow-name", Input::new(&self.name)))
            .when(matches!(self.editor_mode, EditorMode::Form), |view| {
                view.child(self.connections_view(palette, cx))
                    .child(self.config_fields(palette))
            })
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
                                this.screen = this.start_screen();
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
                Ok(mut properties) => {
                    self.carry_connection_choices(&mut properties);
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

    pub(super) fn toggle(&mut self, card: &Card, cx: &mut Context<Self>) {
        let Some(environment) = self.environment.clone() else {
            return;
        };
        self.changing = Some(Change::Toggle);
        self.mutate_then(
            WorkflowRequest::SetEnabled {
                environment,
                flow: card.id.clone(),
                enabled: !card.enabled(),
            },
            Outcome::Stay,
            cx,
        );
        if !self.busy {
            self.changing = None;
        }
    }

    /// Turns the Overview's name into a field holding it, selected, as Teams' Rename does.
    pub(super) fn start_rename(&mut self, card: &Card, window: &mut Window, cx: &mut Context<Self>) {
        if self.core.read_only() || self.busy {
            return;
        }
        let name = card.name.clone();
        let field = cx.new(|cx| InputState::new(window, cx).default_value(name));
        field.update(cx, |state, cx| {
            state.focus(window, cx);
            state.select_all(window, cx);
        });
        let subscription = cx.subscribe_in(&field, window, |this, _, event: &InputEvent, _, cx| match event {
            InputEvent::PressEnter { .. } => this.save_rename(cx),
            InputEvent::Change => cx.notify(),
            _ => {}
        });
        self.rename = Some((field, subscription));
        cx.notify();
    }

    /// Saves the new name: only the display name changes (Teams' `PATCH` of `properties.displayName`).
    pub(super) fn save_rename(&mut self, cx: &mut Context<Self>) {
        let (Some((field, _)), Some(environment), Screen::Detail(card)) =
            (&self.rename, &self.environment, &self.screen)
        else {
            return;
        };
        let name = field.read(cx).value().trim().to_owned();
        if name.is_empty() {
            return;
        }
        let request = WorkflowRequest::Rename {
            environment: environment.clone(),
            flow: card.id.clone(),
            name,
        };
        self.changing = Some(Change::Rename);
        self.mutate_then(request, Outcome::Stay, cx);
        if !self.busy {
            self.changing = None;
        }
    }
}
