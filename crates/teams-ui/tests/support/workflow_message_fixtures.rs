//! Synthetic workflows run from a message, for the native view tests and the sandbox shots: templates and workflows
//! shaped as the gallery and the Flow service shape them, with question cards like Teams' three templates'. No tenant
//! identities or credentials.
use serde_json::{Value, json};
use teams_proto::{WorkflowFailure, WorkflowRequest, WorkflowResponse};

pub const FOLLOW: &str = "synthetic-follow";
pub const FOLLOW_FLOW: &str = "synthetic-follow-flow";
pub const SCHEDULE_FLOW: &str = "synthetic-schedule-flow";
pub const TASK_FLOW: &str = "synthetic-task-flow";
/// A follow-up workflow asking Teams' follow-up card.
pub const FOLLOW_CARD_FLOW: &str = "synthetic-follow-card";

/// A synthetic question card: a heading, a required note, a time and a switch.
pub fn question_card() -> Value {
    json!({"type":"AdaptiveCard","version":"1.3","body":[
        {"type":"TextBlock","text":"Remind me about this message:","weight":"Bolder","size":"Medium"},
        {"type":"Input.Text","id":"note","label":"Note:","isRequired":true,"errorMessage":"Please write a note"},
        {"type":"Input.Time","id":"when","label":"Time"},
        {"type":"Input.Toggle","id":"tell","title":"Notify myself"}
    ]})
}

/// A follow-up's card, as Teams' Follow up on a message asks.
pub fn follow_card() -> Value {
    json!({"type":"AdaptiveCard","version":"1.3","body":[
        {"type":"TextBlock","text":"Remind me about this message:","weight":"Bolder","size":"Medium"},
        {"type":"Input.Date","id":"remindDate","isRequired":true,"errorMessage":"Please select a remind date","label":"Date:"},
        {"type":"Input.Time","id":"remindTime","isRequired":true,"errorMessage":"Please select a remind time","label":"Time"},
        {"placeholder":"Notes for reminder","type":"Input.Text","id":"NotesForReminder","isMultiline":true}
    ]})
}

/// A scheduled reply's card, as Teams' Schedule a reply asks.
pub fn schedule_card() -> Value {
    json!({"type":"AdaptiveCard","version":"1.3","body":[
        {"type":"TextBlock","text":"When do you want your reply to be sent?","weight":"Bolder","size":"Medium"},
        {"type":"Input.Date","id":"scheduledDate","isRequired":true,"errorMessage":"Please select scheduled date","label":"Date:"},
        {"type":"Input.Time","id":"scheduledTime","isRequired":true,"errorMessage":"Please select scheduled time","label":"Time:"},
        {"type":"Input.Toggle","id":"notifyMyself","title":"Notify myself when message is posted?"},
        {"placeholder":"Message to send","type":"Input.Text","id":"messageToSend","isMultiline":true,"isRequired":true,"errorMessage":"Please enter message to send","label":"Reply:"}
    ]})
}

/// A Planner task's card, as Teams' Create a Planner task from a message asks.
pub fn task_card() -> Value {
    json!({"type":"AdaptiveCard","version":"1.3","body":[
        {"type":"TextBlock","text":"Add a task","weight":"Bolder","size":"Medium"},
        {"text":"Due date (Optional)","type":"TextBlock","size":"Medium"},
        {"type":"Input.Date","spacing":"Small","id":"taskDueDate"},
        {"type":"TextBlock","size":"Medium","text":"Assign to (Optional)"},
        {"type":"Input.ChoiceSet","choices.data":{"type":"Data.Query","dataset":"graph.microsoft.com/users"},"id":"assignees","isMultiSelect":true}
    ]})
}

/// The "For a selected message" trigger asking `card`.
pub fn message_trigger(card: &Value) -> Value {
    let card = card.to_string();
    json!({"manual":{"type":"Request","kind":"ApiConnection",
        "metadata":{"flowSystemMetadata":{"swaggerOperationId":"ForASelectedMessageV2","inputsAdaptiveCard": card}},
        "inputs":{"operationId":"ForASelectedMessageV2","parameters":{"inputsAdaptiveCard": card},
            "host":{"connection":{"name":"@parameters('$connections')['shared_teams']['connectionId']"}}}}})
}

/// A template run from a message, with no choices to make: its markup's context says so.
pub fn follow_template() -> Value {
    json!({"name": FOLLOW, "properties":{
        "displayName":"Follow up on a message",
        "description":"Synthetic: remind me about a message.",
        "categoryNames":["schedule","TeamsMessageContextTemplate"],
        "metadata":{"userIntentMarkup":{"version":"1.0","automation":{
            "template":{"id": FOLLOW, "summary":"${resources['synthetic-follow.summary']}","parameters":[],"editorBindings":[]},
            "context":{"triggerType":"messageAction"}
        },"dataBindings":{}}},
        "definition":{"triggers": message_trigger(&question_card()), "actions":{"Remind":{"runAfter":{},"type":"Wait","inputs":{}}}},
        "connectionReferences":{
            "shared_teams":{"api":{"name":"shared_teams"}},
            "shared_office365users":{"api":{"name":"shared_office365users"}}
        }
    }})
}

