//! Teams' card editor, for every template whose own markup names its choices (`metadata.userIntentMarkup`): one
//! collapsible card per template with the apps it uses, its title and description, and one field per choice (a team
//! and a channel, a schedule, a message, a plan, a SharePoint site, list and folder, a mail folder, people, a subject),
//! as the Teams Workflows app shows it. No definition, field names or connection controls: each app's connection is
//! made and signed in behind the scenes the way that app does it, and an edited workflow keeps its own connections.
use std::collections::BTreeMap;

use super::*;
use gpui_component::date_picker::DatePickerState;
use teams_proto::TeamPerson;

use super::combo::Combo;
use super::markup::{self, Kind, Param};

/// The card editor's state: the template's own markup, the definition it fills and the user's choices.
pub(super) struct Simple {
    template: String,
    /// The template's name ("Send webhook alerts to a channel"): the markup keeps it.
    title: String,
    pub(super) description: String,
    markup: Value,
    definition: Value,
    /// The template's (or the workflow's) connection references, by key.
    references: Value,
    pub(super) flow: Option<Card>,
    /// Edit was chosen in the workflow list's menu: Save and Back return to the list, as in Teams.
    pub(super) from_list: bool,
    pub(super) params: Vec<Param>,
    /// A template run from a message (its markup's `context.triggerType` is `messageAction`): with no choices to make,
    /// its card shows how to start it from a message instead of fields, as Teams' does.
    pub(super) message_action: bool,
    bindings: Vec<markup::Binding>,
    pub(super) fields: Vec<Field>,
    pub(super) expanded: bool,
    /// Each app's connection, by API name (`shared_teams`), in the order the card's chips show them.
    pub(super) links: Vec<(String, Link)>,
    /// Connector operations' inputs, read once: (API, operation) to its `inputsDefinition.properties`.
    pub(super) operations: BTreeMap<(String, String), Option<Value>>,
}

/// One field's controls and what it has listed.
#[derive(Default)]
pub(super) struct Field {
    pub(super) combo: Option<Combo>,
    pub(super) input: Option<Entity<InputState>>,
    /// A message box: its editor and Teams' formatting bar over it.
    pub(super) message: Option<(Entity<crate::editor::Editor>, Entity<crate::editor::toolbar::FormatBar>)>,
    pub(super) date: Option<Entity<DatePickerState>>,
    pub(super) choices: Choices,
    /// How many choice reads were started for the field: only the latest one's answer is taken, so a list read for a
    /// value chosen before (another site, another group) never fills it.
    pub(super) listed: u64,
    /// A folder field's open list and the folders browsed into.
    pub(super) folder_open: bool,
    pub(super) folder_path: Vec<Choice>,
    /// A people field's search: its text and who it found.
    pub(super) found: Option<(String, Vec<TeamPerson>)>,
    pub(super) asked: u64,
    /// What is wrong with the value typed, shown under the field.
    pub(super) problem: Option<&'static str>,
    pub(super) subscriptions: Vec<Subscription>,
}

/// What a connector listed for a field.
#[derive(Default)]
pub(super) enum Choices {
    #[default]
    Idle,
    Loading,
    Ready(Vec<Choice>),
    Failed,
}

/// One choice a connector offered: its value, its name, and for a folder whether it holds folders and how to browse
/// into it.
#[derive(Clone, Debug)]
pub(super) struct Choice {
    pub(super) value: String,
    pub(super) name: String,
    pub(super) full: String,
    pub(super) parent: bool,
    pub(super) selection: Value,
}

/// An app's connection the saved workflow uses.
#[derive(Clone)]
pub(super) enum Link {
    Checking,
    Ready(String),
    Failed(WorkflowFailure),
}

