//! A meeting's items in the activity feeds of the people it invites (`docs/protocol/chat.md`, "The activity feed":
//! "A meeting item"): "<organizer> invited you", "updated", "canceled", each naming the meeting's time and the
//! event it opens. The chat service keeps each item in the account's `48:notifications` stream and tells it on
//! the account's push channel.

use serde_json::{Value, json};

use super::repo::Event;
use super::wire::iso;
use crate::app::App;
use crate::chatsvc::service::{self, Activity};
use crate::db::DbError;
use crate::db::users::User;

/// What happened to the meeting, as the Meet app's activity types name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Change {
    Created,
    Updated,
    Canceled,
}

/// The feed message of `change` to `event` by `actor`, for the account `to`, under the id `at` its feed gives it.
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
    let properties = json!({"activity": activity, "isread": false, "messageUpdatePolicyValue": 1});
    service::stream_message(app, service::FEED, (at, &format!("8:orgid:{to}")), "", &properties)
}

/// Tells each of `people` but `actor` of `change` to `event` in their activity feed. An invitation names the
/// account itself; a change or a cancellation is not told to who muted the meeting's chat or channel.
pub(super) async fn tell(
    app: &App,
    people: &[String],
    change: Change,
    event: &Event,
    actor: &User,
) -> Result<(), DbError> {
    let people: Vec<String> = people.iter().filter(|to| **to != actor.id).cloned().collect();
    let told = Activity {
        people: &people,
        thread: event.meeting_chat.as_deref().or(event.channel_id.as_deref()),
        personal: change == Change::Created,
        item: &|to, id| item(app, to, change, event, actor, id),
    };
    service::tell_activity(app, told).await
}
