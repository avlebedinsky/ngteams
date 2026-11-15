//! A workflow's webhook: `POST <origin>/workflows/automations/direct/workflows/<id>/triggers/<trigger>/paths/invoke
//! ?api-version=1&sp=…&sv=1.0&sig=<secret>`, as Power Automate's own link reads. It takes no account token: the
//! workflow's random secret in `sig` is the credential, compared by digest, and kept out of every log (the route has
//! its own trace span, which names the path only). A wrong secret and a deleted workflow look the same: 404.
//!
//! The body is the Teams Workflows payload, `{"type": "message", "attachments": [{"contentType":
//! "application/vnd.microsoft.card.adaptive", "content": {…}}]}` (each card is posted as one message, as the
//! template's loop does), a bare Adaptive Card, or `{"text": "…"}`. Each is posted as the Workflows bot through the
//! chat service, while one of the workflow's owners may still post there. The answer is Power Automate's 202 and a
//! run is kept for the run history; a payload the client's card renderer can't take is refused with 400.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, RawQuery, State};
use axum::http::header::RETRY_AFTER;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

use super::definition::{self, Target};
use super::{Error, repo};
use crate::app::App;
use crate::config::Config;
use crate::db::{DbError, users};

/// The run history's one action: the post.
pub(crate) const POST_ACTION: &str = "Post_card_in_a_chat_or_channel";
const PREFIX: &str = "/workflows/automations/direct/workflows";
const ADAPTIVE: &str = "application/vnd.microsoft.card.adaptive";
const CARD_KINDS: [&str; 3] = [
    ADAPTIVE,
    "application/vnd.microsoft.card.hero",
    "application/vnd.microsoft.card.thumbnail",
];
/// The most cards one post may carry.
const MAX_CARDS: usize = 10;
/// How deep a card's containers may nest: deeper, the client's renderer leaves them out.
const MAX_DEPTH: usize = 16;
const TIMEOUT: Duration = Duration::from_secs(30);

/// A new webhook secret: 256 random bits.
pub(super) fn secret() -> Result<String, aws_lc_rs::error::Unspecified> {
    let mut bytes = [0_u8; 32];
    aws_lc_rs::rand::fill(&mut bytes)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// `listCallbackUrl`'s answer: the workflow's webhook link.
pub(super) fn callback(config: &Config, flow: &repo::Workflow, trigger: &str) -> Value {
    let base = format!(
        "{}{PREFIX}/{}/triggers/{trigger}/paths/invoke",
        config.origin(),
        flow.id
    );
    let run = format!("/triggers/{trigger}/run");
    let queries = [
        ("api-version", "1"),
        ("sp", run.as_str()),
        ("sv", "1.0"),
        ("sig", flow.secret.as_str()),
    ];
    let mut link = url::Url::parse(&base).unwrap_or_else(|_| unreachable!("the origin is a URL"));
    link.query_pairs_mut().extend_pairs(queries);
    let queries: Map<String, Value> = queries
        .iter()
        .map(|(key, value)| ((*key).to_owned(), json!(value)))
        .collect();
    json!({"response": {"value": link.as_str(), "method": "POST", "basePath": base, "queries": queries}})
}

/// The webhook route: outside the node's request trace, which would log its query.
pub(crate) fn router(config: &Config) -> Router<Arc<App>> {
    if !config.workflows.enabled {
        return Router::new();
    }
    Router::new()
        .route(&format!("{PREFIX}/{{flow}}/triggers/{{trigger}}/paths/invoke"), post(invoke))
        .layer(DefaultBodyLimit::max(config.workflows.max_body_bytes))
        .layer(TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, TIMEOUT))
        .layer(TraceLayer::new_for_http().make_span_with(|request: &Request<_>| {
            tracing::debug_span!("request", method = %request.method(), path = %request.uri().path())
        }))
}

/// A refusal, in Logic Apps' error shape.
fn refused(status: StatusCode, code: &'static str) -> Response {
    Error::Code(status, code).into_response()
}

async fn invoke(
    State(app): State<Arc<App>>,
    Path((flow, trigger)): Path<(String, String)>,
    RawQuery(query): RawQuery,
    body: Bytes,
) -> Response {
    match run(&app, &flow, &trigger, query.as_deref(), &body).await {
        Ok(response) => response,
        Err(error) => Error::Database(error).into_response(),
    }
}

