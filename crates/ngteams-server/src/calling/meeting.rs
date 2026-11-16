//! A meeting's call (`docs/protocol/calling.md`, "Joining a meeting", "The lobby"): a group call in the meeting's chat
//! that nobody rings. Whoever comes first starts it with its join, everyone after joins it, and its organizer is the
//! meeting's (the calendar's [`crate::calendar::Meeting`]), whoever started the call.
//!
//! - **The lobby** is the meeting's option `AutoAdmittedUsers`, read at each join: whom it doesn't let in waits. A
//!   joiner that waits keeps its offer in its seat and has no media until a host lets it in; its leg is then made
//!   with that offer and its acceptance comes as a joiner's does, so the client goes from the lobby straight into the
//!   call (no lobby media session, no retarget).
//! - **Hosts** (the organizer and the presenters in the call) let people in (`admit`) and turn them away
//!   (`removeParticipant`).

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use super::group::{self, Opened, Start};
use super::repo::{self, Key};
use super::state::{At, Call, Controls, Meeting, Out, Party, Refusal, Seat, Status};
use super::wire;
use super::{Caller, Failure, call_events, carry_out, needed_links, now_ms, step, step_any};
use crate::app::App;
use crate::db::DbError;
use crate::db::users;

/// Where a joiner goes, as the meeting's lobby option says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Admission {
    In,
    Lobby,
}

/// Where `account` goes when it joins the meeting `meeting` organizes in `chat`: its organizer always in; everyone
/// else as `AutoAdmittedUsers` says. Every account of the server is of the company, so only the invited (the meeting
/// chat's people) and the organizer alone are told apart.
async fn admission(app: &App, chat: &str, organizer: &str, account: &str) -> Result<Admission, DbError> {
    if account == organizer {
        return Ok(Admission::In);
    }
    let lets_in = crate::calendar::option(&app.db, chat, "AutoAdmittedUsers").await?;
    let invited = || async {
        let members = call_events::members(app, chat).await?;
        Ok::<_, DbError>(members.is_some_and(|members| members.iter().any(|member| member == account)))
    };
    let admitted = match lets_in.as_str() {
        "OrganizerOnly" => false,
        "InvitedUsers" => invited().await?,
        _ => true,
    };
    Ok(if admitted { Admission::In } else { Admission::Lobby })
}

/// The options saved for the meeting whose chat is `chat` that bind its call.
async fn controls(app: &App, chat: &str) -> Result<Controls, DbError> {
    let option = |name| crate::calendar::option(&app.db, chat, name);
    Ok(Controls {
        lobby: option("AutoAdmittedUsers").await?,
        presenters: option("PresenterOption").await?,
        restrictions: option("AttendeeRestrictions").await?,
    })
}

/// The organizer saved the options of the meeting whose chat is `chat`: the meeting's call running in it, if any,
/// goes by them from now on and tells everyone in it.
///
/// # Errors
/// A database failure.
pub(crate) async fn options_saved(app: &App, chat: &str) -> Result<(), DbError> {
    let Some(running) = repo::running_in(&app.db, chat).await? else {
        return Ok(());
    };
    if running.call.meeting.is_none() {
        return Ok(());
    }
    let controls = controls(app, chat).await?;
    let changed = step_any(app, Key::Id(&running.call.id), |call, at, out| {
        call.set_controls(controls.clone(), at, out);
        Ok(())
    })
    .await;
    match changed {
        Err(Failure::Database(error)) => Err(error),
        _ => Ok(()),
    }
}

