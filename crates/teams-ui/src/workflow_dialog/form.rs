//! Forms come from the service's actual definition. Literal connector inputs and recurrence settings are editable;
//! expressions and unknown nodes survive. No token, callback URL or connector authentication is evaluated here.
use super::*;

pub(super) struct ConfigField {
    pointer: String,
    label: String,
    kind: Value,
    input: Entity<InputState>,
    shown: String,
    html: bool,
}

#[derive(Default)]
pub(super) enum EditorMode {
    #[default]
    Form,
    Definition,
}

impl WorkflowDialog {
    pub(super) fn prepare_fields(&mut self, properties: &mut Value, window: &mut Window, cx: &mut Context<Self>) {
        let mut literals = Vec::new();
        if let Some(parameters) = properties.pointer("/definition/parameters").and_then(Value::as_object) {
            for (key, value) in parameters
                .iter()
                .filter(|(key, _)| !key.starts_with('$') && !matches!(key.as_str(), "teams.group" | "teams.channel"))
            {
                if let Some(value) = value.get("defaultValue") {
                    literals.push((
                        format!("/definition/parameters/{}/defaultValue", escape(key)),
                        title(key),
                        value.clone(),
                    ));
                }
            }
        }
        collect(
            properties.get("definition").unwrap_or(&Value::Null),
            "/definition",
            "",
            &mut literals,
        );
        self.fields = literals
            .into_iter()
            .map(|(pointer, label, kind)| {
                let shown = match &kind {
                    Value::String(value) => value.clone(),
                    Value::Array(values) => values.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "),
                    other => other.to_string(),
                };
                let html = pointer.ends_with("/inputs/parameters/body~1messageBody");
                let shown = if html {
                    teams_proto::chat::parse_html(&shown).plain_text()
                } else {
                    shown
                };
                let input = cx.new(|cx| InputState::new(window, cx).default_value(shown.clone()));
                ConfigField {
                    pointer,
                    label,
                    kind,
                    input,
                    shown,
                    html,
                }
            })
            .collect();
        self.editor_mode = EditorMode::Form;
        self.draft = properties.clone();
    }

    pub(super) fn configured(&self, cx: &Context<Self>) -> Result<Value, &'static str> {
        if matches!(self.editor_mode, EditorMode::Definition) {
            return serde_json::from_str(self.definition.read(cx).value().as_ref())
                .map_err(|_| "The workflow definition is not valid JSON.");
        }
        let mut properties = self.draft.clone();
        for field in &self.fields {
            let text = field.input.read(cx).value();
            if text.as_ref() == field.shown {
                continue;
            }
            let value = match field.kind {
                Value::String(_) if field.html => Value::String(
                    teams_proto::chat::MessageText::plain(&text)
                        .map_err(|_| "Enter a message for this channel.")?
                        .html()
                        .to_owned(),
                ),
                Value::String(_) => Value::String(text.to_string()),
                Value::Array(_) => Value::Array(
                    text.split(',')
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(|value| Value::String(value.to_owned()))
                        .collect(),
                ),
                _ => serde_json::from_str(text.as_ref()).map_err(|_| "A number or true/false setting is invalid.")?,
            };
            let destination = properties
                .pointer_mut(&field.pointer)
                .ok_or("The workflow definition changed. Reopen it before saving.")?;
            *destination = value;
        }
        Ok(properties)
    }

    pub(super) fn config_fields(&self, palette: Palette) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .children(self.fields.iter().enumerate().map(|(index, field)| {
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(palette.muted_foreground))
                            .child(field.label.clone()),
                    )
                    .child(selected_by(
                        format!("workflow-config-{index}"),
                        Input::new(&field.input),
                    ))
            }))
            .into_any_element()
    }
}

