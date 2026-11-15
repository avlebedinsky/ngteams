//! What the server reads of a workflow's definition: its one webhook trigger and the one place its Teams posts go.
//! A definition the server can't run (another trigger, a post to several places, an action needing another
//! connector) is refused when saved, rather than kept and never run.

use serde_json::{Map, Value};

/// The webhook trigger kinds the client offers "Copy webhook link" for.
const WEBHOOK_KINDS: [&str; 2] = ["TeamsWebhook", "TeamsWebhookV2"];
/// The Teams post operations a webhook workflow runs.
const POSTS: [&str; 2] = ["PostCardToConversation", "PostMessageToConversation"];
const TEAMS_API: &str = "/providers/Microsoft.PowerApps/apis/shared_teams";
/// The actions that need no connector: the template's loop and condition, and Teams' kept markup.
const PLAIN: [&str; 6] = ["Foreach", "If", "Scope", "InitializeVariable", "SetVariable", "Compose"];
/// How deep actions may nest.
const MAX_DEPTH: usize = 8;

/// Where a workflow posts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Target {
    /// A team's channel: its group id and the channel's thread.
    Channel { group: String, channel: String },
    /// A chat's thread.
    Chat(String),
}

impl Target {
    pub(super) fn thread(&self) -> &str {
        match self {
            Self::Channel { channel, .. } => channel,
            Self::Chat(chat) => chat,
        }
    }
}

/// The definition's webhook trigger's name and where it posts, or `None` when the server can't run it.
pub(super) fn read(definition: &Value) -> Option<(String, Target)> {
    let trigger = webhook_trigger(definition)?;
    let parameters = definition.get("parameters").and_then(Value::as_object);
    let mut targets = Vec::new();
    actions(definition.get("actions")?, parameters, 0, &mut targets)?;
    let first = targets.first()?.clone();
    targets
        .iter()
        .all(|target| *target == first)
        .then_some((trigger, first))
}

/// The name of the definition's only trigger, when it is a Teams webhook.
pub(super) fn webhook_trigger(definition: &Value) -> Option<String> {
    let triggers = definition.get("triggers")?.as_object()?;
    let [(name, trigger)] = triggers.iter().collect::<Vec<_>>()[..] else {
        return None;
    };
    (trigger.get("type").and_then(Value::as_str) == Some("Request")
        && trigger
            .get("kind")
            .and_then(Value::as_str)
            .is_some_and(|kind| WEBHOOK_KINDS.contains(&kind)))
    .then(|| name.clone())
}

/// Reads every action of `actions` (nested in loops, conditions and scopes), adding each post's target.
fn actions(
    actions: &Value,
    parameters: Option<&Map<String, Value>>,
    depth: usize,
    targets: &mut Vec<Target>,
) -> Option<()> {
    if depth > MAX_DEPTH {
        return None;
    }
    for action in actions.as_object()?.values() {
        let kind = action.get("type").and_then(Value::as_str)?;
        if kind == "OpenApiConnection" {
            targets.push(post(action.get("inputs")?, parameters)?);
            continue;
        }
        if !PLAIN.contains(&kind) {
            return None;
        }
        if let Some(nested) = action.get("actions") {
            self::actions(nested, parameters, depth + 1, targets)?;
        }
        if let Some(nested) = action.pointer("/else/actions") {
            self::actions(nested, parameters, depth + 1, targets)?;
        }
    }
    Some(())
}

/// The target of a Teams post's inputs.
fn post(inputs: &Value, parameters: Option<&Map<String, Value>>) -> Option<Target> {
    let host = inputs.get("host")?;
    if host.get("apiId").and_then(Value::as_str) != Some(TEAMS_API)
        || !host
            .get("operationId")
            .and_then(Value::as_str)
            .is_some_and(|operation| POSTS.contains(&operation))
    {
        return None;
    }
    let given = inputs.get("parameters")?;
    let value = |key: &str| resolve(given.get(key)?, parameters).filter(|value| !value.is_empty());
    match given.get("location").and_then(Value::as_str)? {
        "Channel" => Some(Target::Channel {
            group: value("body/recipient/groupId")?,
            channel: value("body/recipient/channelId")?,
        }),
        "Group chat" => Some(Target::Chat(value("body/recipient")?)),
        _ => None,
    }
}

