use serde_json::{Value, json};

use super::{Caller, activity::Told, repo::Member};
use crate::app::App;

pub(super) async fn publish(app: &App, caller: &Caller, members: &[Member], kind: &str, resource: &Value) {
    let include_actor = kind != "ReadReceipt" && resource["messagetype"].as_str() != Some("Control/Typing");
    let readers = members
        .iter()
        .map(|member| member.user_id.as_str())
        .filter(|member| include_actor || *member != caller.id);
    send(app, readers, kind, resource).await;
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