/// `POST epconv` with a meeting's chat and organizer: the call of the meeting is joined when it runs, else started
/// with the joiner in it; a joiner the lobby holds waits there instead. Nobody rings.
pub(super) async fn start(app: &App, caller: &Caller, asked: Start<'_>, body: &Value) -> Result<Response, Failure> {
    let nodes = group::nodes(app)?;
    let meeting = crate::calendar::meeting_of(&app.db, asked.chat)
        .await?
        .ok_or(Refusal::Missing)?;
    let admitted = admission(app, asked.chat, &meeting.organizer_id, &caller.id).await?;
    if let Some(running) = repo::running_in(&app.db, asked.chat).await? {
        return enter(app, caller, &running.call, admitted, body).await;
    }
    let offer = group::sdp(Some(asked.offer))?;
    let (endpoint, links) = needed_links(app, caller, body).await?;
    if !["acceptance", "end"].iter().all(|name| links.contains_key(*name)) {
        return Err(Refusal::Bad.into());
    }
    let mut from = super::ops::party(&caller.id, &caller.name, asked.from);
    from.keep(&endpoint, links);
    from.active = Some(endpoint);
    let mut call = group::new_call(asked.chat, from);
    call.meeting = Some(Meeting {
        organizer: meeting.organizer_id,
        controls: controls(app, asked.chat).await?,
        ..Meeting::default()
    });
    let opened = match admitted {
        Admission::In => group::open(app, nodes.as_ref(), &mut call, &offer, &[]).await?,
        Admission::Lobby => open_waiting(app, &mut call, asked.offer).await?,
    };
    let out = match opened {
        Opened::Made { out, .. } => out,
        Opened::Running(running) => return enter(app, caller, &running, admitted, body).await,
    };
    let origin = app.config.origin();
    let conversation = wire::conversation_url(&origin, &call.conversation, None);
    super::ops::tell_chat_started(app, caller, &call, Some(&conversation)).await;
    carry_out(app, &call, out).await;
    if let (Some(home), Some(leg)) = (call.media.as_ref(), call.party(Seat::CALLER).leg.as_ref()) {
        group::seed_leg(app, home, &call.id, &leg.id, Some(asked.offer)).await;
    }
    Ok(Json(wire::conversation(&origin, &app.config.region, &call)).into_response())
}

/// Into the running meeting's call `call`: a join, or a wait in its lobby. A plain group call in the chat is no
/// meeting's: it is refused, not joined without a lobby or hosts.
async fn enter(
    app: &App,
    caller: &Caller,
    call: &Call,
    admitted: Admission,
    body: &Value,
) -> Result<Response, Failure> {
    if call.meeting.is_none() {
        return Err(Refusal::Forbidden.into());
    }
    let key = Key::Id(&call.id);
    match admitted {
        Admission::In => group::join(app, caller, key, call, body).await,
        Admission::Lobby => wait(app, caller, key, body).await,
    }
}

/// Stores a meeting's call whose first joiner waits in its lobby: on a media node, with no leg yet. Another start in
/// the chat that was first is the call to wait in.
async fn open_waiting(app: &App, call: &mut Call, offer: &Value) -> Result<Opened, Failure> {
    let home = group::nodes(app)?.pick().await?;
    call.media = Some(home);
    let origin = app.config.origin();
    let at = At {
        origin: &origin,
        now: now_ms(),
    };
    let mut out = Out::default();
    call.started_waiting(offer, at, &mut out);
    match repo::insert(&app.db, call).await {
        Ok(()) => Ok(Opened::Made {
            out,
            offers: Vec::new(),
        }),
        Err(DbError::Conflict) => {
            let chat = call.chat.as_deref().unwrap_or_default();
            let running = repo::running_in(&app.db, chat).await?.ok_or(Failure::Busy)?;
            Ok(Opened::Running(Box::new(running.call)))
        }
        Err(error) => Err(error.into()),
    }
}

/// A joiner the lobby holds: it waits with its offer, answered with the conversation (which lists it waiting).
async fn wait(app: &App, caller: &Caller, key: Key<'_>, body: &Value) -> Result<Response, Failure> {
    let offer = body
        .pointer("/callInvitation/mediaContent")
        .filter(|offer| offer.is_object())
        .ok_or(Refusal::Bad)?;
    let (endpoint, links) = needed_links(app, caller, body).await?;
    if !["acceptance", "end"].iter().all(|name| links.contains_key(*name)) {
        return Err(Refusal::Bad.into());
    }
    let named = body
        .pointer("/participants/from")
        .filter(|from| from.is_object())
        .ok_or(Refusal::Bad)?;
    let account = super::ops::party(&caller.id, &caller.name, named);
    let region = &app.config.region;
    let answer = step_any(app, key, |call, at, out| {
        call.wait(&account, (&endpoint, links.clone()), offer, at, out)?;
        Ok(wire::conversation(at.origin, region, call))
    })
    .await?;
    Ok(Json(answer).into_response())
}

/// The accounts a lobby decision names (`participants.to`), by their own ids.
fn named(body: &Value) -> Result<Vec<String>, Failure> {
    let to = body
        .pointer("/participants/to")
        .and_then(Value::as_array)
        .ok_or(Refusal::Bad)?;
    Ok(to
        .iter()
        .filter_map(|person| person.get("id").and_then(Value::as_str))
        .filter_map(|id| id.strip_prefix("8:orgid:"))
        .map(str::to_owned)
        .collect())
}

