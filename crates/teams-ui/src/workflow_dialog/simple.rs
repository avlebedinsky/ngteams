//! Teams' card editor for a template whose own markup names its choices (the webhook template's team and channel),
//! as the Teams Workflows app shows it: one collapsible card per template, no definition, field names or connection
//! controls. The Microsoft Teams connection is created and signed in behind the scenes the way that app does it, so
//! the user only picks the team and the channel. A template whose markup names anything else keeps the full editor.
use super::*;
use gpui_component::Sizable as _;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};

/// The connector this editor signs in by itself: the only one the Teams reference was seen doing it for.
const TEAMS_API: &str = "shared_teams";

/// The card editor's state: the template's own markup, the definition it fills and the user's two choices.
pub(super) struct Simple {
    template: String,
    name: String,
    description: String,
    apps: Vec<String>,
    markup: Value,
    definition: Value,
    references: Value,
    flow: Option<Card>,
    team: Option<String>,
    channel: Option<String>,
    expanded: bool,
    link: Link,
}

/// The Microsoft Teams connection the saved workflow posts with.
#[derive(Clone)]
pub(super) enum Link {
    Checking,
    Ready(String),
    Failed(WorkflowFailure),
}

impl Simple {
    /// The editor for a gallery template (its `metadata.userIntentMarkup`) or for a workflow saved from one (the
    /// markup its `Do_Not_Remove_FlowIL` action keeps), when every choice is a team or a channel and every app is
    /// Microsoft Teams.
    pub(super) fn open(card: &Card, existing: bool) -> Option<Self> {
        let properties = &card.properties;
        let markup = if existing {
            let kept = properties
                .pointer("/definition/actions/Do_Not_Remove_FlowIL/metadata/userIntentMarkup")?
                .as_str()?;
            serde_json::from_str::<Value>(kept).ok()?
        } else {
            properties.pointer("/metadata/userIntentMarkup")?.clone()
        };
        let template = markup.pointer("/automation/template")?;
        let parameters = template.get("parameters")?.as_array()?;
        if parameters.is_empty()
            || !parameters.iter().all(|parameter| {
                matches!(
                    parameter.get("typeName").and_then(Value::as_str),
                    Some("Teams.Group" | "Teams.Channel")
                )
            })
        {
            return None;
        }
        let references = properties.get("connectionReferences").cloned().unwrap_or(Value::Null);
        let apis = references.as_object()?;
        if apis.is_empty() || !apis.values().all(|reference| api_name(reference) == Some(TEAMS_API)) {
            return None;
        }
        let value = |kind: &str| {
            parameters
                .iter()
                .find(|parameter| parameter.get("typeName").and_then(Value::as_str) == Some(kind))
                .and_then(|parameter| parameter.get("value"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        let name = if existing {
            template
                .get("displayName")
                .and_then(Value::as_str)
                .unwrap_or(&card.name)
                .to_owned()
        } else {
            card.name.clone()
        };
        Some(Self {
            template: template
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or(&card.id)
                .to_owned(),
            name,
            description: properties
                .get("description")
                .or_else(|| properties.pointer("/definition/description"))
                .and_then(Value::as_str)
                .unwrap_or(&card.description)
                .to_owned(),
            apps: vec!["Microsoft Teams".to_owned()],
            definition: properties.get("definition")?.clone(),
            references,
            markup: markup.clone(),
            flow: existing.then(|| card.clone()),
            team: value("Teams.Group"),
            channel: value("Teams.Channel"),
            expanded: true,
            link: Link::Checking,
        })
    }
}

impl Simple {
    pub(super) fn collapse(&mut self) {
        self.expanded = false;
    }
}

fn api_name(reference: &Value) -> Option<&str> {
    reference.pointer("/api/name").and_then(Value::as_str).or_else(|| {
        reference
            .get("id")
            .and_then(Value::as_str)
            .and_then(|id| id.strip_prefix("/providers/Microsoft.PowerApps/apis/"))
    })
}

impl WorkflowDialog {
    /// Opens the card editor, its choices defaulting to this channel for a new workflow, and makes sure of the
    /// Microsoft Teams connection.
    pub(super) fn open_simple(&mut self, mut simple: Simple, cx: &mut Context<Self>) {
        if simple.flow.is_none() {
            simple.team = Some(self.team.group_id.clone().unwrap_or_else(|| self.team.id.clone()));
            simple.channel = Some(self.channel.id.clone());
        }
        self.error = None;
        self.notice = None;
        self.simple = Some(simple);
        self.screen = Screen::Edit {
            flow: self.simple.as_ref().and_then(|simple| simple.flow.clone()),
        };
        self.ensure_teams_connection(cx);
        cx.notify();
    }

    /// Reads the environment's connections and uses a signed-in Microsoft Teams one; without one, creates and signs
    /// one in as the Teams Workflows app does.
    fn ensure_teams_connection(&mut self, cx: &mut Context<Self>) {
        let Some(environment) = self.environment.clone() else {
            self.set_link(Link::Failed(WorkflowFailure::NotFound));
            return;
        };
        let generation = self.generation;
        let core = Arc::clone(&self.core);
        self.tasks
            .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let link = teams_connection(&core, environment).await;
                let _ = this.update(cx, |this, cx| {
                    if this.generation == generation {
                        this.set_link(link);
                        cx.notify();
                    }
                });
            }));
    }

