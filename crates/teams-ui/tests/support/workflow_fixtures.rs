//! Synthetic workflow data for native view tests; no tenant identities or credentials.
use serde_json::{Value, json};
use teams_proto::{WorkflowRequest, WorkflowResponse};
#[expect(
    clippy::unnecessary_wraps,
    reason = "the fake implements the service result callback and other callbacks return errors"
)]
pub fn answer(request: &WorkflowRequest) -> WorkflowResponse {
    Ok(match request {
        WorkflowRequest::Gallery { search, .. } => gallery(search.as_deref()),
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

/// The Workflows app's synthetic gallery, newest first: id, name, kinds and apps.
const GALLERY: [(&str, &str, &[&str], &[&str]); 10] = [
    (
        "synthetic-webhook",
        "Send webhook alerts to a channel",
        &["get_updates"],
        &["shared_teams"],
    ),
    (
        "synthetic-template",
        "Automate weekly reminders and updates in a conversation",
        &["schedule"],
        &["shared_teams"],
    ),
    (
        "synthetic-planner",
        "Notify a channel when Planner tasks are completed",
        &["get_updates"],
        &["shared_planner", "shared_office365users", "shared_teams"],
    ),
    (
        "synthetic-follow",
        "Follow up on a message",
        &["schedule"],
        &["shared_teams", "shared_office365users"],
    ),
    (
        "synthetic-task",
        "Create a Planner task from a message",
        &["take_actions"],
        &["shared_teams", "shared_planner"],
    ),
    (
        "synthetic-rss",
        "Post a channel message when a new RSS feed is published",
        &["get_updates"],
        &["shared_rss", "shared_teams"],
    ),
    (
        "synthetic-note",
        "Save a message to OneNote",
        &["take_actions"],
        &["shared_teams", "shared_onenote"],
    ),
    (
        "synthetic-sharepoint",
        "Notify a channel when SharePoint file or items change",
        &["get_updates"],
        &["shared_sharepointonline", "shared_teams"],
    ),
    (
        "synthetic-email",
        "Forward your emails to a channel",
        &["get_updates"],
        &["shared_office365", "shared_teams"],
    ),
    (
        "synthetic-reminder",
        "Send yourself a reminder",
        &["schedule"],
        &["shared_teams"],
    ),
];

/// One gallery template as the gallery shapes it.
fn gallery_entry(id: &str, name: &str, kinds: &[&str], apis: &[&str]) -> Value {
    let references: serde_json::Map<String, Value> = apis
        .iter()
        .map(|api| {
            let reference = json!({"api":{"name": api, "id": format!("/providers/Microsoft.PowerApps/apis/{api}")}});
            ((*api).to_owned(), reference)
        })
        .collect();
    json!({"name": id, "properties": {
        "displayName": name,
        "description": format!("{name}, synthetically, said at the length that needs more than the two lines a card shows."),
        "categoryNames": kinds,
        "connectionReferences": references
    }})
}

/// The Workflows app's gallery ([`GALLERY`]); with `search`, the templates whose names hold it.
pub fn gallery(search: Option<&str>) -> Value {
    let shown: Vec<Value> = GALLERY
        .iter()
        .filter(|(_, name, _, _)| search.is_none_or(|text| name.to_lowercase().contains(&text.to_lowercase())))
        .map(|(id, name, kinds, apis)| gallery_entry(id, name, kinds, apis))
        .collect();
    json!({"value": shown})
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
        }},
        "dataBindings":{"Post":[
            {"source":"${context['parameters']['TeamsGroup']}","target":"${json['inputs']['parameters']['body/recipient/groupId']}"},
            {"source":"${context['parameters']['TeamsChannel']}","target":"${json['inputs']['parameters']['body/recipient/channelId']}"}
        ]}}},
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

/// Another workflow of the account, off.
pub fn stopped_flow() -> Value {
    json!({"name":"synthetic-off","properties":{"displayName":"Synthetic stopped workflow","state":"Stopped",
        "definition":{"triggers":{},"actions":{}}}})
}

/// The webhook workflow turned off.
pub fn stopped_webhook_answer(request: &WorkflowRequest) -> WorkflowResponse {
    match request {
        WorkflowRequest::Get { .. } => Ok(webhook_flow("Stopped")),
        other => webhook_answer(other),
    }
}

/// Teams' Planner template id: its card's words are Teams' own.
pub const PLANNER: &str = "8f348e6a1ad64fe8bcb974b02585d987";

