//! Synthetic workflow data for native view tests; no tenant identities or credentials.
use serde_json::{Value, json};
use teams_proto::{WorkflowRequest, WorkflowResponse};
#[expect(
    clippy::unnecessary_wraps,
    reason = "the fake implements the service result callback and other callbacks return errors"
)]
pub fn answer(request: &WorkflowRequest) -> WorkflowResponse {
    Ok(match request {
        WorkflowRequest::Catalog => json!({"value": [
            {"id":"synthetic-template","icons":["https://static.powerapps.com/synthetic/teams/icon.png"],"name":"Automate weekly reminders and updates in a conversation","description":"Get weekly reminders and status updates automatically so nothing gets missed"},
            {"id":"synthetic-planner","icons":["https://static.powerapps.com/synthetic/teams/icon.png","https://static.powerapps.com/synthetic/planner/icon.png"],"name":"Notify a channel when Planner tasks are completed","description":"Celebrate progress by notifying a channel when a Planner task is marked as done"},
            {"id":"synthetic-sharepoint","icons":["https://static.powerapps.com/synthetic/teams/icon.png","https://static.powerapps.com/synthetic/sharepointonline/icon.png"],"name":"Notify a channel when SharePoint file or items change","description":"Let everyone in the channel know whenever a SharePoint file or list item changes"},
            {"id":"synthetic-email","icons":["https://static.powerapps.com/synthetic/teams/icon.png","https://static.powerapps.com/synthetic/office365/icon.png"],"name":"Forward your emails to a channel","description":"Automatically forward important emails to a channel to keep everyone in the loop"},
            {"id":"synthetic-files","icons":["https://static.powerapps.com/synthetic/teams/icon.png","https://static.powerapps.com/synthetic/sharepointonline/icon.png"],"name":"Notify a channel when new SharePoint files are added","description":"Keep everyone informed so they never miss a file update"},
            {"id":"synthetic-webhook","icons":["https://static.powerapps.com/synthetic/teams/icon.png"],"name":"Send webhook alerts to a channel","description":"Get updates in Teams from tools compatible with incoming webhooks"}
        ]}),
        WorkflowRequest::CopilotEligibility => {
            json!({"userCopilotSettings":{"value":{"isCopilotAllowedByAdmin":true,"eligibility":{"isCopilotEligible":false}}}})
        }
        WorkflowRequest::Environments => {
            json!({"value": [{"name":"Default-synthetic", "properties":{"displayName":"Synthetic environment"}}]})
        }
        WorkflowRequest::Connections { .. } => json!({"value": []}),
        WorkflowRequest::List { .. } => json!({"value": [{"name":"synthetic-flow"}]}),
        WorkflowRequest::Get { .. } | WorkflowRequest::Template { .. } => flow(),
        WorkflowRequest::RunActions { .. } => json!({"value":[{"name":"post","properties":{"status":"Succeeded"}}]}),
        WorkflowRequest::History { .. } => {
            json!({"value": [{"name":"synthetic-run", "properties":{"status":"Succeeded","startTime":"2026-01-01T09:00:00Z","endTime":"2026-01-01T09:00:01Z"}}]})
        }
        _ => Value::Null,
    })
}
pub fn flow() -> Value {
    json!({"name":"synthetic-flow","properties":{"displayName":"Weekly channel reminder","description":"A synthetic workflow","state":"Started","definition":{"parameters":{"message":{"type":"String","defaultValue":"Weekly reminder"},"teams.group":{"type":"String","defaultValue":"synthetic-group"},"teams.channel":{"type":"String","defaultValue":"synthetic-channel"}},"triggers":{"manual":{"type":"Request","kind":"Button"}},"actions":{"post":{"type":"OpenApiConnection","inputs":{"host":{"apiId":"/providers/Microsoft.PowerApps/apis/shared_teams","connectionName":"shared_teams","operationId":"PostMessageToConversation"},"parameters":{"location":"Channel","body/recipient/groupId":"@parameters('teams.group')","body/recipient/channelId":"@parameters('teams.channel')","body/messageBody":"<p>Weekly reminder</p>"}}}}},"connectionReferences":{}}})
}