/// A workflow run from a message named `name`, on, asking `card`, its markup kept.
pub fn message_flow(id: &str, name: &str, card: &Value) -> Value {
    let markup = follow_template()["properties"]["metadata"]["userIntentMarkup"].to_string();
    json!({"name": id, "properties":{
        "displayName": name,
        "description":"Synthetic: run from a message.",
        "state":"Started",
        "definition":{"triggers": message_trigger(card), "actions":{
            "Do_Not_Remove_FlowIL":{"type":"InitializeVariable","runAfter":{},"metadata":{"userIntentMarkup": markup}},
            "Remind":{"runAfter":{"Do_Not_Remove_FlowIL":["Succeeded"]},"type":"Wait","inputs":{}}
        }}
    }})
}

/// The follow-up workflow saved from [`follow_template`].
pub fn follow_flow() -> Value {
    message_flow(FOLLOW_FLOW, "Follow up on a message", &question_card())
}

/// One flow list entry as the classic list summarizes it.
fn listed(id: &str, name: &str, state: &str, operation: Option<&str>) -> Value {
    let trigger = operation.map_or_else(
        || json!({"type":"Recurrence"}),
        |operation| json!({"type":"Request","kind":"ApiConnection","swaggerOperationId": operation}),
    );
    json!({"name": id, "properties":{"displayName": name, "state": state,
        "definitionSummary":{"triggers":[trigger]}}})
}

/// The service for a message's workflows: connected apps, the follow-up template, three workflows run from a message
/// (and one that isn't, and one off), an empty history.
#[expect(clippy::unnecessary_wraps, reason = "a workflow handler answers a service result")]
pub fn message_answer(request: &WorkflowRequest) -> WorkflowResponse {
    Ok(match request {
        WorkflowRequest::Catalog => {
            json!({"value":[{"id": FOLLOW, "name":"Follow up on a message","description":"Synthetic"}]})
        }
        WorkflowRequest::Gallery { .. } => json!({"value":[follow_template()]}),
        WorkflowRequest::Template { .. } => follow_template(),
        WorkflowRequest::CopilotEligibility => {
            json!({"userCopilotSettings":{"value":{"isCopilotAllowedByAdmin":true,"eligibility":{"isCopilotEligible":false}}}})
        }
        WorkflowRequest::Environments => {
            json!({"value": [{"name":"Default-synthetic", "properties":{"displayName":"Synthetic environment"}}]})
        }
        WorkflowRequest::Connections { .. } => json!({"value":[
            {"name":"own-teams","properties":{"apiId":"/providers/Microsoft.PowerApps/apis/shared_teams","statuses":[{"status":"Connected"}]}},
            {"name":"own-users","properties":{"apiId":"/providers/Microsoft.PowerApps/apis/shared_office365users","statuses":[{"status":"Connected"}]}}
        ]}),
        WorkflowRequest::Get { flow, .. } if flow == SCHEDULE_FLOW => message_flow(
            SCHEDULE_FLOW,
            "Schedule a message reply to send at the perfect time",
            &schedule_card(),
        ),
        WorkflowRequest::Get { flow, .. } if flow == FOLLOW_CARD_FLOW => {
            message_flow(FOLLOW_CARD_FLOW, "Follow up on a message", &follow_card())
        }
        WorkflowRequest::Get { flow, .. } if flow == TASK_FLOW => message_flow(
            TASK_FLOW,
            "Turn a Teams message into a task in Synthetic plan",
            &task_card(),
        ),
        WorkflowRequest::Get { flow, .. } if flow == "synthetic-weekly" => {
            json!({"name":"synthetic-weekly","properties":{
            "displayName":"Weekly channel reminder","state":"Started",
            "definition":{"triggers":{"Recurrence":{"type":"Recurrence"}},"actions":{}}}})
        }
        WorkflowRequest::Get { flow, .. } if flow == "synthetic-off" => json!({"name":"synthetic-off","properties":{
            "displayName":"Stopped message workflow","state":"Stopped",
            "definition":{"triggers": message_trigger(&question_card()),"actions":{}}}}),
        WorkflowRequest::Create { .. } | WorkflowRequest::Get { .. } => follow_flow(),
        WorkflowRequest::List { .. } => json!({"value":[
            listed(FOLLOW_FLOW, "Follow up on a message", "Started", Some("ForASelectedMessageV2")),
            listed(SCHEDULE_FLOW, "Schedule a message reply to send at the perfect time", "Started", Some("ForASelectedMessageV2")),
            listed(TASK_FLOW, "Turn a Teams message into a task in Synthetic plan", "Started", Some("ForASelectedMessageV2")),
            listed("synthetic-weekly", "Weekly channel reminder", "Started", None),
            listed("synthetic-off", "Stopped message workflow", "Stopped", Some("ForASelectedMessageV2"))
        ]}),
        WorkflowRequest::History { .. } | WorkflowRequest::Owners { .. } | WorkflowRequest::Triggers { .. } => {
            json!({"value": []})
        }
        _ => Value::Null,
    })
}

/// As [`message_answer`], but a run fails with the service's error.
pub fn failing_answer(request: &WorkflowRequest) -> WorkflowResponse {
    match request {
        WorkflowRequest::RunOnMessage { .. } => Err(WorkflowFailure::Service(502)),
        other => message_answer(other),
    }
}