/// A Planner template as the gallery shapes it: a group and its plan (from the Planner connector's lists), then the
/// team and channel to notify.
pub fn planner_template() -> Value {
    json!({"name":"synthetic-planner","properties":{
        "displayName":"Notify a channel when Planner tasks are completed",
        "description":"Synthetic: announce completed tasks.",
        "metadata":{"userIntentMarkup":{"version":"1.0","automation":{"template":{
            "id": PLANNER,
            "parameters":[
                {"id":"PlannerGroupId","typeName":"Planner.GroupId","mode":"input"},
                {"id":"PlannerPlanId","typeName":"Planner.PlanId","mode":"input","dependencies":["PlannerGroupId"]},
                {"id":"TeamsGroup","typeName":"Teams.Group","mode":"input"},
                {"id":"TeamsChannel","typeName":"Teams.Channel","mode":"input","dependencies":["TeamsGroup"]}
            ],
            "editorBindings":[
                {"parameterId":"PlannerGroupId","label":"${resources['x']}"},
                {"parameterId":"PlannerPlanId","label":"${resources['x']}"},
                {"parameterId":"TeamsGroup","label":"${resources['x']}"},
                {"parameterId":"TeamsChannel","label":"${resources['x']}"}
            ]
        }},
        "dataBindings":{
            "When_a_task_is_completed":[
                {"source":"${context['parameters']['PlannerGroupId']}","target":"${json['inputs']['parameters']['groupId']}"},
                {"source":"${context['parameters']['PlannerPlanId']}","target":"${json['inputs']['parameters']['id']}"}
            ],
            "Post":[
                {"source":"${context['parameters']['TeamsGroup']}","target":"${json['inputs']['parameters']['body/recipient/groupId']}"},
                {"source":"${context['parameters']['TeamsChannel']}","target":"${json['inputs']['parameters']['body/recipient/channelId']}"}
            ]
        }}},
        "definition":{
            "triggers":{"When_a_task_is_completed":{"type":"OpenApiConnection","inputs":{
                "host":{"apiId":"/providers/Microsoft.PowerApps/apis/shared_planner","connectionName":"shared_planner","operationId":"OnCompleteTask_V3"},
                "parameters":{"groupId":"","id":""}}}},
            "actions":{"Post":{"runAfter":{},"type":"OpenApiConnection","inputs":{
                "host":{"apiId":"/providers/Microsoft.PowerApps/apis/shared_teams","connectionName":"shared_teams","operationId":"PostCardToConversation"},
                "parameters":{"location":"Channel","body/recipient/groupId":"","body/recipient/channelId":""}}}}
        },
        "connectionReferences":{
            "shared_planner":{"api":{"name":"shared_planner"}},
            "shared_teams":{"api":{"name":"shared_teams"}}
        }
    }})
}

/// The service for the Planner template: a Planner connection a failed sign-in left unauthenticated, the Planner
/// operation's lists, one group with one plan.
pub fn planner_answer(request: &WorkflowRequest) -> WorkflowResponse {
    Ok(match request {
        WorkflowRequest::Template { .. } => planner_template(),
        WorkflowRequest::Connections { .. } => json!({"value": [{"name":"shared-planner-left-over","properties":{
            "apiId":"/providers/Microsoft.PowerApps/apis/shared_planner",
            "statuses":[{"status":"Error","error":{"code":"Unauthenticated"}}]}}]}),
        WorkflowRequest::Operation { .. } => json!({"properties":{"inputsDefinition":{"properties":{
            "groupId":{"x-ms-dynamic-list":{"operationId":"ListGroups","parameters":{}}},
            "id":{"x-ms-dynamic-list":{"operationId":"ListGroupPlans","parameters":{"groupId":{"parameterReference":"groupId","required":true}}}}
        }}}}),
        WorkflowRequest::Choices { body, .. } => match body.pointer("/dynamicInvocationDefinition/operationId") {
            Some(Value::String(operation)) if operation == "ListGroups" => {
                json!({"value":[{"value":"synthetic-group","displayName":"Synthetic team"}]})
            }
            Some(Value::String(operation)) if operation == "ListGroupPlans" => {
                json!({"value":[{"value":"synthetic-plan","displayName":"Synthetic plan"}]})
            }
            _ => json!({"value": []}),
        },
        other => return webhook_answer(other),
    })
}

