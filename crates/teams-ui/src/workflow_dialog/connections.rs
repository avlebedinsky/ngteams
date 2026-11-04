//! Connector binding and Microsoft-hosted sign-in. Bearer tokens remain in the engine; the view accepts only a
//! service-issued HTTPS OAuth consent link on Microsoft's consent hosts, never a returned token-posting endpoint.
use super::*;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};

pub(super) struct Binding {
    pub key: String,
    pub connector: String,
    pub label: String,
    pub selected: Option<String>,
}

impl WorkflowDialog {
    pub(super) fn load_connections(&mut self, cx: &mut Context<Self>) {
        let Some(environment) = self.environment.clone() else {
            return;
        };
        let generation = self.generation;
        let reply = self.core.workflow_request(WorkflowRequest::Connections { environment });
        self.tasks
            .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let answer = reply.await.unwrap_or(Err(WorkflowFailure::Connection));
                let _ = this.update(cx, |this, cx| {
                    if generation != this.generation {
                        return;
                    }
                    match answer.and_then(|value| model::rows(&value).map(<[Value]>::to_vec)) {
                        Ok(connections) => {
                            this.connections = connections;
                            this.connection_error = None;
                        }
                        Err(error) => this.connection_error = Some(error),
                    }
                    cx.notify();
                });
            }));
    }

    pub(super) fn prepare_bindings(&mut self, properties: &Value) {
        self.bindings = properties
            .get("connectionReferences")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(key, value)| {
                let connector = value.pointer("/api/name").and_then(Value::as_str).or_else(|| {
                    value
                        .get("id")
                        .and_then(Value::as_str)
                        .and_then(|id| id.strip_prefix("/providers/Microsoft.PowerApps/apis/"))
                })?;
                let label = value
                    .pointer("/api/properties/displayName")
                    .and_then(Value::as_str)
                    .unwrap_or(connector);
                Some(Binding {
                    key: key.clone(),
                    connector: connector.to_owned(),
                    label: label.to_owned(),
                    selected: value.get("connectionName").and_then(Value::as_str).map(str::to_owned),
                })
            })
            .collect();
    }

    pub(super) fn carry_connection_choices(&self, properties: &mut Value) {
        let Some(references) = properties
            .get_mut("connectionReferences")
            .and_then(Value::as_object_mut)
        else {
            return;
        };
        for binding in &self.bindings {
            if let (Some(selected), Some(Value::Object(reference))) =
                (&binding.selected, references.get_mut(&binding.key))
            {
                reference.insert("connectionName".to_owned(), Value::String(selected.clone()));
            }
        }
    }

    pub(super) fn bind_connections(&self, properties: &mut Value) -> Result<(), &'static str> {
        let mut references = match properties.get("connectionReferences") {
            Some(Value::Object(references)) => references.clone(),
            None | Some(Value::Null) => serde_json::Map::new(),
            _ => return Err("The workflow connection references must be an object."),
        };
        if references.len() != self.bindings.len() {
            return Err("A connection reference has no supported app identity. Check the definition before saving.");
        }
        for binding in &self.bindings {
            let selected = binding
                .selected
                .as_deref()
                .ok_or("Choose a connected account for every app before saving.")?;
            let connected = self.connections.iter().any(|connection| {
                connection_name(connection) == Some(selected) && compatible(connection, &binding.connector)
            });
            if !connected {
                return Err("A selected connection is no longer connected. Refresh and choose another account.");
            }
            let reference = references
                .get_mut(&binding.key)
                .ok_or("The workflow connections changed. Reopen the configuration.")?;
            reference["connectionName"] = Value::String(selected.to_owned());
            reference["source"] = Value::String("Embedded".to_owned());
            reference["id"] = Value::String(format!("/providers/Microsoft.PowerApps/apis/{}", binding.connector));
            if reference.get("tier").is_none() {
                reference["tier"] = Value::String("NotSpecified".to_owned());
            }
        }
        properties["connectionReferences"] = Value::Object(references);
        Ok(())
    }

    pub(super) fn connections_view(&self, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let mut view = div().flex().flex_col().gap_3();
        if self.bindings.is_empty() {
            return view.into_any_element();
        }
        view = view.child(
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child("Connected accounts"),
        );
        if let Some(error) = &self.connection_error {
            view = view.child(
                div()
                    .text_sm()
                    .text_color(rgb(theme::DANGER))
                    .child(model::problem(error)),
            );
        }
        for (index, binding) in self.bindings.iter().enumerate() {
            view = view.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(binding.label.clone())
                    .child(self.connection_picker(index, binding, palette, cx)),
            );
        }
        view.child(selected_by(
            "workflow-connections-refresh",
            Button::new("workflow-connections-refresh")
                .ghost()
                .label("Refresh connections")
                .icon(Glyph::REFRESH.icon())
                .on_click(cx.listener(|this, _, _, cx| this.load_connections(cx))),
        ))
        .into_any_element()
    }

    fn connection_picker(&self, index: usize, binding: &Binding, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let connections: Vec<_> = self
            .connections
            .iter()
            .filter(|connection| compatible(connection, &binding.connector))
            .filter_map(|connection| {
                Some((
                    connection_name(connection)?.to_owned(),
                    connection
                        .pointer("/properties/displayName")
                        .and_then(Value::as_str)
                        .unwrap_or("Connected account")
                        .to_owned(),
                ))
            })
            .collect();
        let selected = binding.selected.clone();
        let label = connections
            .iter()
            .find(|(id, _)| Some(id) == selected.as_ref())
            .map_or("Choose account", |(_, label)| label.as_str())
            .to_owned();
        let weak = cx.entity().downgrade();
        let connector = binding.connector.clone();
        let picker = Button::new(format!("workflow-connection-{index}"))
            .outline()
            .label(label)
            .icon(Glyph::CHEVRON_DOWN.icon())
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.scrollable(true).max_h(px(260.0));
                for (id, label) in &connections {
                    let (id, label, weak) = (id.clone(), label.clone(), weak.clone());
                    let checked = Some(&id) == selected.as_ref();
                    menu = menu.item(connection_item(id, label, checked, weak, index, palette));
                }
                menu
            });
        div()
            .flex()
            .gap_2()
            .child(selected_by(format!("workflow-connection-{index}"), picker))
            .when(!self.core.read_only(), |row| {
                row.child(selected_by(
                    format!("workflow-connect-{index}"),
                    Button::new(format!("workflow-connect-{index}"))
                        .label("Sign in")
                        .disabled(self.busy)
                        .on_click(cx.listener(move |this, _, _, cx| this.connect(&connector, cx))),
                ))
            })
            .into_any_element()
    }

    fn connect(&mut self, connector: &str, cx: &mut Context<Self>) {
        let Some(environment) = self.environment.clone() else {
            return;
        };
        if self.busy || self.core.read_only() {
            return;
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let connection = format!("ngteams-{stamp:x}");
        let connector = connector.to_owned();
        let core = Arc::clone(&self.core);
        let generation = self.generation;
        let reply = core.workflow_request(WorkflowRequest::CreateConnection {
            environment: environment.clone(),
            connector: connector.clone(),
            connection: connection.clone(),
        });
        self.busy = true;
        self.tasks.push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let created = reply.await.unwrap_or(Err(WorkflowFailure::Connection));
            let answer = match created { Ok(_) => core.workflow_request(WorkflowRequest::ConnectionConsent { environment, connector, connection }).await.unwrap_or(Err(WorkflowFailure::Connection)), Err(error) => Err(error) };
            let _ = this.update(cx, |this, cx| {
                if generation != this.generation { return; }
                this.busy = false;
                match answer {
                    Ok(value) => match value.get("consentLink").and_then(Value::as_str).filter(|link| consent_link(link)) {
                        Some(link) => { cx.open_url(link); this.notice = Some("Complete sign-in in your browser, then refresh connections.".to_owned()); }
                        None => this.error = Some("The connection service did not return a supported Microsoft sign-in link. Refresh connections to check its state.".to_owned()),
                    },
                    Err(error) => this.error = Some(model::problem(&error).to_owned()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
}
fn connection_name(value: &Value) -> Option<&str> {
    value.get("name").and_then(Value::as_str)
}
fn compatible(value: &Value, connector: &str) -> bool {
    value
        .pointer("/properties/apiId")
        .and_then(Value::as_str)
        .is_some_and(|id| id == format!("/providers/Microsoft.PowerApps/apis/{connector}"))
        && value
            .pointer("/properties/statuses")
            .and_then(Value::as_array)
            .is_some_and(|statuses| {
                statuses
                    .iter()
                    .any(|status| status.get("status").and_then(Value::as_str) == Some("Connected"))
            })
}
fn consent_link(link: &str) -> bool {
    teams_proto::workflow::is_workflow_consent_link(link)
}

fn connection_item(
    id: String,
    label: String,
    checked: bool,
    weak: WeakEntity<WorkflowDialog>,
    index: usize,
    palette: Palette,
) -> PopupMenuItem {
    PopupMenuItem::element(move |_, _| div().text_color(rgb(palette.foreground)).child(label.clone()))
        .checked(checked)
        .on_click(move |_, _, cx| {
            let _ = weak.update(cx, |this, cx| {
                if let Some(binding) = this.bindings.get_mut(index) {
                    binding.selected = Some(id.clone());
                }
                cx.notify();
            });
        })
}
