//! A meeting's call (`docs/protocol/calling.md`, "Joining a meeting"): a group call in the meeting's chat that nobody
//! rings. Whoever comes first starts it with its join, everyone after joins it, and its organizer is the meeting's
//! (the calendar's [`crate::calendar::Meeting`]), whoever started the call.

use axum::Json;
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use super::group::{self, Opened, Start};
use super::repo::{self, Key};
use super::state::Meeting;
use super::wire;
use super::{Caller, Failure, call_events, carry_out, needed_links};
use crate::app::App;
use crate::db::DbError;

/// Whether `account` may come into the meeting whose chat is `chat`: someone in its chat (its organizer and whoever
/// it invites).
async fn may_join(app: &App, chat: &str, account: &str) -> Result<bool, DbError> {
    let members = call_events::members(app, chat).await?;
    Ok(members.is_some_and(|members| members.iter().any(|member| member == account)))
}

/// `POST epconv` with a meeting's chat and organizer: the call of the meeting is joined when it runs, else started
/// with the joiner in it. Nobody rings.
pub(super) async fn start(app: &App, caller: &Caller, asked: Start<'_>, body: &Value) -> Result<Response, Failure> {
    let nodes = group::nodes(app)?;
    let meeting = crate::calendar::meeting_of(&app.db, asked.chat)
        .await?
        .ok_or(super::state::Refusal::Missing)?;
    if !may_join(app, asked.chat, &caller.id).await? {
        return Err(super::state::Refusal::Missing.into());
    }
    if let Some(running) = repo::running_in(&app.db, asked.chat).await? {
        return group::join(app, caller, Key::Id(&running.call.id), &running.call, body).await;
    }
    let offer = group::sdp(Some(asked.offer))?;
    let (endpoint, links) = needed_links(app, caller, body).await?;
    if !["acceptance", "end"].iter().all(|name| links.contains_key(*name)) {
        return Err(super::state::Refusal::Bad.into());
    }
    let mut from = super::ops::party(&caller.id, &caller.name, asked.from);
    from.keep(&endpoint, links);
    from.active = Some(endpoint);
    let mut call = group::new_call(asked.chat, from);
    call.meeting = Some(Meeting {
        organizer: meeting.organizer_id,
        ..Meeting::default()
    });
    let out = match group::open(app, nodes.as_ref(), &mut call, &offer, &[]).await? {
        Opened::Made { out, .. } => out,
        Opened::Running(running) => {
            return group::join(app, caller, Key::Id(&running.id), &running, body).await;
        }
    };
    let origin = app.config.origin();
    let conversation = wire::conversation_url(&origin, &call.conversation, None);
    super::ops::tell_chat_started(app, caller, &call, Some(&conversation)).await;
    carry_out(app, &call, out).await;
    if let (Some(home), Some(leg)) = (call.media.as_ref(), call.party(super::state::Seat::CALLER).leg.as_ref()) {
        group::seed_leg(app, home, &call.id, &leg.id, Some(asked.offer)).await;
    }
    Ok(Json(wire::conversation(&origin, &app.config.region, &call)).into_response())
}
