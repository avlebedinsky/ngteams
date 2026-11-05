//! Workflows run from a message: a workflow whose trigger is Microsoft Teams' "For a selected message" (V2), started
//! on one chat message or channel post with the answers to the trigger's own input card. See
//! `docs/protocol/workflows.md`, "Workflows run from a message".
//!
//! Teams starts such a run through its Workflows bot; the bot then posts the trigger body below to the workflow's
//! Logic Flows runtime trigger. The native client posts the same body there itself, with its own API Hub token, so no
//! account credential goes to a bot. The body is built here from typed values, never taken from a view.
use serde_json::{Map, Value, json};

/// The trigger operation of a workflow run from a message.
pub const SELECTED_MESSAGE: &str = "ForASelectedMessageV2";

/// Who sent the message a workflow runs on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MessageSender {
    /// A person: their directory object id and name.
    User { id: String, name: String },
    /// An app or a bot: its app id and name.
    App { id: String, name: String },
    /// Nobody the message names.
    Unknown,
}

/// The message a workflow runs on, and who runs it: what the trigger's `teamsFlowRunContext` carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageRun {
    /// The message's id.
    pub id: String,
    /// The id of the post a channel message belongs to (its own for a post); none in a chat.
    pub reply_to: Option<String>,
    pub sender: MessageSender,
    /// The message's HTML, as the service holds it.
    pub html: String,
    /// Its text without markup.
    pub text: String,
    /// Teams' link to the message.
    pub link: String,
    /// The chat's or the channel's thread id.
    pub conversation: String,
    /// The team's thread id and its group's object id, for a channel message.
    pub team: Option<(String, String)>,
    /// The tenant's id, when known.
    pub tenant: Option<String>,
    /// The account running the workflow: its MRI, name and directory object id.
    pub actor: String,
    pub actor_name: String,
    pub actor_object_id: String,
    /// The local time zone's Windows id ("Turkey Standard Time").
    pub time_zone: String,
}

impl MessageRun {
    /// Teams' link to a message: a chat's carries its kind, a channel message names its post.
    #[must_use]
    pub fn link_to(conversation: &str, id: &str, reply_to: Option<&str>) -> String {
        match reply_to {
            Some(post) => format!("https://teams.cloud.microsoft/l/message/{conversation}/{id}?parentMessageId={post}"),
            None => format!(
                "https://teams.cloud.microsoft/l/message/{conversation}/{id}?context=%7B%22contextType%22:%22chat%22%7D"
            ),
        }
    }

    /// The trigger body: the run's context as the Workflows bot sends it, and the card's answers.
    #[must_use]
    pub fn body(&self, answers: &Map<String, Value>) -> Value {
        let (user, application) = match &self.sender {
            MessageSender::User { id, name } => (json!({"DisplayName": name, "Id": id}), Value::Null),
            MessageSender::App { id, name } => (Value::Null, json!({"DisplayName": name, "Id": id})),
            MessageSender::Unknown => (Value::Null, Value::Null),
        };
        let channel_data = json!({
            "EventType": null,
            "InvalidateInvokeCache": null,
            "Team": self.team.as_ref().map(|(team, group)| json!({"Id": team, "AadGroupId": group, "Name": null})),
            "Tenant": self.tenant.as_ref().map(|tenant| json!({"Id": tenant})),
            "Channel": self.team.as_ref().map(|_| json!({"Id": self.conversation, "Name": null})),
            "Notification": null,
            "Workflow": null,
            "OnBehalfOf": null,
            "FeedbackLoopEnabled": null,
        });
        json!({
            "teamsFlowRunContext": {
                "MessagePayload": {
                    "Id": self.id,
                    "From": {"Application": application, "Conversation": null, "Device": null, "User": user},
                    "Body": {"ContentType": "html", "Content": self.html, "PlainText": self.text},
                    "Subject": null,
                    "LinkToMessage": self.link,
                    "ReplyToId": self.reply_to,
                },
                "Id": format!("f:{}", activity_id()),
                "ChannelData": channel_data,
                "From": {
                    "Id": self.actor,
                    "Name": self.actor_name,
                    "Role": null,
                    "AadObjectId": self.actor_object_id,
                    "Properties": null,
                },
                "Conversation": {"Id": self.conversation, "Name": null, "IsGroup": true},
                "Locale": "en-US",
                "CommandContext": "message",
                "LocalTimezone": self.time_zone,
            },
            "cardOutputs": answers,
        })
    }
}

