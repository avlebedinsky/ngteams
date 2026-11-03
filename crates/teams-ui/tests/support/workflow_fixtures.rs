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
            {"id":"synthetic-template","name":"Automate weekly reminders and updates in a conversation","description":"Get weekly reminders and status updates automatically so nothing gets missed"},
            {"id":"synthetic-planner","name":"Notify a channel when Planner tasks are completed","description":"Celebrate progress by notifying a channel when a Planner task is marked as done"},
            {"id":"synthetic-sharepoint","name":"Notify a channel when SharePoint file or items change","description":"Let everyone in the channel know whenever a SharePoint file or list item changes"},
            {"id":"synthetic-email","name":"Forward your emails to a channel","description":"Automatically forward important emails to a channel to keep everyone in the loop"},
            {"id":"synthetic-files","name":"Notify a channel when new SharePoint files are added","description":"Keep everyone informed so they never miss a file update"},
            {"id":"synthetic-webhook","name":"Send webhook alerts to a channel","description":"Get updates in Teams from tools compatible with incoming webhooks"}
        ]}),
        WorkflowRequest::Environments => {
            json!({"value": [{"name":"Default-synthetic", "properties":{"displayName":"Synthetic environment"}}]})
        }
        WorkflowRequest::Connections { .. } => json!({"value": []}),
        WorkflowRequest::List { .. } => json!({"value": [{"name":"synthetic-flow"}]}),
        WorkflowRequest::Get { .. } | WorkflowRequest::Template { .. } => flow(),
        WorkflowRequest::History { .. } => {
            json!({"value": [{"name":"synthetic-run", "properties":{"status":"Succeeded","startTime":"2026-01-01T09:00:00Z","endTime":"2026-01-01T09:00:01Z"}}]})
        }
        _ => Value::Null,
    })
}
pub fn flow() -> Value {
    json!({"name":"synthetic-flow","properties":{"displayName":"Weekly channel reminder","description":"A synthetic workflow","state":"Started","definition":{"parameters":{"message":{"type":"String","defaultValue":"Weekly reminder"}},"triggers":{"manual":{"type":"Request","kind":"Button"}},"actions":{"compose":{"type":"Compose","inputs":"Synthetic value","team":"synthetic-group","channel":"synthetic-channel"}}},"connectionReferences":{}}})
}