    fn set_link(&mut self, link: Link) {
        if let Some(simple) = &mut self.simple {
            simple.link = link;
        }
    }

    pub(super) fn simple_save(&mut self, cx: &mut Context<Self>) {
        if self.core.read_only() || self.busy {
            return;
        }
        let Some(environment) = self.environment.clone() else {
            self.error = Some("No Power Automate environment is available for this account.".to_owned());
            cx.notify();
            return;
        };
        let request = match self.simple_properties() {
            Ok(properties) => match self.simple.as_ref().and_then(|simple| simple.flow.clone()) {
                Some(flow) => WorkflowRequest::Update {
                    environment,
                    flow: flow.id.clone(),
                    expected_modified: flow
                        .properties
                        .get("lastModifiedTime")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    properties,
                },
                None => WorkflowRequest::Create {
                    environment,
                    properties,
                },
            },
            Err(error) => {
                self.error = Some(error.to_owned());
                cx.notify();
                return;
            }
        };
        self.mutate_then(request, Outcome::Overview, cx);
    }

    /// The workflow Teams saves: the template's definition sent to the chosen channel, the markup kept with the
    /// choices and their names so Teams can edit it again, and the signed-in Microsoft Teams connection.
    fn simple_properties(&self) -> Result<Value, &'static str> {
        let simple = self.simple.as_ref().ok_or("Open the workflow again.")?;
        let team = simple
            .team
            .as_deref()
            .and_then(|group| self.team_by_group(group))
            .ok_or("Choose the team the channel is in.")?;
        let group = team.group_id.as_deref().unwrap_or(&team.id);
        let channel = simple
            .channel
            .as_deref()
            .and_then(|id| team.channels.iter().find(|channel| channel.id == id))
            .ok_or("Choose a channel.")?;
        let connection = match &simple.link {
            Link::Ready(name) => name.clone(),
            Link::Checking => return Err("Microsoft Teams is still connecting. Try again in a moment."),
            Link::Failed(_) => return Err("Microsoft Teams could not be connected. Close Workflows and try again."),
        };
        let mut properties = serde_json::json!({"definition": simple.definition.clone()});
        form::apply_channel(&mut properties, group, &channel.id);
        let mut definition = properties["definition"].take();
        definition["description"] = Value::String(simple.description.clone());
        keep_markup(
            &mut definition,
            simple,
            (group, &team.name),
            (&channel.id, &channel.name),
        );
        let references: serde_json::Map<String, Value> = simple
            .references
            .as_object()
            .into_iter()
            .flatten()
            .map(|(key, _)| {
                (
                    key.clone(),
                    serde_json::json!({
                        "id": format!("/providers/Microsoft.PowerApps/apis/{TEAMS_API}"),
                        "connectionName": connection,
                        "source": "NotSpecified",
                    }),
                )
            })
            .collect();
        let mut properties = serde_json::json!({
            "definition": definition,
            "connectionReferences": references,
            "displayName": summary(simple, Some(&channel.name)),
        });
        if simple.flow.is_none() {
            properties["templateName"] = Value::String(simple.template.clone());
        }
        Ok(properties)
    }

    fn team_by_group(&self, group: &str) -> Option<&Team> {
        self.teams
            .iter()
            .find(|team| team.group_id.as_deref().unwrap_or(&team.id) == group)
    }

    fn channel_name(&self) -> Option<String> {
        let simple = self.simple.as_ref()?;
        let team = self.team_by_group(simple.team.as_deref()?)?;
        let channel = simple.channel.as_deref()?;
        team.channels
            .iter()
            .find(|candidate| candidate.id == channel)
            .map(|channel| channel.name.clone())
    }

    pub(super) fn simple_editor(&self, simple: &Simple, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let heading = simple
            .flow
            .as_ref()
            .map_or_else(|| "Untitled workflow".to_owned(), |flow| flow.name.clone());
        div()
            .flex()
            .flex_col()
            .gap_8()
            .px_12()
            .pt_2()
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
                            .child(selected_by(
                                "workflow-simple-back",
                                icon_button("workflow-simple-back", Glyph::CHEVRON_LEFT, "Back")
                                    .ghost()
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|this, _, _, cx| this.leave_simple(cx))),
                            ))
                            .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(heading)),
                    )
                    .child(selected_by(
                        "workflow-collapse-all",
                        Button::new("workflow-collapse-all")
                            .ghost()
                            .small()
                            .label(if simple.expanded { "Collapse all" } else { "Expand all" })
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_card(cx))),
                    )),
            )
            .child(self.simple_card(simple, palette, cx))
            .into_any_element()
    }

    fn simple_card(&self, simple: &Simple, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let channel = self.channel_name();
        div()
            .id("workflow-simple-card")
            .debug_selector(|| "workflow-simple-card".to_owned())
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded_xl()
            .border_1()
            .border_color(rgb(palette.border))
            .hover(|card| card.border_color(rgb(palette.foreground)))
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .child(div().flex().gap_2().children(simple.apps.iter().map(|app| {
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .px_2()
                            .py_0p5()
                            .rounded_full()
                            .text_sm()
                            .bg(rgb(palette.hover))
                            .child(gpui::img(crate::app_icon::image()).size(px(16.0)))
                            .child(app.clone())
                    })))
                    .child(selected_by(
                        "workflow-card-toggle",
                        icon_button(
                            "workflow-card-toggle",
                            if simple.expanded {
                                Glyph::CHEVRON_UP
                            } else {
                                Glyph::CHEVRON_DOWN
                            },
                            if simple.expanded { "Collapse" } else { "Expand" },
                        )
                        .ghost()
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_card(cx))),
                    )),
            )
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(summary(simple, channel.as_deref())),
            )
            .child(
                div()
                    .text_color(rgb(palette.muted_foreground))
                    .child(simple.description.clone()),
            )
            .when(simple.expanded, |card| {
                card.child(self.team_field(simple, palette, cx))
                    .child(self.channel_field(simple, palette, cx))
            })
            .when_some(
                match &simple.link {
                    Link::Failed(error) => Some(error),
                    Link::Checking | Link::Ready(_) => None,
                },
                |card, error| {
                    card.child(
                        div()
                            .debug_selector(|| "workflow-simple-problem".to_owned())
                            .text_sm()
                            .text_color(rgb(theme::DANGER))
                            .child(format!(
                                "Microsoft Teams could not be connected. {}",
                                model::problem(error)
                            )),
                    )
                },
            )
            .into_any_element()
    }

    fn toggle_card(&mut self, cx: &mut Context<Self>) {
        if let Some(simple) = &mut self.simple {
            simple.expanded = !simple.expanded;
        }
        cx.notify();
    }

    fn leave_simple(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.error = None;
        self.screen = match self.simple.take().and_then(|simple| simple.flow) {
            Some(flow) => Screen::Detail(flow),
            None => Screen::Browse,
        };
        cx.notify();
    }

    fn team_field(&self, simple: &Simple, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let teams: Vec<(String, String)> = self
            .teams
            .iter()
            .filter(|team| !team.archived)
            .map(|team| {
                (
                    team.group_id.clone().unwrap_or_else(|| team.id.clone()),
                    team.name.clone(),
                )
            })
            .collect();
        let label = if has_label(simple, "Teams.Group") {
            "Team the channel is in"
        } else {
            "Team"
        };
        field(
            (label, "workflow-simple-team"),
            teams,
            simple.team.clone(),
            |this, group| {
                if let Some(simple) = &mut this.simple
                    && simple.team.as_deref() != Some(&group)
                {
                    simple.team = Some(group);
                    simple.channel = None;
                }
            },
            palette,
            cx,
        )
    }

    fn channel_field(&self, simple: &Simple, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let channels: Vec<(String, String)> = simple
            .team
            .as_deref()
            .and_then(|group| self.team_by_group(group))
            .map(|team| {
                team.channels
                    .iter()
                    .map(|channel| (channel.id.clone(), channel.name.clone()))
                    .collect()
            })
            .unwrap_or_default();
        field(
            ("Channel", "workflow-simple-channel"),
            channels,
            simple.channel.clone(),
            |this, channel| {
                if let Some(simple) = &mut this.simple {
                    simple.channel = Some(channel);
                }
            },
            palette,
            cx,
        )
    }

    /// The card editor's footer: Save and Back, as in Teams.
    pub(super) fn simple_footer(&self, cx: &Context<Self>) -> AnyElement {
        let connecting = self
            .simple
            .as_ref()
            .is_some_and(|simple| matches!(simple.link, Link::Checking));
        div()
            .flex()
            .gap_2()
            .child(selected_by(
                "workflow-save",
                Button::new("workflow-save")
                    .primary()
                    .min_w(px(96.0))
                    .label(if self.busy { "Saving…" } else { "Save" })
                    .loading(self.busy)
                    .disabled(self.busy || connecting || self.uncertain.is_some())
                    .on_click(cx.listener(|this, _, _, cx| this.simple_save(cx))),
            ))
            .child(selected_by(
                "workflow-edit-cancel",
                Button::new("workflow-edit-cancel")
                    .outline()
                    .min_w(px(96.0))
                    .label("Back")
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.leave_simple(cx))),
            ))
            .into_any_element()
    }
}