/// A fresh activity id, shaped as the bot's (`8-4-4-4-12` hex digits).
fn activity_id() -> String {
    let mut bytes = [0_u8; 16];
    // A clock-derived fallback keeps the id unique enough if the system source fails; it names no secret.
    if getrandom::fill(&mut bytes).is_err() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        bytes = stamp.to_le_bytes();
    }
    let hex = bytes.iter().fold(String::with_capacity(32), |mut hex, byte| {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
        hex
    });
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// The trigger of a workflow run from a message, by name, when its definition has exactly one and it is Teams'
/// "For a selected message".
#[must_use]
pub fn message_trigger(definition: &Value) -> Option<(&str, &Value)> {
    let triggers = definition.get("triggers")?.as_object()?;
    let (name, trigger) = triggers.iter().next().filter(|_| triggers.len() == 1)?;
    let operation = trigger
        .pointer("/inputs/operationId")
        .or_else(|| trigger.pointer("/metadata/flowSystemMetadata/swaggerOperationId"))
        .and_then(Value::as_str);
    (operation == Some(SELECTED_MESSAGE)).then_some((name.as_str(), trigger))
}

/// Whether a flow list entry's summary (`properties.definitionSummary`) names Teams' "For a selected message" trigger.
#[must_use]
pub fn summary_runs_from_message(properties: &Value) -> bool {
    properties
        .pointer("/definitionSummary/triggers")
        .and_then(Value::as_array)
        .is_some_and(|triggers| {
            triggers.iter().any(|trigger| {
                trigger.get("swaggerOperationId").and_then(Value::as_str) == Some(SELECTED_MESSAGE)
                    || trigger
                        .pointer("/metadata/flowSystemMetadata/swaggerOperationId")
                        .and_then(Value::as_str)
                        == Some(SELECTED_MESSAGE)
            })
        })
}

/// The input card a message trigger asks its questions with (`inputsAdaptiveCard`), parsed.
#[must_use]
pub fn input_card(trigger: &Value) -> Option<Value> {
    let text = trigger
        .pointer("/inputs/parameters/inputsAdaptiveCard")
        .or_else(|| trigger.pointer("/metadata/flowSystemMetadata/inputsAdaptiveCard"))
        .and_then(Value::as_str)?;
    serde_json::from_str(text).ok()
}

/// The card's inputs, in its order: each `Input.*` element of its body, at any depth.
#[must_use]
pub fn card_inputs(card: &Value) -> Vec<&Value> {
    fn walk<'a>(element: &'a Value, found: &mut Vec<&'a Value>) {
        match element {
            Value::Array(items) => items.iter().for_each(|item| walk(item, found)),
            Value::Object(object) => {
                let input = object
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| kind.starts_with("Input."));
                if input && object.get("id").and_then(Value::as_str).is_some() {
                    found.push(element);
                    return;
                }
                for key in ["body", "items", "columns"] {
                    if let Some(inner) = object.get(key) {
                        walk(inner, found);
                    }
                }
            }
            _ => {}
        }
    }
    let mut found = Vec::new();
    walk(card.get("body").unwrap_or(&Value::Null), &mut found);
    found
}

/// Whether answers fit a card: each names one of its inputs, every value is a string (as Teams sends a card's
/// answers), and every required input has a non-empty one.
#[must_use]
pub fn answers_fit(card: &Value, answers: &Map<String, Value>) -> bool {
    let inputs = card_inputs(card);
    let id = |input: &Value| input.get("id").and_then(Value::as_str).map(str::to_owned);
    let ids: Vec<String> = inputs.iter().filter_map(|input| id(input)).collect();
    answers
        .iter()
        .all(|(key, value)| ids.contains(key) && value.is_string())
        && inputs.iter().all(|input| {
            let required = input.get("isRequired").and_then(Value::as_bool) == Some(true);
            !required
                || id(input)
                    .and_then(|key| answers.get(&key).and_then(Value::as_str).map(str::to_owned))
                    .is_some_and(|value| !value.trim().is_empty())
        })
}
