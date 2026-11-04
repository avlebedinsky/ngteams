//! A template's own markup (`metadata.userIntentMarkup`), read and filled as Teams' card editor does: its
//! `parameters` are the card's fields (by `typeName`), its `dataBindings` say which trigger or action input each one
//! fills, and its `summary` names the card. The words (labels and titles) are Teams' own: the markup refers to them by
//! resource key, and the Workflows app carries them. See `docs/protocol/workflows.md`.
use chrono::{Datelike as _, Timelike as _};
use serde_json::{Map, Value};

/// What a field is, by the markup's `typeName`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    TeamsGroup,
    TeamsChannel,
    /// The message a channel post carries.
    Message,
    /// A value the template fixes (a recurrence's frequency): never shown.
    Fixed,
    Interval,
    StartDate,
    TimeOfDay,
    TimeZone,
    WeekDays,
    /// A choice from a connector's dynamic list (a plan, a SharePoint site, list or library).
    Choice,
    /// A folder from a connector's folder tree (a SharePoint folder, a mail folder).
    Folder,
    /// People from the directory (a mail's To or From).
    People,
    Text,
}

impl Kind {
    fn of(type_name: &str) -> Self {
        match type_name {
            "Teams.Group" => Self::TeamsGroup,
            "Teams.Channel" => Self::TeamsChannel,
            "Teams.MessageBody" => Self::Message,
            "BuiltIn.Frequency" => Self::Fixed,
            "BuiltIn.Interval" => Self::Interval,
            "BuiltIn.StartDate" => Self::StartDate,
            "BuiltIn.TimeOfDay" => Self::TimeOfDay,
            "BuiltIn.Timezone" => Self::TimeZone,
            "BuiltIn.WeekDays" => Self::WeekDays,
            "Planner.GroupId" | "Planner.PlanId" | "SharePoint.Site" | "SharePoint.List" | "SharePoint.Library" => {
                Self::Choice
            }
            "SharePoint.FolderPath" | "Outlook.MailFolderPath" => Self::Folder,
            "Outlook.ToLine" | "Outlook.From" => Self::People,
            _ => Self::Text,
        }
    }
}

/// One of the card's fields: the markup's parameter and the user's value.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Param {
    pub id: String,
    pub type_name: String,
    pub kind: Kind,
    pub label: String,
    pub optional: bool,
    pub dependencies: Vec<String>,
    /// The value as the markup keeps it (`Null` when none).
    pub value: Value,
    /// Its name as shown (`valueDisplayName`).
    pub display: Option<String>,
}

impl Param {
    /// Whether the field holds nothing.
    pub fn empty(&self) -> bool {
        match &self.value {
            Value::Null => true,
            Value::String(text) => text.trim().is_empty(),
            Value::Array(items) => items.is_empty(),
            _ => false,
        }
    }

    pub fn text(&self) -> Option<&str> {
        self.value.as_str()
    }
}

/// The input a parameter fills: an action or trigger by name, and the path in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Binding {
    pub node: String,
    pub param: String,
    pub path: Vec<String>,
}

/// The template's fields, in its `editorBindings` order (then any other parameter), with the values the markup holds.
pub(super) fn params(markup: &Value) -> Vec<Param> {
    let template = markup.pointer("/automation/template").unwrap_or(&Value::Null);
    let id = template.get("id").and_then(Value::as_str).unwrap_or_default();
    let all: Vec<&Value> = template
        .get("parameters")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect();
    let editors: Vec<&Value> = template
        .get("editorBindings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect();
    let mut ordered: Vec<&Value> = editors
        .iter()
        .filter_map(|binding| {
            let wanted = binding.get("parameterId")?;
            all.iter()
                .find(|parameter| parameter.get("id") == Some(wanted))
                .copied()
        })
        .collect();
    for parameter in &all {
        if !ordered.iter().any(|known| known.get("id") == parameter.get("id")) {
            ordered.push(parameter);
        }
    }
    ordered
        .into_iter()
        .filter_map(|parameter| {
            let param_id = parameter.get("id")?.as_str()?.to_owned();
            let type_name = parameter.get("typeName")?.as_str()?.to_owned();
            let labelled = editors.iter().any(|binding| {
                binding.get("parameterId").and_then(Value::as_str) == Some(param_id.as_str())
                    && binding.get("label").is_some()
            });
            let kind = Kind::of(&type_name);
            Some(Param {
                label: label(id, &param_id, &type_name, labelled),
                kind,
                optional: parameter.get("optional").and_then(Value::as_bool) == Some(true),
                dependencies: parameter
                    .get("dependencies")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
                value: parameter.get("value").cloned().unwrap_or(Value::Null),
                display: parameter
                    .get("valueDisplayName")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned),
                id: param_id,
                type_name,
            })
        })
        .collect()
}