async fn run(app: &App, flow: &str, trigger: &str, query: Option<&str>, body: &[u8]) -> Result<Response, DbError> {
    let sig = query
        .into_iter()
        .flat_map(|query| url::form_urlencoded::parse(query.as_bytes()))
        .find(|(key, _)| key == "sig");
    let Some(flow) = repo::workflow(&app.db, flow).await? else {
        return Ok(refused(StatusCode::NOT_FOUND, "WorkflowNotFound"));
    };
    let read = definition::read(&super::flows::stored_definition(&flow));
    let signed = sig.is_some_and(|(_, sig)| Sha256::digest(sig.as_bytes()) == Sha256::digest(flow.secret.as_bytes()));
    let Some((_, target)) = read.filter(|(name, _)| signed && name == trigger) else {
        return Ok(refused(StatusCode::NOT_FOUND, "WorkflowNotFound"));
    };
    if !flow.enabled {
        return Ok(refused(StatusCode::CONFLICT, "WorkflowTriggerIsNotEnabled"));
    }
    if !app.workflow_limit.take(&flow.id).await {
        let mut response = refused(StatusCode::TOO_MANY_REQUESTS, "WorkflowRequestsThrottled");
        response
            .headers_mut()
            .insert(RETRY_AFTER, axum::http::HeaderValue::from_static("60"));
        return Ok(response);
    }
    let started = super::now();
    let Some(messages) = messages(body) else {
        keep(app, &flow, started, Some("InvalidRequestContent")).await?;
        return Ok(refused(StatusCode::BAD_REQUEST, "InvalidRequestContent"));
    };
    let outcome = deliver(app, &flow, &target, &messages).await?;
    keep(app, &flow, started, outcome.err()).await?;
    Ok(StatusCode::ACCEPTED.into_response())
}

/// Posts each message where the workflow posts, while one of its owners may post there; the failing action's code.
async fn deliver(
    app: &App,
    flow: &repo::Workflow,
    target: &Target,
    messages: &[Message],
) -> Result<Result<(), &'static str>, DbError> {
    if !owner_may_post(app, flow, target).await? {
        return Ok(Err("Forbidden"));
    }
    for message in messages {
        let (kind, content) = match message {
            Message::Card(card) => ("RichText/Media_Card", card_content(card, &app.config.origin())),
            Message::Text(text) => ("RichText/Html", format!("<p>{}</p>", escape(text))),
        };
        let posted = crate::chatsvc::service::post_as_app(
            app,
            crate::chatsvc::service::AppPost {
                thread: target.thread(),
                app: super::WORKFLOWS_BOT,
                name: super::BOT_NAME,
                kind,
                content: &content,
                properties: Map::new(),
            },
        )
        .await?;
        if posted.is_none() {
            return Ok(Err("NotFound"));
        }
    }
    Ok(Ok(()))
}

