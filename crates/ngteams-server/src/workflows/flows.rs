//! The Flow API's routes under `/workflows/providers/Microsoft.ProcessSimple`: environments, the gallery, and an
//! environment's workflows with their owners, triggers and runs, as the classic Flow resource provider answers them.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::definition::{self, Target};
use super::{Answer, Caller, Error, Flow, gallery, hook, repo};
use crate::app::App;
use crate::db::users;

/// The properties a save keeps; the rest of the workflow's resource is the server's.
const KEPT: [&str; 6] = [
    "displayName",
    "description",
    "definition",
    "connectionReferences",
    "templateName",
    "metadata",
];
/// The longest workflow name.
const MAX_NAME: usize = 256;

type Body = Result<Json<Value>, axum::extract::rejection::JsonRejection>;

pub(super) fn routes() -> Router<Arc<App>> {
    let flow = "/environments/{environment}/flows/{flow}";
    Router::new()
        .route("/environments", get(environments))
        .route("/galleries/public/teamsworkflowtemplates", get(gallery::catalog))
        .route("/galleries/public/templates", get(gallery::gallery))
        .route("/galleries/public/templates/{id}", get(gallery::template))
        .route("/environments/{environment}/flows", get(list).post(create))
        .route(flow, get(read).patch(change).delete(remove))
        .route(&format!("{flow}/start"), post(start))
        .route(&format!("{flow}/stop"), post(stop))
        .route(&format!("{flow}/owners"), get(owners))
        .route(&format!("{flow}/owners/{{owner}}"), delete(remove_owner))
        .route(&format!("{flow}/modifyowners"), post(modify_owners))
        .route(&format!("{flow}/triggers"), get(triggers))
        .route(
            &format!("{flow}/triggers/{{trigger}}/listCallbackUrl"),
            post(callback_url),
        )
        .route(&format!("{flow}/triggers/{{trigger}}/run"), post(run_trigger))
        .route(&format!("{flow}/runs"), get(runs))
        .route(&format!("{flow}/runs/{{run}}/actions"), get(run_actions))
}

/// `GET …/environments`: the account's one environment.
async fn environments(caller: Caller<Flow>) -> Json<Value> {
    let name = caller.environment();
    Json(json!({"value": [{
        "name": name,
        "id": format!("/providers/Microsoft.ProcessSimple/environments/{name}"),
        "type": "Microsoft.ProcessSimple/environments",
        "location": "own",
        "properties": {
            "displayName": "Default",
            "isDefault": true,
            "environmentSku": "Default",
            "provisioningState": "Succeeded",
            "creationType": "DefaultTenant",
        },
    }]}))
}

#[derive(Debug, Default, Deserialize)]
struct ListQuery {
    #[serde(rename = "flowState")]
    state: Option<String>,
}

/// `GET …/flows`: the workflows the account owns, newest first (`flowState=active`: those turned on).
async fn list(
    State(app): State<Arc<App>>,
    caller: Caller<Flow>,
    Path(environment): Path<String>,
    Query(query): Query<ListQuery>,
) -> Answer<Json<Value>> {
    caller.check(&environment)?;
    let active = query
        .state
        .as_deref()
        .is_some_and(|state| state.eq_ignore_ascii_case("active"));
    let value: Vec<Value> = repo::owned_by(&app.db, &caller.id)
        .await?
        .iter()
        .filter(|flow| !active || flow.enabled)
        .map(|flow| resource(flow, &caller))
        .collect();
    Ok(Json(json!({"value": value})))
}

/// `POST …/flows`: a workflow made from a saved definition, on unless saved stopped.
async fn create(
    State(app): State<Arc<App>>,
    caller: Caller<Flow>,
    Path(environment): Path<String>,
    body: Body,
) -> Answer<Response> {
    caller.check(&environment)?;
    let properties = properties(&super::body(body)?)?;
    let name = display_name(properties.get("displayName"))?;
    let target = target(&properties)?;
    connections_are(&app, std::slice::from_ref(&caller.id), &properties).await?;
    if !sees(&app, &caller.id, &target).await? {
        return Err(Error::FORBIDDEN);
    }
    let at = super::now();
    let flow = repo::Workflow {
        id: uuid::Uuid::new_v4().to_string(),
        creator_id: caller.id.clone(),
        display_name: name,
        enabled: properties.get("state").and_then(Value::as_str) != Some("Stopped"),
        properties: Value::Object(kept(properties)).to_string(),
        target_thread: target.thread().to_owned(),
        secret: hook::secret().map_err(|_| Error::Code(StatusCode::SERVICE_UNAVAILABLE, "ServiceUnavailable"))?,
        created_at: at,
        updated_at: at,
    };
    repo::insert(&app.db, &flow).await?;
    Ok((StatusCode::CREATED, Json(resource(&flow, &caller))).into_response())
}