/// Which input of which action or trigger each parameter fills, from the markup's `dataBindings`.
pub(super) fn bindings(markup: &Value) -> Vec<Binding> {
    let mut found = Vec::new();
    for (node, list) in markup
        .get("dataBindings")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        for binding in list.as_array().into_iter().flatten() {
            let source = binding.get("source").and_then(Value::as_str).and_then(source_param);
            let path = binding.get("target").and_then(Value::as_str).map(target_path);
            if let (Some(param), Some(path)) = (source, path)
                && !path.is_empty()
            {
                found.push(Binding {
                    node: node.clone(),
                    param,
                    path,
                });
            }
        }
    }
    found
}

/// `${context['parameters']['X']}` names the parameter `X`.
fn source_param(source: &str) -> Option<String> {
    let inner = source.strip_prefix("${context['parameters']['")?.strip_suffix("']}")?;
    (!inner.is_empty() && !inner.contains('\'')).then(|| inner.to_owned())
}

/// `${json['inputs']['parameters']['body/x']}` is the path `inputs`, `parameters`, `body/x`.
fn target_path(target: &str) -> Vec<String> {
    let Some(inner) = target.strip_prefix("${json").and_then(|rest| rest.strip_suffix('}')) else {
        return Vec::new();
    };
    inner
        .split("']")
        .filter_map(|piece| piece.strip_prefix("['"))
        .map(str::to_owned)
        .collect()
}

/// The action or trigger named `name`, wherever the definition nests it (in a loop's, a branch's or a case's
/// actions too).
pub(super) fn node_mut<'a>(definition: &'a mut Value, name: &str) -> Option<&'a mut Value> {
    let pointer = pointer_to(definition, name)?;
    definition.pointer_mut(&pointer)
}

/// The action or trigger named `name`, read.
pub(super) fn node<'a>(definition: &'a Value, name: &str) -> Option<&'a Value> {
    definition.pointer(&pointer_to(definition, name)?)
}

fn pointer_to(definition: &Value, name: &str) -> Option<String> {
    if definition.pointer(&format!("/triggers/{}", escape(name))).is_some() {
        return Some(format!("/triggers/{}", escape(name)));
    }
    in_actions(definition.get("actions")?, name, "/actions")
}

fn in_actions(actions: &Value, name: &str, at: &str) -> Option<String> {
    let actions = actions.as_object()?;
    if actions.contains_key(name) {
        return Some(format!("{at}/{}", escape(name)));
    }
    actions.iter().find_map(|(key, nested)| {
        let here = format!("{at}/{}", escape(key));
        nested
            .get("actions")
            .and_then(|inner| in_actions(inner, name, &format!("{here}/actions")))
            .or_else(|| {
                nested
                    .pointer("/else/actions")
                    .and_then(|inner| in_actions(inner, name, &format!("{here}/else/actions")))
            })
            .or_else(|| in_cases(nested, name, &here))
    })
}

/// A switch's cases' actions.
fn in_cases(switch: &Value, name: &str, at: &str) -> Option<String> {
    switch.get("cases")?.as_object()?.iter().find_map(|(case, body)| {
        let inner = body.get("actions")?;
        in_actions(inner, name, &format!("{at}/cases/{}/actions", escape(case)))
    })
}