async fn owner_may_post(app: &App, flow: &repo::Workflow, target: &Target) -> Result<bool, DbError> {
    for owner in repo::owners(&app.db, &flow.id).await? {
        let enabled = users::by_id(&app.db, &owner)
            .await?
            .is_some_and(|user| !user.disabled && user.deleted_at.is_none());
        if enabled && super::flows::sees(app, &owner, target).await? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Keeps the run for the history: failed with `error`, or succeeded.
async fn keep(app: &App, flow: &repo::Workflow, started: i64, error: Option<&str>) -> Result<(), DbError> {
    let run = repo::Run {
        workflow_id: flow.id.clone(),
        id: uuid::Uuid::new_v4().simple().to_string(),
        started_at: started,
        ended_at: super::now(),
        status: if error.is_some() { "Failed" } else { "Succeeded" }.to_owned(),
        error: error.map(str::to_owned),
    };
    repo::record(&app.db, &run, app.config.workflows.runs_kept).await
}

/// One message a post makes.
#[derive(Debug, PartialEq)]
enum Message {
    /// A card attachment (`contentType`, `content`).
    Card(Value),
    Text(String),
}

/// The messages a body asks for, or `None` when it is none of the forms the template takes or a card is one the
/// client can't draw.
fn messages(body: &[u8]) -> Option<Vec<Message>> {
    let body: Value = serde_json::from_slice(body).ok()?;
    let body = body.as_object()?;
    if let Some(attachments) = body.get("attachments") {
        let attachments = attachments.as_array()?;
        if attachments.is_empty() || attachments.len() > MAX_CARDS {
            return None;
        }
        return attachments
            .iter()
            .map(|attachment| {
                let kind = attachment.get("contentType")?.as_str()?;
                let content = attachment.get("content")?;
                (CARD_KINDS.contains(&kind) && card(kind, content))
                    .then(|| Message::Card(json!({"contentType": kind, "content": content})))
            })
            .collect();
    }
    if body.get("type").and_then(Value::as_str) == Some("AdaptiveCard") {
        let card = Value::Object(body.clone());
        return self::card(ADAPTIVE, &card)
            .then(|| vec![Message::Card(json!({"contentType": ADAPTIVE, "content": card}))]);
    }
    let text = body.get("text")?.as_str()?.trim();
    (!text.is_empty()).then(|| vec![Message::Text(text.to_owned())])
}

/// Whether the client's card renderer takes `content` as a card of `kind`.
fn card(kind: &str, content: &Value) -> bool {
    if kind != ADAPTIVE {
        return content.is_object();
    }
    content.get("type").and_then(Value::as_str) == Some("AdaptiveCard")
        && elements(content.get("body"), 0)
        && actions(content.get("actions"), 0)
}

/// Whether every element of a body or container is an object of a named type, nested no deeper than the renderer
/// reads.
fn elements(elements: Option<&Value>, depth: usize) -> bool {
    let Some(elements) = elements else {
        return true;
    };
    depth < MAX_DEPTH
        && elements.as_array().is_some_and(|elements| {
            elements.iter().all(|element| {
                element.get("type").and_then(Value::as_str).is_some()
                    && elements_of(element, depth + 1)
                    && actions(element.get("actions"), depth + 1)
                    && element
                        .get("selectAction")
                        .is_none_or(|action| action.get("type").and_then(Value::as_str).is_some())
            })
        })
}

/// The elements an element holds: a container's items, a column set's columns' items, a table's cells' items.
fn elements_of(element: &Value, depth: usize) -> bool {
    let nested = |key: &str| {
        element.get(key).is_none_or(|parts| {
            parts
                .as_array()
                .is_some_and(|parts| parts.iter().all(|part| elements(part.get("items"), depth + 1)))
        })
    };
    elements(element.get("items"), depth)
        && nested("columns")
        && element.get("rows").is_none_or(|rows| {
            rows.as_array().is_some_and(|rows| {
                rows.iter().all(|row| {
                    row.get("cells")
                        .is_none_or(|_| elements_of(&json!({"columns": row["cells"]}), depth + 1))
                })
            })
        })
}

/// Whether every action is an object of a named type; a shown card's own card is a card.
fn actions(actions: Option<&Value>, depth: usize) -> bool {
    let Some(actions) = actions else {
        return true;
    };
    depth < MAX_DEPTH
        && actions.as_array().is_some_and(|actions| {
            actions.iter().all(|action| {
                action.get("type").and_then(Value::as_str).is_some()
                    && action.get("card").is_none_or(|card| {
                        elements(card.get("body"), depth + 1) && self::actions(card.get("actions"), depth + 1)
                    })
            })
        })
}

/// A card message's content, as the chat service carries a bot's: a `SWIFT.1` object whose text is a fallback and
/// whose `Swift` holds the base64 JSON of the card's activity.
fn card_content(card: &Value, origin: &str) -> String {
    let activity = json!({"type": "message/card", "attachments": [card]});
    let encoded = STANDARD.encode(activity.to_string());
    format!(
        "<URIObject type=\"SWIFT.1\"><Title>Card</Title><Description>Card - access it on {}.</Description>\
         <Swift b64=\"{encoded}\"/></URIObject>",
        escape(origin)
    )
}

fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            c => escaped.push(c),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adaptive(body: &Value) -> Value {
        json!({"type": "AdaptiveCard", "version": "1.5", "body": body})
    }

    #[test]
    fn the_template_s_forms_read_as_messages() {
        let payload = json!({"type": "message", "attachments": [
            {"contentType": ADAPTIVE, "content": adaptive(&json!([{"type": "TextBlock", "text": "Up"}]))},
            {"contentType": "application/vnd.microsoft.card.hero", "content": {"title": "Hero"}},
        ]});
        let read = messages(payload.to_string().as_bytes()).unwrap();
        assert_eq!(read.len(), 2);
        let bare = adaptive(&json!([]));
        assert_eq!(
            messages(bare.to_string().as_bytes()),
            Some(vec![Message::Card(json!({"contentType": ADAPTIVE, "content": bare}))])
        );
        assert_eq!(
            messages(br#"{"text": " Deployed "}"#),
            Some(vec![Message::Text("Deployed".into())])
        );
        for refused in [
            &b"not json"[..],
            br"[]",
            br#"{"text": "  "}"#,
            br#"{"attachments": []}"#,
            br#"{"attachments": {}}"#,
            br#"{"attachments": [{"contentType": "application/vnd.microsoft.teams.card.o365connector", "content": {}}]}"#,
            br#"{"attachments": [{"contentType": "application/vnd.microsoft.card.adaptive", "content": {"type": "Other"}}]}"#,
            br#"{"attachments": [{"contentType": "application/vnd.microsoft.card.hero", "content": "x"}]}"#,
            br#"{"type": "AdaptiveCard", "body": [{"text": "no type"}]}"#,
            br#"{"other": 1}"#,
        ] {
            assert_eq!(messages(refused), None, "{}", String::from_utf8_lossy(refused));
        }
        let many: Vec<Value> = (0..=MAX_CARDS)
            .map(|_| json!({"contentType": ADAPTIVE, "content": adaptive(&json!([]))}))
            .collect();
        assert_eq!(messages(json!({"attachments": many}).to_string().as_bytes()), None);
    }

    #[test]
    fn a_card_s_containers_columns_tables_and_actions_are_checked_to_the_renderer_s_depth() {
        let good = adaptive(&json!([
            {"type": "Container", "items": [{"type": "TextBlock", "text": "a"}],
             "selectAction": {"type": "Action.OpenUrl", "url": "https://example.test"}},
            {"type": "ColumnSet", "columns": [{"type": "Column", "items": [{"type": "Image", "url": "https://example.test/i.png"}]}]},
            {"type": "Table", "rows": [{"type": "TableRow", "cells": [{"type": "TableCell", "items": [{"type": "TextBlock", "text": "c"}]}]}]},
            {"type": "ActionSet", "actions": [{"type": "Action.ShowCard", "card": {"type": "AdaptiveCard", "body": [{"type": "TextBlock", "text": "s"}]}}]},
        ]));
        assert!(card(ADAPTIVE, &good));
        let mut deep = json!([{"type": "TextBlock", "text": "deep"}]);
        for _ in 0..MAX_DEPTH {
            deep = json!([{"type": "Container", "items": deep}]);
        }
        assert!(!card(ADAPTIVE, &adaptive(&deep)));
        assert!(!card(
            ADAPTIVE,
            &adaptive(&json!([{"type": "ColumnSet", "columns": {}}]))
        ));
        assert!(!card(
            ADAPTIVE,
            &adaptive(&json!([{"type": "Table", "rows": [{"cells": [{"items": [{}]}]}]}]))
        ));
        assert!(!card(
            ADAPTIVE,
            &adaptive(&json!([{"type": "Container", "selectAction": {}}]))
        ));
        let mut bad_action = adaptive(&json!([]));
        bad_action["actions"] = json!([{"title": "untyped"}]);
        assert!(!card(ADAPTIVE, &bad_action));
        assert!(!card(ADAPTIVE, &adaptive(&json!({}))));
    }

    #[test]
    fn a_card_message_carries_its_card_in_its_swift_and_escapes_its_text() {
        let card = json!({"contentType": ADAPTIVE, "content": adaptive(&json!([]))});
        let content = card_content(&card, "https://own.example.test");
        let encoded = content.split("b64=\"").nth(1).unwrap().split('"').next().unwrap();
        let activity: Value = serde_json::from_slice(&STANDARD.decode(encoded).unwrap()).unwrap();
        assert_eq!(activity["attachments"][0], card);
        assert_eq!(escape("<a href=\"x\">&'"), "&lt;a href=&quot;x&quot;&gt;&amp;&#39;");
    }

    #[test]
    fn secrets_are_long_and_never_repeat() {
        let one = secret().unwrap();
        assert_eq!(one.len(), 43);
        assert_ne!(one, secret().unwrap());
    }
}