/// A webhook workflow saved from the card editor (its markup kept), renamed by hand, with its own Teams connection.
pub fn saved_webhook_flow() -> Value {
    let mut flow = webhook_flow("Started");
    let mut markup = webhook_template()["properties"]["metadata"]["userIntentMarkup"].clone();
    markup["automation"]["template"]["parameters"][0]["value"] = json!("synthetic-group");
    markup["automation"]["template"]["parameters"][0]["valueDisplayName"] = json!("Synthetic team");
    markup["automation"]["template"]["parameters"][1]["value"] = json!("synthetic-channel");
    markup["automation"]["template"]["parameters"][1]["valueDisplayName"] = json!("General");
    markup["automation"]["template"]["displayName"] = json!("Send webhook alerts to a channel");
    flow["properties"]["displayName"] = json!("Renamed by hand");
    flow["properties"]["lastModifiedTime"] = json!("2026-01-01T00:00:00Z");
    flow["properties"]["definition"]["actions"]["Do_Not_Remove_FlowIL"] = json!({
        "type":"InitializeVariable","runAfter":{},
        "metadata":{"userIntentMarkup": markup.to_string()}
    });
    flow["properties"]["definition"]["actions"]["Post"]["inputs"]["parameters"]["body/recipient/groupId"] =
        json!("synthetic-group");
    flow["properties"]["definition"]["actions"]["Post"]["inputs"]["parameters"]["body/recipient/channelId"] =
        json!("synthetic-channel");
    flow["properties"]["connectionReferences"] = json!({"shared_teams":{
        "id":"/providers/Microsoft.PowerApps/apis/shared_teams","connectionName":"own-connection","source":"Embedded"}});
    flow
}

/// The service for the saved, renamed webhook workflow in this channel, with another connected Teams connection.
pub fn saved_webhook_answer(request: &WorkflowRequest) -> WorkflowResponse {
    Ok(match request {
        WorkflowRequest::Get { .. } | WorkflowRequest::Update { .. } => saved_webhook_flow(),
        WorkflowRequest::Connections { .. } => json!({"value": [{"name":"another-connection","properties":{
            "apiId":"/providers/Microsoft.PowerApps/apis/shared_teams","statuses":[{"status":"Connected"}]}}]}),
        other => return webhook_answer(other),
    })
}

/// A template whose markup holds `parameters` filling `bindings` (node to (parameter, input)), on a definition with a
/// `trigger` and a Teams post, for the apps `apis`.
fn card_template(id: &str, name: &str, parameters: &Value, bindings: &Value, trigger: &Value, apis: &[&str]) -> Value {
    let references: serde_json::Map<String, Value> = apis
        .iter()
        .map(|api| ((*api).to_owned(), json!({"api":{"name": api}})))
        .collect();
    json!({"name": format!("synthetic-{id}"), "properties":{
        "displayName": name,
        "description": "Synthetic template.",
        "metadata":{"userIntentMarkup":{"version":"1.0","automation":{"template":{
            "id": id,
            "parameters": parameters,
            "editorBindings": parameters.as_array().into_iter().flatten()
                .map(|parameter| json!({"parameterId": parameter["id"], "label": "${resources['x']}"}))
                .collect::<Vec<_>>()
        }},"dataBindings": bindings}},
        "definition":{
            "triggers":{"Trigger": trigger},
            "actions":{"Post":{"runAfter":{},"type":"OpenApiConnection","inputs":{
                "host":{"apiId":"/providers/Microsoft.PowerApps/apis/shared_teams","operationId":"PostMessageToConversation"},
                "parameters":{"location":"Channel","body/recipient/groupId":"","body/recipient/channelId":""}}}}
        },
        "connectionReferences": references
    }})
}

fn bind(param: &str, path: &[&str]) -> Value {
    let target = path
        .iter()
        .fold(String::new(), |target, key| target + "['" + key + "']");
    json!({"source": format!("${{context['parameters']['{param}']}}"), "target": format!("${{json{target}}}")})
}

fn teams_parameters() -> Vec<Value> {
    vec![
        json!({"id":"TeamsGroup","typeName":"Teams.Group","mode":"input"}),
        json!({"id":"TeamsChannel","typeName":"Teams.Channel","mode":"input","dependencies":["TeamsGroup"]}),
    ]
}