/// `GET …/flows/<flow>`.
async fn read(
    State(app): State<Arc<App>>,
    caller: Caller<Flow>,
    Path((environment, flow)): Path<(String, String)>,
) -> Answer<Json<Value>> {
    let flow = owned(&app, &caller, &environment, &flow).await?;
    Ok(Json(resource(&flow, &caller)))
}

/// `PATCH …/flows/<flow>`: a rename (only `displayName`) or an edit (its definition and connections too).
async fn change(
    State(app): State<Arc<App>>,
    caller: Caller<Flow>,
    Path((environment, flow)): Path<(String, String)>,
    body: Body,
) -> Answer<Json<Value>> {
    let mut flow = owned(&app, &caller, &environment, &flow).await?;
    let changes = properties(&super::body(body)?)?;
    let mut kept_now = stored(&flow);
    if changes.contains_key("displayName") {
        flow.display_name = display_name(changes.get("displayName"))?;
    }
    if changes.contains_key("definition") {
        let target = target(&changes)?;
        if !sees(&app, &caller.id, &target).await? {
            return Err(Error::FORBIDDEN);
        }
        flow.target_thread = target.thread().to_owned();
    }
    connections_are(&app, &repo::owners(&app.db, &flow.id).await?, &changes).await?;
    kept_now.extend(kept(changes));
    flow.properties = Value::Object(kept_now).to_string();
    flow.updated_at = super::now().max(flow.updated_at + 1);
    if !repo::update(&app.db, &flow).await? {
        return Err(Error::FLOW_NOT_FOUND);
    }
    Ok(Json(resource(&flow, &caller)))
}

/// `DELETE …/flows/<flow>`: its webhook link stops working with it.
async fn remove(
    State(app): State<Arc<App>>,
    caller: Caller<Flow>,
    Path((environment, flow)): Path<(String, String)>,
) -> Answer<StatusCode> {
    let flow = owned(&app, &caller, &environment, &flow).await?;
    repo::delete(&app.db, &flow.id).await?;
    Ok(StatusCode::OK)
}

async fn start(
    State(app): State<Arc<App>>,
    caller: Caller<Flow>,
    Path((environment, flow)): Path<(String, String)>,
) -> Answer<StatusCode> {
    turn(&app, &caller, &environment, &flow, true).await
}

async fn stop(
    State(app): State<Arc<App>>,
    caller: Caller<Flow>,
    Path((environment, flow)): Path<(String, String)>,
) -> Answer<StatusCode> {
    turn(&app, &caller, &environment, &flow, false).await
}

/// Turns the workflow on or off: a webhook post to one that is off is refused.
async fn turn(app: &App, caller: &Caller<Flow>, environment: &str, flow: &str, on: bool) -> Answer<StatusCode> {
    let mut flow = owned(app, caller, environment, flow).await?;
    flow.enabled = on;
    flow.updated_at = super::now().max(flow.updated_at + 1);
    repo::update(&app.db, &flow).await?;
    Ok(StatusCode::OK)
}

/// `GET …/owners`: its creator first, then the others.
async fn owners(
    State(app): State<Arc<App>>,
    caller: Caller<Flow>,
    Path((environment, flow)): Path<(String, String)>,
) -> Answer<Json<Value>> {
    let flow = owned(&app, &caller, &environment, &flow).await?;
    let mut owners = repo::owners(&app.db, &flow.id).await?;
    owners.sort_by_key(|owner| *owner != flow.creator_id);
    let base = flow_id(&environment, &flow.id);
    let value: Vec<Value> = owners
        .iter()
        .map(|owner| {
            json!({
                "name": owner,
                "id": format!("{base}/permissions/{owner}"),
                "type": "Microsoft.ProcessSimple/environments/flows/permissions",
                "properties": {
                    "roleName": "Owner",
                    "permissionType": "Principal",
                    "principal": {"id": owner, "type": "User", "tenantId": caller.tenant},
                },
            })
        })
        .collect();
    Ok(Json(json!({"value": value})))
}