impl Simple {
    /// The editor for a gallery template (its `metadata.userIntentMarkup`) or for a workflow saved from one (the
    /// markup its `Do_Not_Remove_FlowIL` action keeps).
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
        let params = markup::params(&markup);
        let message_action = markup
            .pointer("/automation/context/triggerType")
            .and_then(Value::as_str)
            == Some("messageAction");
        if params.is_empty() && !message_action {
            return None;
        }
        let references = properties.get("connectionReferences").cloned().unwrap_or(Value::Null);
        let definition = properties.get("definition")?.clone();
        let mut apis: Vec<String> = references
            .as_object()?
            .values()
            .filter_map(api_name)
            .map(str::to_owned)
            .collect();
        if apis.is_empty() {
            return None;
        }
        apis.sort_by_key(|api| chip_rank(api, &definition));
        apis.dedup();
        Some(Self {
            template: template
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or(&card.id)
                .to_owned(),
            title: if existing {
                template
                    .get("displayName")
                    .and_then(Value::as_str)
                    .unwrap_or(&card.name)
                    .to_owned()
            } else {
                card.name.clone()
            },
            description: properties
                .get("description")
                .or_else(|| properties.pointer("/definition/description"))
                .and_then(Value::as_str)
                .unwrap_or(&card.description)
                .to_owned(),
            bindings: markup::bindings(&markup),
            fields: Vec::new(),
            params,
            message_action,
            definition,
            references,
            markup: markup.clone(),
            flow: existing.then(|| card.clone()),
            from_list: false,
            expanded: true,
            links: apis.into_iter().map(|api| (api, Link::Checking)).collect(),
            operations: BTreeMap::new(),
        })
    }

    pub(super) fn collapse(&mut self) {
        self.expanded = false;
    }

    /// The card's title as it reads now.
    pub(super) fn summary(&self) -> String {
        // A summary the markup spells out rather than naming a resource is the card's title as it stands.
        match self
            .markup
            .pointer("/automation/template/summary")
            .and_then(Value::as_str)
        {
            Some(literal) if !literal.contains("${") && !literal.trim().is_empty() => literal.to_owned(),
            _ => markup::summary(&self.template, &self.title, &self.params),
        }
    }

    /// The team a channel field lists the channels of: the team field it depends on, else the card's first.
    pub(super) fn team_of(&self, channel: &Param) -> Option<String> {
        self.params
            .iter()
            .find(|param| param.kind == Kind::TeamsGroup && channel.dependencies.contains(&param.id))
            .or_else(|| self.params.iter().find(|param| param.kind == Kind::TeamsGroup))
            .and_then(|param| param.text().map(str::to_owned))
    }

    /// The connection made for an app, once it is ready.
    pub(super) fn connection(&self, api: &str) -> Option<&str> {
        self.links.iter().find_map(|(name, link)| match link {
            Link::Ready(connection) if name == api => Some(connection.as_str()),
            _ => None,
        })
    }

    fn set_links(&mut self, links: Vec<(String, Link)>) {
        for (api, link) in links {
            if let Some((_, slot)) = self.links.iter_mut().find(|(name, _)| *name == api) {
                *slot = link;
            }
        }
    }

    /// The template's own name ("Follow up on a message"): what a message's menu lists the workflow by.
    pub(super) fn template_name(&self) -> String {
        self.title.clone()
    }

    pub(super) fn connecting(&self) -> bool {
        self.links.iter().any(|(_, link)| matches!(link, Link::Checking))
    }

    pub(super) fn bindings(&self) -> &[markup::Binding] {
        &self.bindings
    }

    pub(super) fn definition(&self) -> &Value {
        &self.definition
    }

    /// Whether every required field holds a valid value: Teams keeps Save off until then.
    pub(super) fn complete(&self) -> bool {
        self.params.iter().all(|param| match param.kind {
            Kind::Fixed => true,
            Kind::Interval => markup::interval(&param.value).is_some(),
            _ => param.optional || !param.empty(),
        })
    }
}

/// A reference's connector (`shared_teams`), from its API object or its id.
fn api_name(reference: &Value) -> Option<&str> {
    reference.pointer("/api/name").and_then(Value::as_str).or_else(|| {
        reference
            .get("id")
            .and_then(Value::as_str)
            .and_then(|id| id.strip_prefix("/providers/Microsoft.PowerApps/apis/"))
    })
}

/// The order Teams' chips show apps in: the trigger's first, then Office 365 Users, Microsoft Teams, SharePoint.
fn chip_rank(api: &str, definition: &Value) -> usize {
    let trigger = definition
        .get("triggers")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|triggers| triggers.values())
        .find_map(|trigger| trigger.pointer("/inputs/host/apiId").and_then(Value::as_str))
        .and_then(|id| id.rsplit('/').next());
    if trigger == Some(api) {
        return 0;
    }
    // a template run from a message: Microsoft Teams (its trigger's app) first, then the others as the template
    // names them
    if teams_proto::workflow::message_trigger(definition).is_some() {
        return usize::from(api != "shared_teams");
    }
    match api {
        "shared_office365users" => 1,
        "shared_teams" => 2,
        "shared_sharepointonline" => 3,
        _ => 4,
    }
}

/// The name Teams' chip gives an app.
pub(super) fn app_name(api: &str) -> &str {
    match api {
        "shared_teams" => "Microsoft Teams",
        "shared_planner" => "Planner",
        "shared_office365users" => "Office 365 Users",
        "shared_sharepointonline" => "SharePoint",
        "shared_office365" => "Office 365 Outlook",
        other => other.strip_prefix("shared_").unwrap_or(other),
    }
}