/// A literal input, or a workflow parameter's default named by `@parameters('<name>')`.
fn resolve(input: &Value, parameters: Option<&Map<String, Value>>) -> Option<String> {
    let text = input.as_str()?;
    match text
        .strip_prefix("@parameters('")
        .and_then(|rest| rest.strip_suffix("')"))
    {
        Some(name) => parameters?.get(name)?.get("defaultValue")?.as_str().map(str::to_owned),
        None if text.starts_with('@') => None,
        None => Some(text.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn post_in(location: &str, parameters: Value) -> Value {
        let mut parameters = parameters;
        parameters["location"] = json!(location);
        json!({"type": "OpenApiConnection", "inputs": {
            "host": {"apiId": TEAMS_API, "operationId": "PostCardToConversation"},
            "parameters": parameters,
        }})
    }

    fn webhook(actions: &Value) -> Value {
        json!({
            "parameters": {"teams.group": {"defaultValue": "group"}, "teams.channel": {"defaultValue": "19:c@thread.tacv2"}},
            "triggers": {"manual": {"type": "Request", "kind": "TeamsWebhook"}},
            "actions": actions,
        })
    }

    #[test]
    fn a_channel_post_takes_its_ids_from_the_parameters_and_a_chat_post_its_literal() {
        let channel = webhook(
            &json!({"Each": {"type": "Foreach", "actions": {"Post": post_in("Channel", json!({
                "body/recipient/groupId": "@parameters('teams.group')",
                "body/recipient/channelId": "@parameters('teams.channel')",
            }))}}}),
        );
        assert_eq!(
            read(&channel),
            Some((
                "manual".into(),
                Target::Channel {
                    group: "group".into(),
                    channel: "19:c@thread.tacv2".into()
                }
            ))
        );
        let chat = webhook(&json!({"Post": post_in("Group chat", json!({"body/recipient": "19:x@thread.v2"}))}));
        assert_eq!(read(&chat).unwrap().1, Target::Chat("19:x@thread.v2".into()));
        assert_eq!(read(&chat).unwrap().1.thread(), "19:x@thread.v2");
    }

    #[test]
    fn what_the_server_cant_run_reads_as_none() {
        let two = webhook(&json!({
            "A": post_in("Group chat", json!({"body/recipient": "19:x@thread.v2"})),
            "B": post_in("Group chat", json!({"body/recipient": "19:y@thread.v2"})),
        }));
        assert_eq!(read(&two), None);
        let empty = webhook(&json!({"A": post_in("Group chat", json!({"body/recipient": ""}))}));
        assert_eq!(read(&empty), None);
        let expression = webhook(&json!({"A": post_in("Group chat", json!({"body/recipient": "@triggerBody()"}))}));
        assert_eq!(read(&expression), None);
        let bot = webhook(&json!({"A": post_in("Chat with Flow bot", json!({"body/recipient": "a@b"}))}));
        assert_eq!(read(&bot), None);
        let none = webhook(&json!({"A": {"type": "Compose", "inputs": "x"}}));
        assert_eq!(read(&none), None);
        let other = webhook(&json!({"A": {"type": "Http", "inputs": {}}}));
        assert_eq!(read(&other), None);
        let mut schedule = webhook(&json!({"A": post_in("Group chat", json!({"body/recipient": "19:x@thread.v2"}))}));
        schedule["triggers"] = json!({"Recurrence": {"type": "Recurrence"}});
        assert_eq!(read(&schedule), None);
        let mut planner = post_in("Channel", json!({}));
        planner["inputs"]["host"]["apiId"] = json!("/providers/Microsoft.PowerApps/apis/shared_planner");
        assert_eq!(read(&webhook(&json!({"A": planner}))), None);
        let mut deep = json!({"A": post_in("Group chat", json!({"body/recipient": "19:x@thread.v2"}))});
        for _ in 0..=MAX_DEPTH {
            deep = json!({"S": {"type": "Scope", "actions": deep}});
        }
        assert_eq!(read(&webhook(&deep)), None);
        let branch = webhook(&json!({"If": {"type": "If", "actions": {}, "else": {"actions": {
            "A": post_in("Group chat", json!({"body/recipient": "19:x@thread.v2"}))
        }}}}));
        assert_eq!(read(&branch).unwrap().1, Target::Chat("19:x@thread.v2".into()));
    }
}