/// `POST conv/<id>/admit` of a host: each person it names who waits in the lobby comes into the call, its leg made with
/// the offer it waited with.
pub(super) async fn admit(app: &App, caller: &Caller, key: Key<'_>, body: &Value) -> Result<Response, Failure> {
    let kept = repo::find(&app.db, key).await?.ok_or(Refusal::Missing)?;
    let seat = kept.call.seat_of(&caller.id).ok_or(Refusal::Missing)?;
    kept.call.hosts(seat)?;
    for id in named(body)? {
        let waiting = kept
            .call
            .seat_of(&id)
            .filter(|seat| kept.call.party(*seat).status == Status::Lobby);
        if let Some(waiting) = waiting {
            let party = kept.call.party(waiting).clone();
            let_in(app, key, &kept.call, &party).await?;
        }
    }
    Ok(StatusCode::ACCEPTED.into_response())
}

/// `party`, waiting in the lobby of `call`, comes in: its seat reserved with a new leg, the leg made on the media node
/// with its offer, then it joins on it from the endpoint it waits on.
async fn let_in(app: &App, key: Key<'_>, call: &Call, party: &Party) -> Result<(), Failure> {
    let offer = group::sdp(party.waiting.as_ref())?;
    let endpoint = party.active.clone().ok_or(Refusal::Gone)?;
    let links = party
        .endpoints
        .iter()
        .find(|known| known.endpoint == endpoint)
        .map(|known| known.links.clone())
        .unwrap_or_default();
    let leg = uuid::Uuid::new_v4().to_string();
    let reserved = step_any(app, key, |call, _, _| {
        let seat = call.seat_of(&party.id).ok_or(Refusal::Gone)?;
        if call.party(seat).status != Status::Lobby {
            return Err(Refusal::Gone);
        }
        call.reserve(party, &leg)
    })
    .await?;
    let home = group::home_of(call)?;
    let made = group::nodes(app)?
        .put_leg(home, &call.id, &leg, reserved.sources, Some(&offer))
        .await;
    let answer = match made {
        Ok(answer) => answer,
        Err(error) => {
            group::unreserve(app, key, &reserved, &leg).await;
            return Err(error.into());
        }
    };
    group::seed_leg(app, home, &call.id, &leg, party.waiting.as_ref()).await;
    let joined = step_any(app, key, |call, at, out| {
        call.join(
            reserved.seat,
            (&leg, &endpoint, links.clone()),
            &group::content(&answer),
            at,
            out,
        )
    })
    .await;
    if let Err(failure) = joined {
        group::unreserve(app, key, &reserved, &leg).await;
        return Err(failure);
    }
    Ok(())
}

/// `POST conv/<id>/removeParticipant` of a host: each person it names is turned away from the lobby, or taken out of
/// the call.
pub(super) async fn remove(app: &App, caller: &Caller, key: Key<'_>, body: &Value) -> Result<Response, Failure> {
    let named = named(body)?;
    step(app, key, caller, |call, seat, at, out| {
        call.hosts(seat)?;
        for id in &named {
            if let Some(removed) = call.seat_of(id) {
                call.remove(seat, removed, at, out)?;
            }
        }
        Ok(())
    })
    .await?;
    Ok(StatusCode::ACCEPTED.into_response())
}

/// `POST epconv` of a channel's Meet now (its subscription: the conversation's callbacks, no call invitation): the
/// channel's meeting is made with its caller its organizer, nobody in it yet, and answered; the organizer then enters
/// it at its conversation ([`enter_at_conversation`]). A meeting that runs in the channel is answered instead. A chat
/// that isn't a channel (a scheduled meeting's has its own organizer) is refused.
pub(super) async fn meet_now(
    app: &App,
    caller: &Caller,
    (from, channel): (&Value, &str),
    body: &Value,
) -> Result<Response, Failure> {
    let nodes = group::nodes(app)?;
    let members = call_events::members(app, channel).await?.ok_or(Refusal::Missing)?;
    if !members.contains(&caller.id) {
        return Err(Refusal::Forbidden.into());
    }
    if crate::chatsvc::channels::place(&app.db, channel).await?.is_none() {
        return Err(Refusal::Bad.into());
    }
    let origin = app.config.origin();
    let region = &app.config.region;
    if let Some(running) = repo::running_in(&app.db, channel).await? {
        return Ok(Json(wire::conversation(&origin, region, &running.call)).into_response());
    }
    let mut organizer = super::ops::party(&caller.id, &caller.name, from);
    if let Some((endpoint, links)) = super::own_links(app, caller, body).await? {
        organizer.keep(&endpoint, links);
    }
    organizer.status = Status::Left;
    let mut call = group::new_call(channel, organizer);
    call.meeting = Some(Meeting {
        organizer: caller.id.clone(),
        channel: true,
        post: None,
        controls: controls(app, channel).await?,
    });
    call.alive_due = now_ms() / 1000 + super::state::KEEP_ALIVE;
    call.reschedule();
    call.media = Some(nodes.pick().await?);
    match repo::insert(&app.db, &call).await {
        Ok(()) => Ok(Json(wire::conversation(&origin, region, &call)).into_response()),
        Err(DbError::Conflict) => {
            let running = repo::running_in(&app.db, channel).await?.ok_or(Failure::Busy)?;
            Ok(Json(wire::conversation(&origin, region, &running.call)).into_response())
        }
        Err(error) => Err(error.into()),
    }
}