impl WorkflowDialog {
    /// Opens the card editor: a new workflow's fields default as Teams' do (this channel, now, this time zone), the
    /// controls are made, and each app's connection is made sure of.
    pub(super) fn open_simple(&mut self, mut simple: Simple, window: &mut Window, cx: &mut Context<Self>) {
        if simple.flow.is_none() {
            self.default_fields(&mut simple);
        }
        simple.fields = (0..simple.params.len())
            .map(|index| Self::field_controls(&simple, index, window, cx))
            .collect();
        self.error = None;
        self.notice = None;
        self.screen = Screen::Edit {
            flow: simple.flow.clone(),
        };
        self.simple = Some(simple);
        self.show_choices(window, cx);
        self.ensure_connections(cx);
        cx.notify();
    }

    /// Teams' defaults for a new workflow: the channel Workflows was opened from (none from the Workflows app, whose
    /// team field starts empty), a weekly interval of one, today, the next five minutes, the local time zone, and
    /// the team's own SharePoint site.
    fn default_fields(&self, simple: &mut Simple) {
        let now = chrono::Local::now().naive_local();
        let place = self.place.as_ref();
        for param in &mut simple.params {
            if !param.empty() {
                continue;
            }
            let (value, display) = match (param.kind, place) {
                (Kind::TeamsGroup, Some((team, _))) => (
                    Value::String(team.group_id.clone().unwrap_or_else(|| team.id.clone())),
                    Some(team.name.clone()),
                ),
                (Kind::TeamsChannel, Some((_, channel))) => {
                    (Value::String(channel.id.clone()), Some(channel.name.clone()))
                }
                (Kind::TeamsGroup | Kind::TeamsChannel, None) => continue,
                (kind, _) => match self.default_value(kind, param, now) {
                    Some(default) => default,
                    None => continue,
                },
            };
            param.value = value;
            param.display = display;
        }
    }

    /// The default of a field that doesn't name the channel.
    fn default_value(&self, kind: Kind, param: &Param, now: chrono::NaiveDateTime) -> Option<(Value, Option<String>)> {
        Some(match kind {
            Kind::Interval => (Value::from(1), None),
            Kind::StartDate => {
                let (value, shown) = markup::date_value(now.date());
                (Value::String(value), Some(shown))
            }
            Kind::TimeOfDay => {
                let (hour, minute) = markup::default_time(now);
                (
                    Value::String(format!("{hour:02}:{minute:02}")),
                    Some(markup::clock_name(hour, minute)),
                )
            }
            Kind::TimeZone => {
                let (id, name) = zones::local();
                (Value::String(id.to_owned()), Some(name.to_owned()))
            }
            Kind::Choice if param.type_name == "SharePoint.Site" => {
                let (team, _) = self.place.as_ref()?;
                (Value::String(team.about.site.clone()?), Some(team.name.clone()))
            }
            _ => return None,
        })
    }

    /// Shows each combobox's chosen name (once the teams are read, too).
    pub(super) fn show_choices(&mut self, window: &mut Window, cx: &mut App) {
        let names: Vec<Option<String>> = match &self.simple {
            Some(simple) => simple
                .params
                .iter()
                .map(|param| {
                    if param.empty() {
                        return None;
                    }
                    match param.kind {
                        Kind::TeamsGroup => param
                            .text()
                            .and_then(|group| self.team_by_group(group))
                            .map(|team| team.name.clone())
                            .or_else(|| param.display.clone()),
                        Kind::TeamsChannel => simple
                            .team_of(param)
                            .and_then(|group| self.team_by_group(&group))
                            .and_then(|team| {
                                team.channels
                                    .iter()
                                    .find(|channel| Some(channel.id.as_str()) == param.text())
                            })
                            .map(|channel| channel.name.clone())
                            .or_else(|| param.display.clone()),
                        _ => param.display.clone().or_else(|| param.text().map(str::to_owned)),
                    }
                })
                .collect(),
            None => return,
        };
        if let Some(simple) = &mut self.simple {
            for ((param, field), name) in simple.params.iter_mut().zip(&mut simple.fields).zip(names) {
                if matches!(param.kind, Kind::TeamsGroup | Kind::TeamsChannel) && name.is_some() {
                    param.display.clone_from(&name);
                }
                if let Some(combo) = &mut field.combo {
                    combo.show(name.unwrap_or_default());
                }
            }
        }
        self.sync_combos(window, cx);
    }