fn collect(value: &Value, pointer: &str, section: &str, fields: &mut Vec<(String, String, Value)>) {
    let Some(object) = value.as_object() else {
        return;
    };
    if let Some(parameters) = value.pointer("/inputs/parameters").and_then(Value::as_object) {
        for (key, value) in parameters {
            if literal(value)
                && !matches!(
                    key.as_str(),
                    "poster" | "location" | "body/recipient/groupId" | "body/recipient/channelId"
                )
            {
                fields.push((
                    format!("{pointer}/inputs/parameters/{}", escape(key)),
                    format!("{section} · {}", title(key)),
                    value.clone(),
                ));
            }
        }
    }
    collect_recurrence(value, pointer, section, fields);
    for key in ["triggerAuthenticationType", "triggerAllowedUsers"] {
        if let Some(value) = value.pointer(&format!("/inputs/{key}")) {
            fields.push((format!("{pointer}/inputs/{key}"), title(key), value.clone()));
        }
    }
    for block in ["triggers", "actions"] {
        if let Some(nodes) = object.get(block).and_then(Value::as_object) {
            for (key, node) in nodes {
                collect(
                    node,
                    &format!("{pointer}/{block}/{}", escape(key)),
                    &key.replace('_', " "),
                    fields,
                );
            }
        }
    }
    if let Some(otherwise) = object.get("else") {
        collect(otherwise, &format!("{pointer}/else"), section, fields);
    }
}

fn collect_recurrence(value: &Value, pointer: &str, section: &str, fields: &mut Vec<(String, String, Value)>) {
    let Some(recurrence) = value.get("recurrence").and_then(Value::as_object) else {
        return;
    };
    for (key, value) in recurrence {
        if literal(value) {
            fields.push((
                format!("{pointer}/recurrence/{}", escape(key)),
                format!("{section} · {}", title(key)),
                value.clone(),
            ));
        }
    }
    let Some(schedule) = recurrence.get("schedule").and_then(Value::as_object) else {
        return;
    };
    for (key, value) in schedule.iter().filter(|(_, value)| literal(value)) {
        fields.push((
            format!("{pointer}/recurrence/schedule/{}", escape(key)),
            format!("{section} · {}", title(key)),
            value.clone(),
        ));
    }
}

fn literal(value: &Value) -> bool {
    match value {
        Value::String(text) => !text.starts_with('@') && !text.contains("@{"),
        Value::Bool(_) | Value::Number(_) => true,
        Value::Array(values) => values
            .iter()
            .all(|value| value.as_str().is_some_and(|text| !text.starts_with('@'))),
        _ => false,
    }
}

pub(super) fn apply_channel(properties: &mut Value, group: &str, channel: &str) {
    for (name, value) in [("teams.group", group), ("teams.channel", channel)] {
        if let Some(default) = properties.pointer_mut(&format!("/definition/parameters/{name}/defaultValue")) {
            *default = Value::String(value.to_owned());
        }
    }
    fill_recipient(properties.get_mut("definition"), group, channel);
}
fn fill_recipient(value: Option<&mut Value>, group: &str, channel: &str) {
    let Some(value) = value else {
        return;
    };
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                fill_literal(key, value, group, channel);
                fill_recipient(Some(value), group, channel);
            }
        }
        Value::Array(values) => {
            for value in values {
                fill_recipient(Some(value), group, channel);
            }
        }
        _ => {}
    }
}
fn fill_literal(key: &str, value: &mut Value, group: &str, channel: &str) {
    let Value::String(text) = value else {
        return;
    };
    if text.starts_with('@') {
        return;
    }
    match key {
        "body/recipient/groupId" => group.clone_into(text),
        "body/recipient/channelId" => channel.clone_into(text),
        _ => {}
    }
}
fn escape(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}
fn title(key: &str) -> String {
    match key {
        "body/messageBody" => "Message".to_owned(),
        "dataset" => "SharePoint site".to_owned(),
        "table" => "Library or list".to_owned(),
        "folderPath" => "Folder".to_owned(),
        "groupId" => "Planner group".to_owned(),
        "id" => "Plan".to_owned(),
        "timeZone" => "Time zone".to_owned(),
        "startTime" => "Start time".to_owned(),
        "weekDays" => "Days of the week".to_owned(),
        other => {
            let words = other.replace(['/', '_', '.'], " ");
            let mut chars = words.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        }
    }
}
