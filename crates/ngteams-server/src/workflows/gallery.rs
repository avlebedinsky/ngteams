//! The template gallery: Teams' two webhook templates under their gallery ids, in the shapes of the channel catalog
//! (`teamsworkflowtemplates`), the Workflows app's gallery (`templates?$filter=…`) and a template's read. Each
//! definition posts every Adaptive Card a webhook receives (its body's `attachments`) as the Workflows bot, to the
//! team and channel, or the chat, the card editor fills in.

use axum::Json;
use axum::extract::{Path, Query};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Answer, Caller, Error, Flow};

/// "Send webhook alerts to a channel".
pub(super) const CHANNEL: &str = "d271a6f01c2545a28348d8f2cddf4c8f";
/// "Send webhook alerts to a chat".
pub(super) const CHAT: &str = "34ac6c11796143e1807d6e16c4c28050";
const TEAMS_API: &str = "/providers/Microsoft.PowerApps/apis/shared_teams";

/// A template the gallery holds.
struct Template {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    /// Its gallery categories: where the channel catalog, the app's Home and Templates find it.
    categories: &'static [&'static str],
}

const TEMPLATES: [Template; 2] = [
    Template {
        id: CHANNEL,
        name: "Send webhook alerts to a channel",
        description: "Generate a webhook URL that lets anyone send external systems updates into a channel using \
                      rich adaptive cards or message cards.",
        categories: &[
            "notifications",
            "microsoftteams",
            "microsoftteams_channel",
            "home_featured",
            "get_updates",
            "simpleEditorSupported",
            "neptune_teams_pa_home",
        ],
    },
    Template {
        id: CHAT,
        name: "Send webhook alerts to a chat",
        description: "Generate a webhook URL that lets anyone send external systems updates into a chat using rich \
                      adaptive cards or message cards.",
        categories: &[
            "notifications",
            "microsoftteams",
            "microsoftteams_chat",
            "home_featured",
            "get_updates",
            "simpleEditorSupported",
            "neptune_teams_pa_home",
        ],
    },
];

/// The `OData` query a gallery read carries.
#[derive(Debug, Default, Deserialize)]
pub(super) struct GalleryQuery {
    #[serde(rename = "$filter")]
    filter: Option<String>,
    #[serde(rename = "$search")]
    search: Option<String>,
    #[serde(rename = "$top")]
    top: Option<usize>,
}

/// The categories a filter's `any(x:x eq '<category>')` asks for and its `all(x:x ne '<category>')` leaves out. The
/// client builds no other filter.
fn wanted(filter: &str) -> (Vec<String>, Vec<String>) {
    let quoted = |marker: &str| -> Vec<String> {
        filter
            .match_indices(marker)
            .filter_map(|(at, _)| literal(&filter[at + marker.len()..]))
            .collect()
    };
    (quoted("x eq '"), quoted("x ne '"))
}

/// The `OData` string literal `rest` starts with (its opening quote already read), a doubled quote standing for one.
fn literal(rest: &str) -> Option<String> {
    let mut value = String::new();
    let mut chars = rest.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\'' {
            value.push(c);
        } else if chars.peek() == Some(&'\'') {
            chars.next();
            value.push('\'');
        } else {
            return Some(value);
        }
    }
    None
}

fn matching(query: &GalleryQuery) -> impl Iterator<Item = &'static Template> + '_ {
    let (some, none) = wanted(query.filter.as_deref().unwrap_or_default());
    let search = query.search.as_deref().map(str::to_lowercase);
    TEMPLATES
        .iter()
        .filter(move |template| {
            some.iter()
                .all(|category| template.categories.contains(&category.as_str()))
                && !none
                    .iter()
                    .any(|category| template.categories.contains(&category.as_str()))
                && search.as_ref().is_none_or(|text| {
                    template.name.to_lowercase().contains(text) || template.description.to_lowercase().contains(text)
                })
        })
        .take(query.top.unwrap_or(usize::MAX))
}