    /// A field changed: the fields that depend on it are cleared and list again.
    pub(super) fn dependents_changed(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(simple) = &mut self.simple else {
            return;
        };
        let Some(id) = simple.params.get(index).map(|param| param.id.clone()) else {
            return;
        };
        let mut cleared = Vec::new();
        for (at, param) in simple.params.iter_mut().enumerate() {
            if param.dependencies.contains(&id) {
                param.value = Value::Null;
                param.display = None;
                cleared.push(at);
            }
        }
        for at in &cleared {
            if let Some(field) = simple.fields.get_mut(*at) {
                field.choices = Choices::Idle;
                field.folder_path.clear();
                if let Some(combo) = &mut field.combo {
                    combo.show(String::new());
                }
            }
        }
        for at in cleared {
            self.load_choices(at, cx);
        }
    }

    /// Makes sure of each app's connection: an edited workflow keeps its own; otherwise a signed-in one of the
    /// environment is used, or one is made and signed in as the Teams Workflows app does, reusing one a failed
    /// sign-in left behind rather than making another.
    fn ensure_connections(&mut self, cx: &mut Context<Self>) {
        let Some(simple) = &mut self.simple else {
            return;
        };
        let Some(environment) = self.environment.clone() else {
            for (_, link) in &mut simple.links {
                *link = Link::Failed(WorkflowFailure::NotFound);
            }
            return;
        };
        let own: Vec<(String, String)> = simple
            .flow
            .as_ref()
            .and_then(|flow| flow.properties.get("connectionReferences"))
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(_, reference)| {
                Some((
                    api_name(reference)?.to_owned(),
                    reference.get("connectionName")?.as_str()?.to_owned(),
                ))
            })
            .collect();
        let mut wanted = Vec::new();
        for (api, link) in &mut simple.links {
            match own.iter().find(|(name, _)| name == api) {
                Some((_, connection)) => *link = Link::Ready(connection.clone()),
                None => wanted.push(api.clone()),
            }
        }
        if wanted.is_empty() {
            self.connections_ready(cx);
            return;
        }
        let generation = self.generation;
        let core = Arc::clone(&self.core);
        self.tasks
            .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let links = connect_all(&core, environment, wanted).await;
                let _ = this.update(cx, |this, cx| {
                    if this.generation != generation {
                        return;
                    }
                    if let Some(simple) = &mut this.simple {
                        simple.set_links(links);
                    }
                    this.connections_ready(cx);
                    cx.notify();
                });
            }));
    }

    /// The connections are known: the connector fields list their choices.
    fn connections_ready(&mut self, cx: &mut Context<Self>) {
        let count = self.simple.as_ref().map_or(0, |simple| simple.params.len());
        for index in 0..count {
            self.load_choices(index, cx);
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
        self.read_message(cx);
        let (properties, flow, from_list) = match self
            .simple
            .as_ref()
            .map(|simple| (simple_properties(simple), simple.flow.clone(), simple.from_list))
        {
            Some((Ok(properties), flow, from_list)) => (properties, flow, from_list),
            Some((Err(error), ..)) => {
                self.error = Some(error.to_owned());
                cx.notify();
                return;
            }
            None => return,
        };
        let request = match flow {
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
        };
        // Teams goes to the Overview after Save, or back to the list when Edit came from the list's menu.
        let outcome = if from_list { Outcome::List } else { Outcome::Overview };
        self.mutate_then(request, outcome, cx);
    }

    /// A message field's text, read into its value.
    pub(super) fn read_message(&mut self, cx: &App) {
        let Some(simple) = &mut self.simple else {
            return;
        };
        for (param, field) in simple.params.iter_mut().zip(&simple.fields) {
            if let Some((editor, _)) = &field.message {
                param.value = super::fields::message_value(editor.read(cx));
            }
        }
    }

    pub(super) fn team_by_group(&self, group: &str) -> Option<&Team> {
        self.teams
            .iter()
            .find(|team| team.group_id.as_deref().unwrap_or(&team.id) == group)
    }

    pub(super) fn leave_simple(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.error = None;
        self.screen = match self.simple.take() {
            Some(Simple {
                flow: Some(flow),
                from_list: false,
                ..
            }) => Screen::Detail(flow),
            Some(Simple { from_list: true, .. }) => self.list_screen(),
            _ => self.start_screen(),
        };
        cx.notify();
    }

    /// The list a change from a row's menu returns to: the Workflows app's Home or its "Your workflows", or the
    /// channel's.
    pub(super) fn list_screen(&self) -> Screen {
        match self.listing {
            Listing::Home => Screen::Home,
            Listing::All => Screen::AllWorkflows,
            Listing::Channel => Screen::Browse,
        }
    }

    pub(super) fn toggle_card(&mut self, cx: &mut Context<Self>) {
        if let Some(simple) = &mut self.simple {
            simple.expanded = !simple.expanded;
        }
        cx.notify();
    }
}