/// Teams' weekly reminder: interval, start, time, zone, days, team, channel and the message.
pub fn weekly_template() -> Value {
    let mut parameters = vec![
        json!({"id":"BuiltInFrequency","typeName":"BuiltIn.Frequency","mode":"input","value":"Week"}),
        json!({"id":"BuiltInInterval","typeName":"BuiltIn.Interval","mode":"input"}),
        json!({"id":"BuiltInStartDate","typeName":"BuiltIn.StartDate","mode":"input"}),
        json!({"id":"BuiltInTimeOfDay","typeName":"BuiltIn.TimeOfDay","mode":"input"}),
        json!({"id":"BuiltInTimezone","typeName":"BuiltIn.Timezone","mode":"input"}),
        json!({"id":"BuiltInWeekDays","typeName":"BuiltIn.WeekDays","mode":"input","value":["Monday"]}),
    ];
    parameters.extend(teams_parameters());
    parameters.push(json!({"id":"TeamsMessageBody","typeName":"Teams.MessageBody","mode":"input"}));
    let recurrence = |param| bind(param, &["recurrence", "schedule"]);
    card_template(
        "805739fdad754e799502b6379900922e",
        "Automate weekly reminders and updates in a channel",
        &Value::from(parameters),
        &json!({
            "Trigger":[bind("BuiltInFrequency", &["recurrence","frequency"]), bind("BuiltInInterval", &["recurrence","interval"]),
                bind("BuiltInStartDate", &["recurrence","startTime"]), recurrence("BuiltInTimeOfDay"),
                bind("BuiltInTimezone", &["recurrence","timeZone"]), recurrence("BuiltInWeekDays")],
            "Post":[bind("TeamsGroup", &["inputs","parameters","body/recipient/groupId"]),
                bind("TeamsChannel", &["inputs","parameters","body/recipient/channelId"]),
                bind("TeamsMessageBody", &["inputs","parameters","body/messageBody"])]
        }),
        &json!({"type":"Recurrence","recurrence":{"frequency":"Week","interval":1}}),
        &["shared_teams"],
    )
}

/// Teams' forward-your-email template: a mail folder, To and From people, a subject, then the team and channel.
pub fn email_template() -> Value {
    let mut parameters = vec![
        json!({"id":"OutlookMailFolderPath","typeName":"Outlook.MailFolderPath","mode":"input","value":"Inbox","valueDisplayName":"Inbox"}),
        json!({"id":"OutlookToLine","typeName":"Outlook.ToLine","mode":"input","optional":true}),
        json!({"id":"OutlookFrom","typeName":"Outlook.From","mode":"input","optional":true}),
        json!({"id":"OutlookSubjectFilter","typeName":"Outlook.SubjectFilter","mode":"input","optional":true}),
    ];
    parameters.extend(teams_parameters());
    card_template(
        "190c5a56070e4a8b802089be6455553e",
        "Forward your emails to a channel",
        &Value::from(parameters),
        &json!({
            "Trigger":[bind("OutlookMailFolderPath", &["inputs","parameters","folderPath"]),
                bind("OutlookToLine", &["inputs","parameters","to"]), bind("OutlookFrom", &["inputs","parameters","from"]),
                bind("OutlookSubjectFilter", &["inputs","parameters","subjectFilter"])],
            "Post":[bind("TeamsGroup", &["inputs","parameters","body/recipient/groupId"]),
                bind("TeamsChannel", &["inputs","parameters","body/recipient/channelId"])]
        }),
        &json!({"type":"OpenApiConnectionNotification","inputs":{
            "host":{"apiId":"/providers/Microsoft.PowerApps/apis/shared_office365","operationId":"OnNewEmailV3"},
            "parameters":{"folderPath":"","to":"","from":"","subjectFilter":""}}}),
        &["shared_office365", "shared_office365users", "shared_teams"],
    )
}

/// The service for the weekly and email templates (by the catalog's ids) and their mail folders.
pub fn cards_answer(request: &WorkflowRequest) -> WorkflowResponse {
    Ok(match request {
        WorkflowRequest::Template { id } if id == "synthetic-template" => weekly_template(),
        WorkflowRequest::Template { id } if id == "synthetic-email" => email_template(),
        WorkflowRequest::Operation { .. } => json!({"properties":{"inputsDefinition":{"properties":{
            "folderPath":{"x-ms-dynamic-tree":{"open":{"operationId":"OnFilePickerOpen","parameters":{}},
                "browse":{"operationId":"OnFilePickerBrowse","parameters":{}}}}
        }}}}),
        WorkflowRequest::Choices { .. } => json!({"value":[
            {"value":"Id::synthetic-inbox","displayName":"Inbox","isParent":true,"fullyQualifiedDisplayName":"Inbox","selectionState":{"id":"synthetic-inbox"}},
            {"value":"Id::synthetic-archive","displayName":"Archive","isParent":false,"fullyQualifiedDisplayName":"Archive","selectionState":{"id":"synthetic-archive"}}
        ]}),
        other => return planner_answer(other),
    })
}
