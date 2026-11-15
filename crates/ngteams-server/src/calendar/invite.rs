//! How a meeting is told to the people it invites besides their calendars: a message in the meeting's chat (in a
//! channel, the meeting's post) whose `properties.meeting` names it, as the service posts a channel's meeting
//! (`docs/protocol/calling.md`, "Schedule a meeting"). The message is changed in place when the meeting is, and a
//! cancellation is a message of its own after it.

use chrono::SecondsFormat;
use serde_json::{Map, Value, json};

use super::repo::Event;
use crate::app::App;
use crate::chatsvc::service::{self, Post, Property};
use crate::db::DbError;
use crate::db::users::User;

const CREATED: &str = "http://schema.skype.com/ScheduledMeetingCreated";
const CANCELLED: &str = "http://schema.skype.com/ScheduledMeetingCancelled";

/// What a message's `properties.meeting` says of `event`: JSON as text.
fn meeting(app: &App, event: &Event, link: &str, cancelled: bool) -> Value {
    let fields: Value = serde_json::from_str(&event.body).unwrap_or_default();
    let time = |ms: i64| {
        chrono::DateTime::from_timestamp_millis(ms)
            .unwrap_or(chrono::DateTime::UNIX_EPOCH)
            .to_rfc3339_opts(SecondsFormat::Secs, false)
    };
    let kind = if event.recurring {
        "RecurringMaster"
    } else if event.series_id.is_some() {
        "Occurrence"
    } else {
        "Single"
    };
    let mut info = json!({
        "startTime": time(event.start_ms), "endTime": time(event.end_ms),
        "location": super::wire::location(&fields, true),
        "exchangeId": event.id, "iCalUid": event.series_id.as_deref().unwrap_or(&event.id),
        "eventType": kind, "meetingType": 1,
    });
    if cancelled {
        info["isCancelled"] = json!(true);
    }
    let meeting = json!({
        "@type": if cancelled { CANCELLED } else { CREATED },
        "meetingtitle": title(&fields), "scheduledmeetinginfo": info,
        "organizerId": event.owner_id, "tenantId": app.issuer.tenant(), "meetingJoinUrl": link,
    });
    json!(meeting.to_string())
}

fn title(fields: &Value) -> &str {
    fields["subject"].as_str().unwrap_or_default()
}

/// Posts the meeting in `thread` for `organizer`; the message's id.
pub(super) async fn scheduled(
    app: &App,
    thread: &str,
    event: &Event,
    link: &str,
    organizer: &User,
    text: &str,
) -> Result<Option<i64>, DbError> {
    let mut properties = Map::new();
    properties.insert("meeting".into(), meeting(app, event, link, false));
    let post = Post {
        thread,
        author: organizer,
        kind: "RichText/Html",
        content: text,
        properties,
        under: None,
    };
    service::post(app, post).await
}

/// Posts a channel's meeting to its channel for who scheduled it. The meeting's link names its own post, so the
/// post is kept first and told once it names the link `link` gives for it.
pub(super) async fn posted(
    app: &App,
    event: &mut Event,
    organizer: &User,
    link: impl Fn(&Event) -> String,
) -> Result<(), DbError> {
    let Some(thread) = event.channel_id.clone() else {
        return Ok(());
    };
    let mut properties = Map::new();
    properties.insert("meeting".into(), meeting(app, event, "", false));
    let post = Post {
        thread: &thread,
        author: organizer,
        kind: "RichText/Html",
        content: "Scheduled a meeting",
        properties,
        under: None,
    };
    let Some(post) = service::post_untold(app, post).await? else {
        return Ok(());
    };
    super::repo::set_post(&app.db, &event.id, post).await?;
    event.post_id = Some(post);
    let set = Property {
        thread: &thread,
        message: post,
        author: organizer,
        name: "meeting",
        value: &meeting(app, event, &link(event), false),
        told_as: "NewMessage",
    };
    service::set_property(app, set).await?;
    Ok(())
}

/// Writes the meeting as it now is over the message that told it.
pub(super) async fn changed(
    app: &App,
    thread: &str,
    event: &Event,
    link: &str,
    organizer: &User,
) -> Result<(), DbError> {
    if let Some(post) = event.post_id {
        let set = Property {
            thread,
            message: post,
            author: organizer,
            name: "meeting",
            value: &meeting(app, event, link, false),
            told_as: "MessageUpdate",
        };
        service::set_property(app, set).await?;
    }
    Ok(())
}

/// Tells the thread that the meeting (or the one occurrence `event` is) is cancelled, `note` with it.
pub(super) async fn cancelled(
    app: &App,
    thread: &str,
    event: &Event,
    link: &str,
    organizer: &User,
    note: Option<&str>,
) -> Result<(), DbError> {
    let fields: Value = serde_json::from_str(&event.body).unwrap_or_default();
    let mut text = format!("The meeting \"{}\" is cancelled", title(&fields));
    if let Some(note) = note.map(str::trim).filter(|note| !note.is_empty()) {
        text = format!("{text}: {note}");
    }
    let mut properties = Map::new();
    properties.insert("meeting".into(), meeting(app, event, link, true));
    let post = Post {
        thread,
        author: organizer,
        kind: "Text",
        content: &text,
        properties,
        under: event.post_id,
    };
    service::post(app, post).await?;
    Ok(())
}
