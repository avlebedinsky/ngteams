//! Workflows with incoming webhooks: the real own-cloud engine's Power Automate requests (`WorkflowRequest`) against
//! the server's Flow and Power Apps routes, and plain HTTP posts to a workflow's webhook link.

mod flows;
mod gallery;
mod hooks;
mod nodes;

use serde_json::{Value, json};
use teams_proto::{WorkflowFailure, WorkflowRequest};

pub(crate) use crate::teams::{ORIGIN, Seat, server};

/// Teams' "Send webhook alerts to a channel" and "… to a chat" templates, by their gallery ids.
pub(crate) const CHANNEL_TEMPLATE: &str = "d271a6f01c2545a28348d8f2cddf4c8f";
pub(crate) const CHAT_TEMPLATE: &str = "34ac6c11796143e1807d6e16c4c28050";

/// The engine's answer to `request`.
pub(crate) async fn ask(seat: &Seat, request: WorkflowRequest) -> Result<Value, WorkflowFailure> {
    seat.engine.workflow_request(request).await.unwrap()
}

/// The answer's `value` list.
pub(crate) fn rows(answer: &Value) -> &[Value] {
    answer["value"].as_array().map_or(&[], Vec::as_slice)
}

/// The account's one environment, as the Workflows dialog takes it.
pub(crate) async fn environment(seat: &Seat) -> String {
    let answer = ask(seat, WorkflowRequest::Environments).await.unwrap();
    rows(&answer)[0]["name"].as_str().unwrap().to_owned()
}

/// The account's Microsoft Teams connection, made the way the card editor makes it when the template opens: an
/// existing connected one, else a new one signed in behind the scenes.
pub(crate) async fn teams_connection(seat: &Seat, environment: &str) -> String {
    let listed = ask(
        seat,
        WorkflowRequest::Connections {
            environment: environment.to_owned(),
        },
    )
    .await
    .unwrap();
    if let Some(found) = rows(&listed).iter().find(|connection| {
        connection["properties"]["apiId"] == "/providers/Microsoft.PowerApps/apis/shared_teams"
            && connection["properties"]["statuses"][0]["status"] == "Connected"
    }) {
        return found["name"].as_str().unwrap().to_owned();
    }
    let name = format!("shared-teams-{}", uuid::Uuid::new_v4().simple());
    let made = ask(
        seat,
        WorkflowRequest::AuthorizeConnection {
            environment: environment.to_owned(),
            connector: "shared_teams".into(),
            connection: name.clone(),
        },
    )
    .await
    .unwrap();
    assert_eq!(made["name"], name.as_str());
    name
}

/// Where a webhook workflow posts.
pub(crate) enum Target<'a> {
    Channel { group: &'a str, channel: &'a str },
    Chat(&'a str),
}

/// The card editor's Save body for the webhook template `template` (read from the gallery) posting to `target`:
/// its definition with the recipient filled, Teams' `Do_Not_Remove_FlowIL` keeping the markup with the chosen values,
/// and the account's Teams connection.
pub(crate) fn save_body(template: &Value, target: &Target<'_>, connection: &str) -> Value {
    let properties = &template["properties"];
    let mut definition = properties["definition"].clone();
    let mut markup = properties["metadata"]["userIntentMarkup"].clone();
    let name = match target {
        Target::Channel { group, channel } => {
            definition["parameters"]["teams.group"]["defaultValue"] = json!(group);
            definition["parameters"]["teams.channel"]["defaultValue"] = json!(channel);
            let parameters = &mut markup["automation"]["template"]["parameters"];
            parameters[0]["value"] = json!(group);
            parameters[0]["valueDisplayName"] = json!("Rocket");
            parameters[1]["value"] = json!(channel);
            parameters[1]["valueDisplayName"] = json!("General");
            "Send webhook alerts to General"
        }
        Target::Chat(chat) => {
            for (_, action) in posts(&mut definition) {
                action["inputs"]["parameters"]["body/recipient"] = json!(chat);
            }
            let parameters = &mut markup["automation"]["template"]["parameters"];
            parameters[0]["value"] = json!(chat);
            parameters[0]["valueDisplayName"] = json!("Project room");
            "Send webhook alerts to Project room"
        }
    };
    markup["automation"]["template"]["displayName"] = properties["displayName"].clone();
    let first: Vec<String> = definition["actions"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(_, action)| action["runAfter"].as_object().is_some_and(serde_json::Map::is_empty))
        .map(|(key, _)| key.clone())
        .collect();
    for key in first {
        definition["actions"][&key]["runAfter"] = json!({"Do_Not_Remove_FlowIL": ["Succeeded"]});
    }
    definition["actions"]["Do_Not_Remove_FlowIL"] = json!({
        "type": "InitializeVariable", "runAfter": {},
        "inputs": {"variables": [{"name": "flow.IL (DO NOT REMOVE)", "type": "string", "value": "Teams"}]},
        "metadata": {"userIntentMarkup": markup.to_string()},
    });
    definition["description"] = properties["description"].clone();
    json!({
        "displayName": name,
        "templateName": template["name"],
        "definition": definition,
        "connectionReferences": {"shared_teams": {
            "id": "/providers/Microsoft.PowerApps/apis/shared_teams",
            "connectionName": connection,
            "source": "NotSpecified",
        }},
    })
}

/// Every Teams post action of a definition, nested in conditions and loops too.
fn posts(definition: &mut Value) -> Vec<(String, &mut Value)> {
    fn walk<'a>(actions: &'a mut Value, found: &mut Vec<(String, &'a mut Value)>) {
        let Some(actions) = actions.as_object_mut() else {
            return;
        };
        for (name, action) in actions {
            if action["inputs"]["host"]["operationId"] == "PostCardToConversation" {
                found.push((name.clone(), action));
                continue;
            }
            let action = action.as_object_mut().unwrap();
            for (key, value) in action.iter_mut() {
                match key.as_str() {
                    "actions" => walk(value, found),
                    "else" => walk(&mut value["actions"], found),
                    _ => {}
                }
            }
        }
    }
    let mut found = Vec::new();
    walk(&mut definition["actions"], &mut found);
    found
}

/// The template `id` as the engine reads it.
pub(crate) async fn template(seat: &Seat, id: &str) -> Value {
    ask(seat, WorkflowRequest::Template { id: id.to_owned() })
        .await
        .unwrap()
}

/// Saves a webhook workflow posting to `target` through the engine; the created workflow.
pub(crate) async fn save(seat: &Seat, environment: &str, target: &Target<'_>) -> Value {
    let id = match target {
        Target::Channel { .. } => CHANNEL_TEMPLATE,
        Target::Chat(_) => CHAT_TEMPLATE,
    };
    let template = template(seat, id).await;
    let connection = teams_connection(seat, environment).await;
    ask(
        seat,
        WorkflowRequest::Create {
            environment: environment.to_owned(),
            properties: save_body(&template, target, &connection),
        },
    )
    .await
    .unwrap()
}

/// "Copy webhook link": the workflow's webhook address, read as the Overview reads it.
pub(crate) async fn webhook_link(seat: &Seat, environment: &str, flow: &str) -> String {
    let answer = ask(
        seat,
        WorkflowRequest::WebhookAddress {
            environment: environment.to_owned(),
            flow: flow.to_owned(),
            trigger: "manual".into(),
        },
    )
    .await
    .unwrap();
    answer
        .get("value")
        .or_else(|| answer.pointer("/response/value"))
        .and_then(Value::as_str)
        .filter(|link| link.starts_with("https://"))
        .unwrap()
        .to_owned()
}