/// The workflow Teams saves: the template's definition with each field's input filled, the markup kept with the
/// choices and their names so Teams can edit it again, and the apps' connections. An edited workflow keeps its name,
/// which a Rename may have changed.
fn simple_properties(simple: &Simple) -> Result<Value, &'static str> {
    if !simple.complete() {
        return Err("Fill in the required fields.");
    }
    let mut definition = simple.definition.clone();
    markup::apply(&mut definition, &simple.params, &simple.bindings);
    definition["description"] = Value::String(simple.description.clone());
    let name = match &simple.flow {
        Some(flow) => flow.name.clone(),
        None => simple.summary(),
    };
    let mut references = serde_json::Map::new();
    for (key, reference) in simple.references.as_object().into_iter().flatten() {
        let api = api_name(reference).ok_or("Open the workflow again.")?;
        let connection = match simple.links.iter().find(|(name, _)| name == api).map(|(_, link)| link) {
            Some(Link::Ready(connection)) => connection.clone(),
            Some(Link::Checking) => return Err("The workflow's apps are still connecting. Try again in a moment."),
            _ => return Err("An app the workflow uses could not be connected. Close Workflows and try again."),
        };
        references.insert(
            key.clone(),
            serde_json::json!({
                "id": format!("/providers/Microsoft.PowerApps/apis/{api}"),
                "connectionName": connection,
                "source": "NotSpecified",
            }),
        );
    }
    let template_references = if simple.flow.is_some() {
        Value::Null
    } else {
        simple.references.clone()
    };
    let kept = markup::filled(&simple.markup, &simple.params, &simple.title, &template_references);
    keep_markup(&mut definition, &kept);
    let mut properties = serde_json::json!({
        "definition": definition,
        "connectionReferences": references,
        "displayName": name,
    });
    if simple.flow.is_none() {
        properties["templateName"] = Value::String(simple.template.clone());
    }
    Ok(properties)
}

/// Keeps the markup in the definition, as Teams does: its `Do_Not_Remove_FlowIL` action runs first, so the
/// template's first actions run after it.
fn keep_markup(definition: &mut Value, markup: &Value) {
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

/// Each app's connection: listed once, then a signed-in one reused or one made and signed in.
async fn connect_all(core: &Arc<dyn Core>, environment: String, apis: Vec<String>) -> Vec<(String, Link)> {
    let listed = core
        .workflow_request(WorkflowRequest::Connections {
            environment: environment.clone(),
        })
        .await
        .unwrap_or(Err(WorkflowFailure::Connection))
        .and_then(|value| model::rows(&value).map(<[Value]>::to_vec));
    let mut links = Vec::new();
    for api in apis {
        let link = match &listed {
            Ok(connections) => connect(core, &environment, &api, connections).await,
            Err(error) => Link::Failed(error.clone()),
        };
        links.push((api, link));
    }
    links
}

/// A signed-in connection of the environment for `api`, made and signed in when there is none. A connection of
/// `api` left unauthenticated (a sign-in that failed before) is signed in again rather than another one made.
async fn connect(core: &Arc<dyn Core>, environment: &str, api: &str, connections: &[Value]) -> Link {
    let name_of = |connection: &Value| connection.get("name").and_then(Value::as_str).map(str::to_owned);
    if let Some(name) = connections
        .iter()
        .find(|connection| connections::is_connected(connection, api))
        .and_then(name_of)
    {
        return Link::Ready(name);
    }
    let prefix = format!("shared-{}-", api.strip_prefix("shared_").unwrap_or(api));
    let connection = connections
        .iter()
        .filter(|connection| {
            connection.pointer("/properties/apiId").and_then(Value::as_str)
                == Some(format!("/providers/Microsoft.PowerApps/apis/{api}").as_str())
        })
        .filter_map(name_of)
        .find(|name| name.starts_with(&prefix))
        .unwrap_or_else(|| {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            format!("{prefix}{stamp:032x}")
        });
    match core
        .workflow_request(WorkflowRequest::AuthorizeConnection {
            environment: environment.to_owned(),
            connector: api.to_owned(),
            connection: connection.clone(),
        })
        .await
        .unwrap_or(Err(WorkflowFailure::Connection))
    {
        Ok(_) => Link::Ready(connection),
        Err(error) => Link::Failed(error),
    }
}
