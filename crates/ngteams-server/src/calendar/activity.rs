//! A meeting's items in the activity feeds of the people it invites (`docs/protocol/chat.md`, "The activity feed":
//! "A meeting item"): "<organizer> invited you", "updated", "canceled", each naming the meeting's time and the
//! event it opens. An item is told on the account's push channel as a new message of its `48:notifications`
//! stream.

use serde_json::{Value, json};

use super::repo::Event;
use super::wire::iso;
use crate::app::App;
use crate::db::users::User;

const FEED: &str = "48:notifications";

/// What happened to the meeting, as the Meet app's activity types name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Change {
    Created,
    Updated,
    Canceled,
}

/// The feed message of `change` to `event` by `actor`, for the account `to`, made at `at`.
fn item(app: &App, to: &str, change: Change, event: &Event, actor: &User, at: i64) -> Value {
    let fields: Value = serde_json::from_str(&event.body).unwrap_or_default();
    let place = if event.team_id.is_some() { "channel" } else { "private" };
    let subtype = match change {
        Change::Created => format!("{place}MeetingCreated"),
        Change::Updated => format!("{place}MeetingUpdated"),
        Change::Canceled => format!("{place}MeetingCanceled"),
    };
    let range = format!(
        "<DateTimeRange><StartDateTime>{}</StartDateTime><EndDateTime>{}</EndDateTime></DateTimeRange>",
        iso(event.start_ms),
        iso(event.end_ms)
    );
    let base = format!("{}/api/chatsvc/{}", app.config.origin(), app.config.region);
    let details = format!("{}/l/meeting/details/?eventId={}", app.config.origin(), event.id);
    let actor_mri = format!("8:orgid:{}", actor.id);
    let activity = json!({
        "activityType": "msGraph", "activitySubtype": subtype,
        "activityTimestamp": iso(at), "activityId": at,
        "sourceThreadId": "", "sourceMessageId": 1,
        "sourceUserId": actor_mri, "sourceUserImDisplayName": actor.display_name,
        "sourceAppDisplayName": "Calendar", "targetUserId": format!("8:orgid:{to}"),
        "messagePreview": fields["subject"], "messagePreviewTemplateOption": "hasText",
        "activityContext": {
            "attributedToActorId": actor_mri,
            "attributedToActor": json!({"displayName": actor.display_name}).to_string(),
            "location": json!({"Custom": range}).to_string(),
        },
        "customTapAction": details,
    });
    json!({
        "type": "Message", "id": at.to_string(), "version": at.to_string(), "sequenceId": at,
        "clientmessageid": at.to_string(), "messagetype": "Text", "contenttype": "Text", "content": "",
        "conversationid": FEED, "to": FEED, "threadtype": "streamofnotifications",
        "conversationLink": format!("{base}/v1/users/ME/conversations/{FEED}"),
        "from": format!("{base}/v1/users/ME/contacts/8:orgid:{to}"), "imdisplayname": "",
        "composetime": iso(at), "originalarrivaltime": iso(at),
        "properties": {"activity": activity, "isread": false, "messageUpdatePolicyValue": 1},
    })
}

/// Tells each of `people` but `actor` of `change` to `event` in their activity feed.
pub(super) async fn tell(app: &App, people: &[String], change: Change, event: &Event, actor: &User) {
    for (place, to) in people.iter().filter(|to| **to != actor.id).enumerate() {
        let at = crate::chatsvc::now() + i64::try_from(place).unwrap_or(0);
        let payload = json!({"resourceType": "NewMessage", "resource": item(app, to, change, event, actor, at)});
        let topic = format!("push:8:orgid:{to}");
        if app.fanout.publish(&topic, payload.to_string().into()).await.is_err() {
            tracing::warn!("calendar activity fan-out unavailable");
        }
    }
}