/// `POST …/modifyowners`: `put` makes each principal an owner, `delete` takes each off.
async fn modify_owners(
    State(app): State<Arc<App>>,
    caller: Caller<Flow>,
    Path((environment, flow)): Path<(String, String)>,
    body: Body,
) -> Answer<StatusCode> {
    let flow = owned(&app, &caller, &environment, &flow).await?;
    let body = super::body(body)?;
    let principals = |key: &str| -> Answer<Vec<String>> {
        match body.get(key) {
            None | Some(Value::Null) => Ok(Vec::new()),
            Some(Value::Array(entries)) => entries
                .iter()
                .map(|entry| {
                    entry
                        .pointer("/properties/principal/id")
                        .or_else(|| entry.get("name"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .ok_or(Error::BAD_REQUEST)
                })
                .collect(),
            Some(_) => Err(Error::BAD_REQUEST),
        }
    };
    let added = principals("put")?;
    let taken = principals("delete")?;
    let target = target(&stored(&flow))?;
    for owner in &added {
        let person = users::by_id(&app.db, owner).await?;
        if person.is_none_or(|person| person.disabled || person.deleted_at.is_some()) {
            return Err(Error::Code(StatusCode::BAD_REQUEST, "PrincipalNotFound"));
        }
        // whoever owns a workflow reads where it posts and its webhook link
        if !sees(&app, owner, &target).await? {
            return Err(Error::FORBIDDEN);
        }
    }
    for owner in &added {
        repo::add_owner(&app.db, &flow.id, owner).await?;
    }
    for owner in &taken {
        take_owner(&app, &flow, owner).await?;
    }
    Ok(StatusCode::OK)
}

/// `DELETE …/owners/<owner>`.
async fn remove_owner(
    State(app): State<Arc<App>>,
    caller: Caller<Flow>,
    Path((environment, flow, owner)): Path<(String, String, String)>,
) -> Answer<StatusCode> {
    let flow = owned(&app, &caller, &environment, &flow).await?;
    take_owner(&app, &flow, &owner).await?;
    Ok(StatusCode::OK)
}

/// Takes a co-owner off; the creator stays.
async fn take_owner(app: &App, flow: &repo::Workflow, owner: &str) -> Answer<()> {
    if owner == flow.creator_id {
        return Err(Error::Code(StatusCode::BAD_REQUEST, "CannotRemoveCreator"));
    }
    if repo::remove_owner(&app.db, &flow.id, owner).await? {
        Ok(())
    } else {
        Err(Error::NOT_FOUND)
    }
}

/// `GET …/triggers`: its webhook trigger, enabled while the workflow is on.
async fn triggers(
    State(app): State<Arc<App>>,
    caller: Caller<Flow>,
    Path((environment, flow)): Path<(String, String)>,
) -> Answer<Json<Value>> {
    let flow = owned(&app, &caller, &environment, &flow).await?;
    let properties = stored(&flow);
    let definition = properties.get("definition").cloned().unwrap_or_default();
    let name = definition::webhook_trigger(&definition).unwrap_or_else(|| "manual".to_owned());
    let kind = definition
        .pointer(&format!("/triggers/{name}/kind"))
        .cloned()
        .unwrap_or_else(|| json!("TeamsWebhook"));
    Ok(Json(json!({"value": [{
        "name": name,
        "id": format!("{}/triggers/{name}", flow_id(&environment, &flow.id)),
        "type": "Microsoft.ProcessSimple/environments/flows/triggers",
        "properties": {
            "type": "Request",
            "kind": kind,
            "state": if flow.enabled { "Enabled" } else { "Disabled" },
            "createdTime": super::iso(flow.created_at),
            "changedTime": super::iso(flow.updated_at),
        },
    }]})))
}

/// `POST …/triggers/<trigger>/listCallbackUrl`: the webhook link "Copy webhook link" copies.
async fn callback_url(
    State(app): State<Arc<App>>,
    caller: Caller<Flow>,
    Path((environment, flow, trigger)): Path<(String, String, String)>,
) -> Answer<Json<Value>> {
    let flow = owned(&app, &caller, &environment, &flow).await?;
    let definition = stored(&flow).get("definition").cloned().unwrap_or_default();
    if definition::webhook_trigger(&definition).as_deref() != Some(trigger.as_str()) {
        return Err(Error::Code(StatusCode::NOT_FOUND, "TriggerNotFound"));
    }
    Ok(Json(hook::callback(&app.config, &flow, &trigger)))
}

/// `POST …/triggers/<trigger>/run`: a webhook workflow runs only from its link.
async fn run_trigger(
    State(app): State<Arc<App>>,
    caller: Caller<Flow>,
    Path((environment, flow, _)): Path<(String, String, String)>,
) -> Answer<StatusCode> {
    owned(&app, &caller, &environment, &flow).await?;
    Err(Error::Code(StatusCode::BAD_REQUEST, "TriggerNotManual"))
}

/// `GET …/runs`: its run history, newest first.
async fn runs(
    State(app): State<Arc<App>>,
    caller: Caller<Flow>,
    Path((environment, flow)): Path<(String, String)>,
) -> Answer<Json<Value>> {
    let flow = owned(&app, &caller, &environment, &flow).await?;
    let base = flow_id(&environment, &flow.id);
    let trigger = definition::webhook_trigger(&stored(&flow).get("definition").cloned().unwrap_or_default())
        .unwrap_or_else(|| "manual".to_owned());
    let value: Vec<Value> = repo::runs(&app.db, &flow.id)
        .await?
        .iter()
        .map(|run| {
            let mut properties = timing(run);
            properties.insert(
                "trigger".into(),
                json!({"name": trigger, "startTime": super::iso(run.started_at),
                       "endTime": super::iso(run.started_at), "status": "Succeeded"}),
            );
            json!({
                "name": run.id,
                "id": format!("{base}/runs/{}", run.id),
                "type": "Microsoft.ProcessSimple/environments/flows/runs",
                "properties": properties,
            })
        })
        .collect();
    Ok(Json(json!({"value": value})))
}

/// `GET …/runs/<run>/actions`: the run's post, and how it went.
async fn run_actions(
    State(app): State<Arc<App>>,
    caller: Caller<Flow>,
    Path((environment, flow, run)): Path<(String, String, String)>,
) -> Answer<Json<Value>> {
    let flow = owned(&app, &caller, &environment, &flow).await?;
    let run = repo::run(&app.db, &flow.id, &run).await?.ok_or(Error::NOT_FOUND)?;
    Ok(Json(json!({"value": [{
        "name": hook::POST_ACTION,
        "id": format!("{}/runs/{}/actions/{}", flow_id(&environment, &flow.id), run.id, hook::POST_ACTION),
        "type": "Microsoft.ProcessSimple/environments/flows/runs/actions",
        "properties": timing(&run),
    }]})))
}

/// A run's or its action's status and times, with the failure's code.
fn timing(run: &repo::Run) -> Map<String, Value> {
    let mut properties = Map::new();
    properties.insert("startTime".into(), json!(super::iso(run.started_at)));
    properties.insert("endTime".into(), json!(super::iso(run.ended_at)));
    properties.insert("status".into(), json!(run.status));
    if let Some(code) = &run.error {
        properties.insert("code".into(), json!(code));
        properties.insert("error".into(), json!({"code": code, "message": code}));
    }
    properties
}

/// The workflow `flow` of the caller's environment, when the caller owns it.
async fn owned(app: &App, caller: &Caller<Flow>, environment: &str, flow: &str) -> Answer<repo::Workflow> {
    caller.check(environment)?;
    repo::owned(&app.db, flow, &caller.id)
        .await?
        .ok_or(Error::FLOW_NOT_FOUND)
}

/// A save's `properties`.
fn properties(body: &Value) -> Answer<Map<String, Value>> {
    body.get("properties")
        .and_then(Value::as_object)
        .cloned()
        .ok_or(Error::BAD_REQUEST)
}

/// What a save keeps of its properties.
fn kept(mut properties: Map<String, Value>) -> Map<String, Value> {
    properties.retain(|key, _| KEPT.contains(&key.as_str()));
    properties
}

/// The properties a workflow keeps.
fn stored(flow: &repo::Workflow) -> Map<String, Value> {
    serde_json::from_str(&flow.properties).unwrap_or_default()
}

fn display_name(value: Option<&Value>) -> Answer<String> {
    let name = value.and_then(Value::as_str).map(str::trim).unwrap_or_default();
    if name.is_empty() || name.chars().count() > MAX_NAME {
        return Err(Error::BAD_REQUEST);
    }
    Ok(name.to_owned())
}

/// Where a saved definition posts, when the server can run it.
fn target(properties: &Map<String, Value>) -> Answer<Target> {
    properties
        .get("definition")
        .and_then(definition::read)
        .map(|(_, target)| target)
        .ok_or(Error::Code(StatusCode::BAD_REQUEST, "WorkflowNotSupported"))
}

/// Every connection a save's references name is one of the workflow's owners'.
async fn connections_are(app: &App, owners: &[String], properties: &Map<String, Value>) -> Answer<()> {
    let Some(references) = properties.get("connectionReferences") else {
        return Ok(());
    };
    let references = references.as_object().ok_or(Error::BAD_REQUEST)?;
    if references.is_empty() {
        return Ok(());
    }
    let mut own = Vec::new();
    for owner in owners {
        own.extend(
            repo::connections(&app.db, owner)
                .await?
                .into_iter()
                .map(|connection| connection.name),
        );
    }
    for reference in references.values() {
        let name = reference
            .get("connectionName")
            .and_then(Value::as_str)
            .ok_or(Error::BAD_REQUEST)?;
        if !own.iter().any(|connection| connection == name) {
            return Err(Error::Code(StatusCode::BAD_REQUEST, "ConnectionNotFound"));
        }
    }
    Ok(())
}

/// Whether `user` may post where `target` is: in the channel's team (and a private channel's own people), or in
/// the chat; never into an archived team or channel.
pub(super) async fn sees(app: &App, user: &str, target: &Target) -> Answer<bool> {
    let db = &app.db;
    match target {
        Target::Channel { group, channel } => {
            let Some(channel) = crate::teams::repo::channel(db, channel).await? else {
                return Ok(false);
            };
            let Some(team) = crate::teams::repo::team(db, &channel.team_id).await? else {
                return Ok(false);
            };
            if team.group_id != *group || team.archived || channel.archived {
                return Ok(false);
            }
            if crate::teams::repo::membership(db, &team.id, user).await?.is_none() {
                return Ok(false);
            }
            Ok(!channel.private
                || !crate::teams::repo::joined(db, user, std::slice::from_ref(&channel.id))
                    .await?
                    .is_empty())
        }
        Target::Chat(chat) => {
            if crate::teams::repo::channel(db, chat).await?.is_some() {
                return Ok(false);
            }
            Ok(crate::chatsvc::service::members(db, chat)
                .await?
                .iter()
                .any(|member| member == user))
        }
    }
}

/// A workflow's resource id.
fn flow_id(environment: &str, flow: &str) -> String {
    format!("/providers/Microsoft.ProcessSimple/environments/{environment}/flows/{flow}")
}

/// The workflow as the Flow API answers it: its kept properties with the server's own.
fn resource(flow: &repo::Workflow, caller: &Caller<Flow>) -> Value {
    let environment = caller.environment();
    let mut properties = stored(flow);
    let definition = properties.get("definition").cloned().unwrap_or_default();
    let triggers: Vec<Value> = definition
        .get("triggers")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(_, trigger)| json!({"type": trigger.get("type"), "kind": trigger.get("kind")}))
        .collect();
    let own = json!({
        "apiId": "/providers/Microsoft.PowerApps/apis/shared_logicflows",
        "displayName": flow.display_name,
        "state": if flow.enabled { "Started" } else { "Stopped" },
        "createdTime": super::iso(flow.created_at),
        "lastModifiedTime": super::iso(flow.updated_at),
        "flowSuspensionReason": "None",
        "environment": {
            "name": environment,
            "type": "Microsoft.ProcessSimple/environments",
            "id": format!("/providers/Microsoft.ProcessSimple/environments/{environment}"),
        },
        "creator": {
            "tenantId": caller.tenant, "objectId": flow.creator_id, "userId": flow.creator_id,
            "userType": "ActiveDirectory",
        },
        "userType": "Owner",
        "definitionSummary": {"triggers": triggers, "actions": []},
    });
    if let Value::Object(own) = own {
        properties.extend(own);
    }
    json!({
        "name": flow.id,
        "id": flow_id(&environment, &flow.id),
        "type": "Microsoft.ProcessSimple/environments/flows",
        "properties": properties,
    })
}