/// `GET …/galleries/public/teamsworkflowtemplates`: the channel dialog's catalog.
pub(super) async fn catalog(_: Caller<Flow>, Query(query): Query<GalleryQuery>) -> Json<Value> {
    let value: Vec<Value> = matching(&query)
        .map(|template| {
            json!({
                "id": template.id, "name": template.name, "description": template.description,
                "icons": [], "categories": ["Notifications"], "configUrl": "",
            })
        })
        .collect();
    Json(json!({"value": value}))
}

/// `GET …/galleries/public/templates`: the Workflows app's gallery, each template whole.
pub(super) async fn gallery(_: Caller<Flow>, Query(query): Query<GalleryQuery>) -> Json<Value> {
    let value: Vec<Value> = matching(&query).map(resource).collect();
    Json(json!({"value": value}))
}

/// `GET …/galleries/public/templates/<id>`: a template with its definition and connections.
pub(super) async fn template(_: Caller<Flow>, Path(id): Path<String>) -> Answer<Json<Value>> {
    TEMPLATES
        .iter()
        .find(|template| template.id == id)
        .map(|template| Json(resource(template)))
        .ok_or(Error::NOT_FOUND)
}

fn resource(template: &Template) -> Value {
    let channel = template.id == CHANNEL;
    json!({
        "name": template.id,
        "id": format!("/providers/Microsoft.ProcessSimple/galleries/public/templates/{}", template.id),
        "type": "Microsoft.ProcessSimple/galleries/templates",
        "properties": {
            "displayName": template.name,
            "summary": template.description,
            "description": template.description,
            "categoryNames": template.categories,
            "galleryName": "public",
            "publishedTime": "2026-01-01T00:00:00Z",
            "author": {"displayName": "ngteams server", "source": "community"},
            "definition": if channel { channel_definition() } else { chat_definition() },
            "connectionReferences": {"shared_teams": {
                "api": {"name": "shared_teams", "id": TEAMS_API, "type": "/providers/Microsoft.PowerApps/apis",
                        "properties": {"displayName": "Microsoft Teams"}},
                "source": "NotSpecified",
                "connectionNamingMode": "NotSpecified",
            }},
            "metadata": {"userIntentMarkup": if channel { channel_markup() } else { chat_markup() }},
        },
    })
}

/// The webhook trigger both templates start with: a request carrying a Teams card payload.
fn trigger() -> Value {
    json!({"manual": {
        "metadata": {},
        "type": "Request",
        "kind": "TeamsWebhook",
        "inputs": {
            "schema": {"type": "object", "properties": {
                "type": {"type": "string"},
                "attachments": {"type": "array", "items": {"type": "object", "required": ["contentType", "content"]}},
            }},
            "triggerAuthenticationType": "All",
        },
    }})
}

/// Posts each card of the webhook's body with `parameters` saying where.
fn post_each(parameters: &Value) -> Value {
    json!({"Send_each_adaptive_card": {
        "runAfter": {},
        "type": "Foreach",
        "foreach": "@triggerOutputs()?['body']?['attachments']",
        "actions": {"Post_card_in_a_chat_or_channel": {
            "type": "OpenApiConnection",
            "inputs": {
                "parameters": parameters,
                "host": {"apiId": TEAMS_API, "connectionName": "shared_teams", "operationId": "PostCardToConversation"},
                "authentication": "@parameters('$authentication')",
            },
        }},
    }})
}

fn base_parameters() -> serde_json::Map<String, Value> {
    let mut parameters = serde_json::Map::new();
    parameters.insert(
        "$authentication".into(),
        json!({"defaultValue": {}, "type": "SecureObject"}),
    );
    parameters.insert("$connections".into(), json!({"defaultValue": {}, "type": "Object"}));
    parameters
}

