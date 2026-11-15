use serde_json::{Value, json};

use super::{Caller, activity::Told, repo::Member};
use crate::app::App;

pub(super) async fn publish(app: &App, members: &[Member], kind: &str, resource: &Value) {
    send(
        app,
        members.iter().map(|member| member.user_id.as_str()),
        kind,
        resource,
    )
    .await;
}

/// Tells `people`, the thread's as the caller's write or moment of the clock found them, of something the caller
/// isn't told of itself: a read receipt, typing.
pub(super) async fn others(app: &App, caller: &Caller, people: &[String], kind: &str, resource: &Value) {
    let others = people.iter().map(String::as_str).filter(|person| *person != caller.id);
    send(app, others, kind, resource).await;
}

/// Tells a message's readers of it, and each account an activity item was written for of its item. Both lists are
/// the transaction's own: someone taken out of the thread while the request ran is in neither.
pub(super) async fn tell(app: &App, kind: &str, resource: &Value, readers: &[String], told: &[Told]) {
    send(app, readers.iter().map(String::as_str), kind, resource).await;
    for item in told {
        send(app, std::iter::once(item.user.as_str()), item.kind, &item.resource).await;
    }
}

async fn send(app: &App, users: impl Iterator<Item = &str>, kind: &str, resource: &Value) {
    let payload = json!({"resourceType": kind, "resource": resource}).to_string();
    for user in users {
        if app
            .fanout
            .publish(&format!("push:8:orgid:{user}"), payload.clone().into())
            .await
            .is_err()
        {
            tracing::warn!("chat event fan-out unavailable; conversation sync will catch up");
        }
    }
}

pub(super) async fn own(app: &App, caller: &Caller, kind: &str, resource: &Value) {
    let payload = json!({"resourceType": kind, "resource": resource}).to_string();
    if app
        .fanout
        .publish(&format!("push:{}", caller.mri()), payload.into())
        .await
        .is_err()
    {
        tracing::warn!("own chat event fan-out unavailable; conversation sync will catch up");
    }
}