/// `POST conv/<id>` with a call invitation on a meeting's call: a channel's meeting takes the people of its channel, a
/// scheduled one whom its lobby lets in (the others wait). The first one in a channel's meeting has the service post
/// the meeting to the channel.
pub(super) async fn enter_at_conversation(
    app: &App,
    caller: &Caller,
    call: &Call,
    body: &Value,
) -> Result<Response, Failure> {
    let meeting = call.meeting.as_ref().ok_or(Refusal::Missing)?;
    let chat = call.chat.as_deref().ok_or(Refusal::Missing)?;
    let admitted = if meeting.channel {
        let members = call_events::members(app, chat).await?.unwrap_or_default();
        if !members.contains(&caller.id) {
            return Err(Refusal::Missing.into());
        }
        Admission::In
    } else {
        admission(app, chat, &meeting.organizer, &caller.id).await?
    };
    let entered = enter(app, caller, call, admitted, body).await?;
    if meeting.channel && meeting.post.is_none() {
        post_meeting(app, call).await;
    }
    Ok(entered)
}

/// Posts a channel's meeting to its channel as its organizer's, once: the first entry claims the post, so two entries
/// at once don't post it twice. One not posted is logged.
async fn post_meeting(app: &App, call: &Call) {
    let key = Key::Id(&call.id);
    if claim_post(app, key).await {
        announce(app, key, call).await;
    }
}

/// Posts the channel's meeting and keeps its post with its call. One not posted is logged.
async fn announce(app: &App, key: Key<'_>, call: &Call) {
    // `None`: its organizer has no account or isn't in its channel (any more)
    match posted(app, call).await.transpose() {
        Some(Ok(post)) => keep_post(app, key, post).await,
        failed => tracing::warn!(?failed, "a channel meeting wasn't posted to its channel"),
    }
}

/// Whether this entry is the one that posts the channel's meeting: none claimed its post before.
async fn claim_post(app: &App, key: Key<'_>) -> bool {
    let claimed = step_any(app, key, |call, _, _| {
        let meeting = call.meeting.as_mut().ok_or(Refusal::Gone)?;
        if meeting.post.is_some() {
            return Ok(false);
        }
        meeting.post = Some(String::new());
        Ok(true)
    })
    .await;
    matches!(claimed, Ok(true))
}

/// The channel's post of its meeting `call`, as its organizer's: its id, when it was posted.
async fn posted(app: &App, call: &Call) -> Result<Option<i64>, DbError> {
    let organizer = call
        .meeting
        .as_ref()
        .map(|meeting| meeting.organizer.as_str())
        .unwrap_or_default();
    let Some(user) = users::by_id(&app.db, organizer).await? else {
        return Ok(None);
    };
    let channel = call.chat.as_deref().unwrap_or_default();
    let conversation = wire::conversation_url(&app.config.origin(), &call.conversation, None);
    call_events::meeting_started(app, channel, &user, (&call.id, &conversation)).await
}

/// Keeps the channel meeting's post `post` with its call.
async fn keep_post(app: &App, key: Key<'_>, post: i64) {
    let kept = step_any(app, key, |call, _, _| {
        if let Some(meeting) = call.meeting.as_mut() {
            meeting.post = Some(post.to_string());
        }
        Ok(())
    })
    .await;
    if let Err(Failure::Database(error)) = kept {
        tracing::warn!(%error, "a channel meeting's post wasn't kept with its call");
    }
}

/// `POST epconv` naming a meeting by its id and passcode alone (Join with an ID): the meeting they name is joined as
/// by its chat. An id and passcode that name no meeting find none.
pub(super) async fn by_code(
    app: &App,
    caller: &Caller,
    (from, (code, passcode), offer): (&Value, (&str, &str), &Value),
    body: &Value,
) -> Result<Response, Failure> {
    let meeting = crate::calendar::meeting_by_code(&app.db, code, passcode)
        .await?
        .ok_or(Refusal::Missing)?;
    let asked = Start {
        from,
        chat: &meeting.chat_id,
        to: &[],
        offer,
    };
    start(app, caller, asked, body).await
}