fn channel_definition() -> Value {
    let mut parameters = base_parameters();
    parameters.insert("teams.group".into(), json!({"defaultValue": "", "type": "String"}));
    parameters.insert("teams.channel".into(), json!({"defaultValue": "", "type": "String"}));
    json!({
        "$schema": "https://schema.management.azure.com/providers/Microsoft.Logic/schemas/2016-06-01/workflowdefinition.json#",
        "contentVersion": "1.0.0.0",
        "parameters": parameters,
        "triggers": trigger(),
        "actions": post_each(&json!({
            "poster": "Flow bot",
            "location": "Channel",
            "body/recipient/groupId": "@parameters('teams.group')",
            "body/recipient/channelId": "@parameters('teams.channel')",
            "body/messageBody": "@item()?['content']",
        })),
        "outputs": {},
    })
}

fn chat_definition() -> Value {
    json!({
        "$schema": "https://schema.management.azure.com/providers/Microsoft.Logic/schemas/2016-06-01/workflowdefinition.json#",
        "contentVersion": "1.0.0.0",
        "parameters": base_parameters(),
        "triggers": trigger(),
        "actions": post_each(&json!({
            "poster": "Flow bot",
            "location": "Group chat",
            "body/recipient": "",
            "body/messageBody": "@item()?['content']",
        })),
        "outputs": {},
    })
}

fn channel_markup() -> Value {
    let id = CHANNEL;
    json!({"version": "1.0", "automation": {"template": {
        "id": id,
        "summary": format!("${{format(resources['{id}.summary'], context['parameters']['TeamsChannel'])}}"),
        "parameters": [
            {"id": "TeamsGroup", "typeName": "Teams.Group", "mode": "input"},
            {"id": "TeamsChannel", "typeName": "Teams.Channel", "mode": "input", "dependencies": ["TeamsGroup"]},
        ],
        "editorBindings": [
            {"parameterId": "TeamsGroup", "label": format!("${{resources['{id}.TeamsGroup.label']}}")},
            {"parameterId": "TeamsChannel"},
        ],
    }}, "dataBindings": {"Post_card_in_a_chat_or_channel": [
        {"source": "${context['parameters']['TeamsGroup']}",
         "target": "${json['inputs']['parameters']['body/recipient/groupId']}"},
        {"source": "${context['parameters']['TeamsChannel']}",
         "target": "${json['inputs']['parameters']['body/recipient/channelId']}"},
    ]}})
}

fn chat_markup() -> Value {
    let id = CHAT;
    json!({"version": "1.0", "automation": {"template": {
        "id": id,
        "summary": format!("${{format(resources['{id}.summary'], context['parameters']['TeamsChat'])}}"),
        "parameters": [{"id": "TeamsChat", "typeName": "Teams.Chat", "mode": "input"}],
        "editorBindings": [{"parameterId": "TeamsChat"}],
    }}, "dataBindings": {"Post_card_in_a_chat_or_channel": [
        {"source": "${context['parameters']['TeamsChat']}",
         "target": "${json['inputs']['parameters']['body/recipient']}"},
    ]}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filter_names_the_categories_it_wants_and_leaves_out() {
        let (some, none) = wanted(
            "properties/categoryNames/any(x:x eq 'microsoftteams_channel') and \
             properties/categoryNames/all(x:x ne 'reported')",
        );
        assert_eq!(some, ["microsoftteams_channel"]);
        assert_eq!(none, ["reported"]);
        assert_eq!(wanted("x eq 'it''s'").0, ["it's"]);
        assert_eq!(wanted("x eq 'open").0, Vec::<String>::new());
    }

    #[test]
    fn top_and_search_narrow_the_gallery() {
        let query = GalleryQuery {
            filter: Some("properties/categoryNames/any(x:x eq 'home_featured')".into()),
            search: None,
            top: Some(1),
        };
        assert_eq!(matching(&query).count(), 1);
        let query = GalleryQuery {
            search: Some("Rich Adaptive".into()),
            ..GalleryQuery::default()
        };
        assert_eq!(matching(&query).count(), 2);
    }
}