pub fn input_answer(request: &WorkflowRequest) -> WorkflowResponse {
    let mut value = answer(request)?;
    if matches!(request, WorkflowRequest::Get { .. }) {
        value["properties"]["definition"]["triggers"]["manual"]["inputs"] = json!({"schema":{"type":"object","required":["text"],"properties":{"text":{"type":"string","title":"Reminder text","description":"Message to post when this workflow runs","minLength":1},"count":{"type":"integer","title":"Repeat count","default":1,"minimum":1}}}});
    }
    Ok(value)
}

/// The webhook template as the gallery shapes it: its markup names a team and a channel, and Microsoft Teams is its
/// only app.
pub fn webhook_template() -> Value {
    json!({"name":"synthetic-webhook","properties":{
        "displayName":"Send webhook alerts to a channel",
        "description":"Generate a webhook URL that lets anyone send external systems updates into a channel using rich adaptive cards or message cards.",
        "metadata":{"userIntentMarkup":{"version":"1.0","automation":{"template":{
            "id":"synthetic-webhook",
            "summary":"${format(resources['synthetic-webhook.summary'], context['parameters']['TeamsChannel'])}",
            "parameters":[
                {"id":"TeamsGroup","typeName":"Teams.Group","mode":"input"},
                {"id":"TeamsChannel","typeName":"Teams.Channel","mode":"input","dependencies":["TeamsGroup"]}
            ],
            "editorBindings":[{"parameterId":"TeamsGroup","label":"${resources['synthetic-webhook.TeamsGroup.label']}"},{"parameterId":"TeamsChannel"}]
        }}}},
        "definition":{
            "parameters":{"teams.group":{"defaultValue":"","type":"String"},"teams.channel":{"defaultValue":"","type":"String"}},
            "triggers":{"manual":{"type":"Request","kind":"TeamsWebhook","inputs":{"triggerAuthenticationType":"All"}}},
            "actions":{"Post":{"runAfter":{},"type":"OpenApiConnection","inputs":{"parameters":{"location":"Channel","body/recipient/groupId":"","body/recipient/channelId":"","body/messageBody":"@triggerBody()"},"host":{"apiId":"/providers/Microsoft.PowerApps/apis/shared_teams","connectionName":"shared_teams","operationId":"PostCardToConversation"}}}}
        },
        "connectionReferences":{"shared_teams":{"api":{"name":"shared_teams","id":"/providers/Microsoft.PowerApps/apis/shared_teams"}}}
    }})
}

/// The service for the webhook template: no Teams connection yet (one is made), the saved workflow, no runs.
pub fn webhook_answer(request: &WorkflowRequest) -> WorkflowResponse {
    Ok(match request {
        WorkflowRequest::Template { .. } => webhook_template(),
        WorkflowRequest::Connections { .. } | WorkflowRequest::History { .. } => json!({"value": []}),
        WorkflowRequest::AuthorizeConnection { connection, .. } => json!({"name": connection}),
        WorkflowRequest::Create { properties, .. } => json!({"name":"synthetic-webhook-flow","properties":{
            "displayName": properties["displayName"],
            "state": "Started",
            "definition": properties["definition"]
        }}),
        WorkflowRequest::Get { .. } => webhook_flow("Started"),
        WorkflowRequest::Owners { .. } => json!({"value": [
            {"name": OWNER, "properties": {"permissionType": "Principal", "principal": {"id": OWNER, "type": "User"}}},
            {"name": CO_OWNER, "properties": {"permissionType": "Principal", "principal": {"id": CO_OWNER, "type": "User"}}}
        ]}),
        other => return answer(other),
    })
}

/// The synthetic workflow's maker and its co-owner, by directory object id.
pub const OWNER: &str = "00000000-0000-4000-8000-00000000000a";
pub const CO_OWNER: &str = "00000000-0000-4000-8000-00000000000b";

/// The saved webhook workflow in `state`, made by [`OWNER`].
pub fn webhook_flow(state: &str) -> Value {
    json!({"name":"synthetic-flow","properties":{
        "displayName": "Send webhook alerts to General",
        "description": webhook_template()["properties"]["description"],
        "state": state,
        "creator": {"objectId": OWNER},
        "definition": webhook_template()["properties"]["definition"]
    }})
}

/// The webhook workflow turned off.
pub fn stopped_webhook_answer(request: &WorkflowRequest) -> WorkflowResponse {
    match request {
        WorkflowRequest::Get { .. } => Ok(webhook_flow("Stopped")),
        other => webhook_answer(other),
    }
}