/// A required choice drawn as Teams' dropdown: its label with a red star, then the whole-width field.
fn field(
    (label, id): (&'static str, &'static str),
    options: Vec<(String, String)>,
    chosen: Option<String>,
    choose: fn(&mut WorkflowDialog, String),
    palette: Palette,
    cx: &Context<WorkflowDialog>,
) -> AnyElement {
    let shown = options
        .iter()
        .find(|(value, _)| Some(value) == chosen.as_ref())
        .map(|(_, name)| name.clone())
        .unwrap_or_default();
    let weak = cx.entity().downgrade();
    let face = div()
        .w_full()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(shown),
        )
        .child(
            Glyph::CHEVRON_DOWN
                .icon()
                .size(px(16.0))
                .text_color(rgb(palette.muted_foreground)),
        );
    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .child(
            div()
                .flex()
                .gap_1()
                .child(label)
                .child(div().text_color(rgb(theme::DANGER)).child("*")),
        )
        .child(selected_by(
            id,
            Button::new(id)
                .outline()
                .w_full()
                .h(px(36.0))
                .child(face)
                .dropdown_menu(move |menu, _, _| {
                    let menu = menu.min_w(px(480.0)).scrollable(true).max_h(px(280.0));
                    options.iter().fold(menu, |menu, (value, name)| {
                        let (value, name, weak) = (value.clone(), name.clone(), weak.clone());
                        let selector = format!("{id}-{value}");
                        menu.item(
                            PopupMenuItem::element(move |_, _| {
                                let selector = selector.clone();
                                div()
                                    .debug_selector(move || selector.clone())
                                    .text_color(rgb(palette.foreground))
                                    .child(name.clone())
                            })
                            .checked(chosen.as_ref() == Some(&value))
                            .on_click(move |_, _, cx| {
                                let _ = weak.update(cx, |this, cx| {
                                    choose(this, value.clone());
                                    cx.notify();
                                });
                            }),
                        )
                    })
                }),
        ))
        .into_any_element()
}