fn escape(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

/// Fills the definition's inputs from the fields, as Teams' Save does: an optional field left empty takes its input
/// out, a time of day fills the recurrence's hours and minutes, days its week days, and people their addresses.
pub(super) fn apply(definition: &mut Value, params: &[Param], bindings: &[Binding]) {
    for binding in bindings {
        let Some(param) = params.iter().find(|param| param.id == binding.param) else {
            continue;
        };
        let Some(node) = node_mut(definition, &binding.node) else {
            continue;
        };
        let (last, parents) = match binding.path.split_last() {
            Some((last, parents)) => (last.clone(), parents),
            None => continue,
        };
        let mut at = &mut *node;
        for key in parents {
            if !at.get(key).is_some_and(Value::is_object) {
                at[key.as_str()] = Value::Object(Map::new());
            }
            at = &mut at[key.as_str()];
        }
        match (param.kind, last.as_str()) {
            (Kind::TimeOfDay, "schedule") => {
                if let Some((hour, minute)) = param.text().and_then(parse_clock) {
                    at["schedule"]["hours"] = Value::from(vec![hour]);
                    at["schedule"]["minutes"] = Value::from(vec![minute]);
                }
            }
            (Kind::WeekDays, "schedule") => at["schedule"]["weekDays"] = param.value.clone(),
            _ if param.empty() => {
                if let Some(object) = at.as_object_mut() {
                    object.remove(&last);
                }
            }
            (Kind::People, _) => at[last.as_str()] = Value::String(addresses(&param.value)),
            _ => at[last.as_str()] = param.value.clone(),
        }
    }
}

/// People as a connector takes them: their addresses, separated by `;`.
fn addresses(value: &Value) -> String {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|person| person.get("email").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(";")
}

/// The markup kept with the workflow: each field's value and shown name, the template's name and its connections
/// (unnamed, as Teams keeps them).
pub(super) fn filled(markup: &Value, params: &[Param], name: &str, references: &Value) -> Value {
    let mut markup = markup.clone();
    if let Some(list) = markup
        .pointer_mut("/automation/template/parameters")
        .and_then(Value::as_array_mut)
    {
        for parameter in list {
            let Some(param) = params
                .iter()
                .find(|param| parameter.get("id").and_then(Value::as_str) == Some(param.id.as_str()))
            else {
                continue;
            };
            let Some(object) = parameter.as_object_mut() else {
                continue;
            };
            if param.empty() {
                object.remove("value");
                object.remove("valueDisplayName");
                continue;
            }
            object.insert("value".to_owned(), param.value.clone());
            object.insert(
                "valueDisplayName".to_owned(),
                Value::String(param.display.clone().unwrap_or_default()),
            );
        }
    }
    if let Some(template) = markup.pointer_mut("/automation/template") {
        template["displayName"] = Value::String(name.to_owned());
        if let Some(references) = references.as_object() {
            template["connectionReferences"] = Value::Object(
                references
                    .iter()
                    .map(|(key, reference)| {
                        let mut kept = reference.clone();
                        kept["source"] = Value::String("NotSpecified".to_owned());
                        kept["connectionNamingNode"] = Value::String("NotSpecified".to_owned());
                        (key.clone(), kept)
                    })
                    .collect(),
            );
        }
    }
    markup
}

/// Teams' label of a field: the template's own words where its markup names a label, else the field kind's.
fn label(template: &str, param: &str, type_name: &str, labelled: bool) -> String {
    let own = match (template, param) {
        ("805739fdad754e799502b6379900922e", "BuiltInInterval") => Some("Repeat every (weeks)"),
        ("805739fdad754e799502b6379900922e", "TeamsGroup") => Some("Select team"),
        ("805739fdad754e799502b6379900922e", "TeamsChannel") => Some("Select channel"),
        ("805739fdad754e799502b6379900922e", "TeamsMessageBody") => Some("Message content"),
        ("8f348e6a1ad64fe8bcb974b02585d987", "PlannerGroupId") => Some("Select a group that has an existing plan"),
        ("8f348e6a1ad64fe8bcb974b02585d987", "PlannerPlanId") => Some("Plan to monitor"),
        ("8f348e6a1ad64fe8bcb974b02585d987", "TeamsGroup") => Some("Team to notify"),
        ("8f348e6a1ad64fe8bcb974b02585d987", "TeamsChannel") => Some("Channel to notify"),
        ("511d104411f84d659018aed7d68a4c22", "SharePointSite") => Some("SharePoint site the list or library is in"),
        ("511d104411f84d659018aed7d68a4c22", "SharePointList") => Some("SharePoint list or library"),
        ("190c5a56070e4a8b802089be6455553e", "OutlookMailFolderPath") => Some("Folder (if empty, defaults to inbox)"),
        ("190c5a56070e4a8b802089be6455553e", "OutlookToLine") => {
            Some("Forward emails sent to any of these email addresses")
        }
        ("190c5a56070e4a8b802089be6455553e", "OutlookFrom") => {
            Some("Forward emails sent from any of these email addresses")
        }
        ("190c5a56070e4a8b802089be6455553e", "OutlookSubjectFilter") => Some("Subject contains this word or phrase"),
        ("190c5a56070e4a8b802089be6455553e", "TeamsGroup") => Some("Team"),
        ("b35fbd284547427cbb4e289d98dd63b4", "SharePointSite") => Some("SharePoint site the library is in"),
        _ => None,
    };
    if let Some(own) = own {
        return own.to_owned();
    }
    match type_name {
        // Teams' webhook and SharePoint templates label their team this way.
        "Teams.Group" if labelled => "Team the channel is in",
        "Teams.Group" => "Team",
        "Teams.Channel" => "Channel",
        "Teams.MessageBody" => "Message content",
        "BuiltIn.Interval" => "Repeat every (weeks)",
        "BuiltIn.StartDate" => "Start date",
        "BuiltIn.TimeOfDay" => "Time",
        "BuiltIn.Timezone" => "Time zone",
        "BuiltIn.WeekDays" => "Day",
        "Planner.GroupId" => "Group",
        "Planner.PlanId" => "Plan",
        "SharePoint.Site" => "SharePoint site",
        "SharePoint.List" => "SharePoint list or library",
        "SharePoint.Library" => "SharePoint library",
        "SharePoint.FolderPath" => "SharePoint folder",
        "Outlook.MailFolderPath" => "Folder",
        "Outlook.ToLine" => "To",
        "Outlook.From" => "From",
        "Outlook.SubjectFilter" => "Subject contains this word or phrase",
        _ => param,
    }
    .to_owned()
}

/// The card's title as Teams words it, from the fields as they are now. With nothing chosen a name reads as its kind
/// ("channel", "plan", "list"), as in Teams.
pub(super) fn summary(template: &str, name: &str, params: &[Param]) -> String {
    let shown = |kind: Kind, fallback: &str| {
        params
            .iter()
            .find(|param| param.kind == kind && !param.empty())
            .and_then(|param| param.display.clone())
            .unwrap_or_else(|| fallback.to_owned())
    };
    let named = |id: &str, fallback: &str| {
        params
            .iter()
            .find(|param| param.id == id && !param.empty())
            .and_then(|param| param.display.clone())
            .unwrap_or_else(|| fallback.to_owned())
    };
    let channel = shown(Kind::TeamsChannel, "channel");
    match template {
        "805739fdad754e799502b6379900922e" => {
            let interval = params
                .iter()
                .find(|param| param.kind == Kind::Interval)
                .and_then(|param| interval(&param.value))
                .map_or_else(|| "few".to_owned(), |weeks| weeks.to_string());
            let days = params
                .iter()
                .find(|param| param.kind == Kind::WeekDays)
                .and_then(|param| param.value.as_array())
                .map_or_else(Vec::new, |days| days.iter().filter_map(Value::as_str).collect());
            let days = match days.as_slice() {
                [one] => (*one).to_owned(),
                _ => "selected days".to_owned(),
            };
            let time = shown(Kind::TimeOfDay, "time");
            format!("Send a message to {channel} every {interval} week(s) on {days} at {time}")
        }
        "8f348e6a1ad64fe8bcb974b02585d987" => format!(
            "Notify {channel} when a Planner task is completed in {}",
            named("PlannerPlanId", "plan")
        ),
        "511d104411f84d659018aed7d68a4c22" => format!(
            "Notify {channel} when files or list items are changed in {}",
            named("SharePointList", "list")
        ),
        "190c5a56070e4a8b802089be6455553e" => {
            format!("Forward the message to {channel} when the email meets your selected conditions")
        }
        "b35fbd284547427cbb4e289d98dd63b4" => format!(
            "Notify {channel} when a new file is uploaded or created within {}",
            named("SharePointLibrary", "library")
        ),
        // A template "to a channel" names the chosen one: "Send webhook alerts to General".
        _ if name.contains(" a channel") => name.replacen(" a channel", &format!(" {channel}"), 1),
        _ => name.to_owned(),
    }
}

/// A recurrence's interval in weeks, as Teams accepts it: a whole number from 1 to 71.
pub(super) fn interval(value: &Value) -> Option<u32> {
    let weeks = match value {
        Value::Number(number) => number.as_u64().and_then(|weeks| u32::try_from(weeks).ok()),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }?;
    (1..=71).contains(&weeks).then_some(weeks)
}

/// `20:05` (or `8:05 PM`) as hours and minutes.
pub(super) fn parse_clock(text: &str) -> Option<(u32, u32)> {
    let text = text.trim().to_ascii_uppercase();
    let (clock, pm) = if let Some(clock) = text.strip_suffix("PM") {
        (clock.trim(), Some(true))
    } else if let Some(clock) = text.strip_suffix("AM") {
        (clock.trim(), Some(false))
    } else {
        (text.as_str(), None)
    };
    let (hour, minute) = clock.split_once(':')?;
    let (mut hour, minute): (u32, u32) = (hour.trim().parse().ok()?, minute.trim().parse().ok()?);
    if let Some(pm) = pm {
        if !(1..=12).contains(&hour) {
            return None;
        }
        hour = match (hour, pm) {
            (12, false) => 0,
            (12, true) => 12,
            (hour, true) => hour + 12,
            (hour, false) => hour,
        };
    }
    (hour < 24 && minute < 60).then_some((hour, minute))
}

/// `8:05 PM`, as Teams shows a time.
pub(super) fn clock_name(hour: u32, minute: u32) -> String {
    let (shown, half) = match hour {
        0 => (12, "AM"),
        1..=11 => (hour, "AM"),
        12 => (12, "PM"),
        _ => (hour - 12, "PM"),
    };
    format!("{shown}:{minute:02} {half}")
}

/// The time Teams offers by default: the next five-minute mark from now.
pub(super) fn default_time(now: chrono::NaiveDateTime) -> (u32, u32) {
    let minutes = now.hour() * 60 + now.minute();
    let next = (minutes / 5 + 1) * 5 % (24 * 60);
    (next / 60, next % 60)
}

/// A start date as the recurrence takes it (`2026-11-04T00:00:00`) and as Teams shows it (`11/4/2026`).
pub(super) fn date_value(date: chrono::NaiveDate) -> (String, String) {
    (
        format!("{}T00:00:00", date.format("%Y-%m-%d")),
        format!("{}/{}/{}", date.month(), date.day(), date.year()),
    )
}

/// The date a recurrence's start time names.
pub(super) fn parse_date(value: &str) -> Option<chrono::NaiveDate> {
    chrono::NaiveDate::parse_from_str(value.get(..10)?, "%Y-%m-%d").ok()
}

/// Whether a workflow's trigger is a recurrence Teams offers to run now.
pub(super) fn recurrence_trigger(definition: &Value) -> Option<String> {
    definition
        .get("triggers")?
        .as_object()?
        .iter()
        .find(|(_, trigger)| trigger.get("type").and_then(Value::as_str) == Some("Recurrence"))
        .map(|(name, _)| name.clone())
}

#[cfg(test)]
#[path = "../../tests/support/workflow_markup_tests.rs"]
mod tests;
