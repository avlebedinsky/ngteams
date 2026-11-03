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