fn has_label(simple: &Simple, kind: &str) -> bool {
    let Some(template) = simple.markup.pointer("/automation/template") else {
        return false;
    };
    let id = template
        .get("parameters")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|parameter| parameter.get("typeName").and_then(Value::as_str) == Some(kind))
        .and_then(|parameter| parameter.get("id"));
    template
        .get("editorBindings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|binding| binding.get("parameterId") == id && binding.get("label").is_some())
}

/// The card's title as Teams words it: a template that is sent "to a channel" names the chosen channel.
fn summary(simple: &Simple, channel: Option<&str>) -> String {
    let names_channel = simple
        .markup
        .pointer("/automation/template/summary")
        .and_then(Value::as_str)
        .is_some_and(|summary| summary.contains("TeamsChannel"));
    match channel {
        Some(channel) if names_channel && simple.name.contains(" a channel") => {
            simple.name.replacen(" a channel", &format!(" {channel}"), 1)
        }
        _ => simple.name.clone(),
    }
}

/// Keeps the template's markup in the definition, with the chosen values and their names, as Teams does: its
/// `Do_Not_Remove_FlowIL` action runs first, so the template's first actions run after it.
fn keep_markup(definition: &mut Value, simple: &Simple, team: (&str, &str), channel: (&str, &str)) {
    let mut markup = simple.markup.clone();
    if let Some(parameters) = markup
        .pointer_mut("/automation/template/parameters")
        .and_then(Value::as_array_mut)
    {
        for parameter in parameters {
            let (value, name) = match parameter.get("typeName").and_then(Value::as_str) {
                Some("Teams.Group") => team,
                Some("Teams.Channel") => channel,
                _ => continue,
            };
            parameter["value"] = Value::String(value.to_owned());
            parameter["valueDisplayName"] = Value::String(name.to_owned());
        }
    }
    if let Some(template) = markup.pointer_mut("/automation/template") {
        template["displayName"] = Value::String(simple.name.clone());
    }
    let Some(actions) = definition.get_mut("actions").and_then(Value::as_object_mut) else {
        return;
    };
    if !actions.contains_key("Do_Not_Remove_FlowIL") {
        for action in actions.values_mut() {
            if action
                .get("runAfter")
                .and_then(Value::as_object)
                .is_none_or(serde_json::Map::is_empty)
            {
                action["runAfter"] = serde_json::json!({"Do_Not_Remove_FlowIL": ["Succeeded"]});
            }
        }
    }
    actions.insert(
        "Do_Not_Remove_FlowIL".to_owned(),
        serde_json::json!({
            "type": "InitializeVariable",
            "runAfter": {},
            "inputs": {"variables": [{"name": "flow.IL (DO NOT REMOVE)", "type": "string", "value": "null"}]},
            "metadata": {"userIntentMarkup": markup.to_string()},
        }),
    );
}

/// A signed-in Microsoft Teams connection of the environment, made and signed in when there is none.
async fn teams_connection(core: &Arc<dyn Core>, environment: String) -> Link {
    let listed = core
        .workflow_request(WorkflowRequest::Connections {
            environment: environment.clone(),
        })
        .await
        .unwrap_or(Err(WorkflowFailure::Connection))
        .and_then(|value| model::rows(&value).map(<[Value]>::to_vec));
    let connections = match listed {
        Ok(connections) => connections,
        Err(error) => return Link::Failed(error),
    };
    if let Some(name) = connections.iter().find_map(|connection| {
        (connections::is_connected(connection, TEAMS_API))
            .then(|| connection.get("name").and_then(Value::as_str))
            .flatten()
    }) {
        return Link::Ready(name.to_owned());
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let connection = format!("shared-teams-{stamp:032x}");
    match core
        .workflow_request(WorkflowRequest::AuthorizeConnection {
            environment,
            connector: TEAMS_API.to_owned(),
            connection: connection.clone(),
        })
        .await
        .unwrap_or(Err(WorkflowFailure::Connection))
    {
        Ok(_) => Link::Ready(connection),
        Err(error) => Link::Failed(error),
    }
}
