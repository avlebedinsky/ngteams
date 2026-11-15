//! The calling requests: the start, the conversation's operations and the call controller's, each checked against
//! the call's parties and carried out as one step of the call ([`super::step`]).

use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use super::repo::{self, Key};
use super::state::{Call, Kind, Party, Phase, Refusal, Seat};
use super::wire::{self, CONTROLLER_HOST, CONVERSATION_HOST};
use super::{Caller, Failure, call_events, calling_endpoints, needed_links, now_ms, own_links, step};
use crate::app::App;
use crate::db::users;

type Params = HashMap<String, String>;

/// A request's JSON; a request without a body (a keep-alive's, a `DELETE`'s) reads as null.
fn json(body: &Bytes) -> Result<Value, Failure> {
    if body.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(body).map_err(|_| Refusal::Bad.into())
}

fn param<'a>(path: &'a Params, name: &str) -> &'a str {
    path.get(name).map_or("", String::as_str)
}

fn hosted(path: &Params, host: &str) -> Result<(), Failure> {
    if param(path, "host") == host {
        Ok(())
    } else {
        Err(Refusal::Missing.into())
    }
}

fn status(status: StatusCode) -> Response {
    status.into_response()
}

/// The call `key` names and `caller`'s seat of it; someone who is no party of it finds none.
async fn party_of(app: &App, key: Key<'_>, caller: &Caller) -> Result<(Call, Seat), Failure> {
    let kept = repo::find(&app.db, key).await?.ok_or(Refusal::Missing)?;
    let seat = kept.call.seat_of(&caller.id).ok_or(Refusal::Missing)?;
    Ok((kept.call, seat))
}

/// What a start names: a 1:1 call, or a group chat's call.
enum Asked<'a> {
    OneToOne(OneToOne<'a>),
    Group(super::group::Start<'a>),
}

/// What a 1:1 call's start names: the caller as it names itself, and its one callee.
struct OneToOne<'a> {
    from: &'a Value,
    to: &'a Value,
    callee: &'a str,
    offer: &'a Value,
}

/// Reads a start; a meeting's and the test call's aren't served.
fn asked<'a>(caller: &Caller, body: &'a Value) -> Result<Asked<'a>, Failure> {
    let from = body.pointer("/participants/from").ok_or(Refusal::Bad)?;
    let callees = body
        .pointer("/participants/to")
        .and_then(Value::as_array)
        .ok_or(Refusal::Bad)?;
    if from.get("id").and_then(Value::as_str) != Some(caller.mri().as_str()) {
        return Err(Refusal::Forbidden.into());
    }
    let set = |pointer: &str| body.pointer(pointer).is_some_and(|value| !value.is_null());
    let other_kind = ["/meetingInfo", "/meetingData", "/conversationRequest/scenario"]
        .into_iter()
        .any(set);
    if other_kind {
        return Err(Failure::NotServed);
    }
    if set("/groupChat") {
        let chat = body
            .pointer("/groupChat/threadId")
            .and_then(Value::as_str)
            .ok_or(Refusal::Bad)?;
        let offer = body.pointer("/callInvitation/mediaContent").ok_or(Refusal::Bad)?;
        return Ok(Asked::Group(super::group::Start {
            from,
            chat,
            to: callees,
            offer,
        }));
    }
    let [to] = callees.as_slice() else {
        return Err(Failure::NotServed);
    };
    let invited = to
        .get("id")
        .and_then(Value::as_str)
        .and_then(|id| id.strip_prefix("8:orgid:"))
        .ok_or(Refusal::Bad)?;
    let offer = body
        .pointer("/callInvitation/mediaContent")
        .filter(|offer| offer.is_object())
        .ok_or(Refusal::Bad)?;
    if invited == caller.id {
        return Err(Refusal::Bad.into());
    }
    Ok(Asked::OneToOne(OneToOne {
        from,
        to,
        callee: invited,
        offer,
    }))
}

pub(super) fn party(id: &str, name: &str, named: &Value) -> Party {
    let mut party = Party {
        id: id.to_owned(),
        name: name.to_owned(),
        ..Party::default()
    };
    party.participant = wire::named_by(named, &party);
    party
}

/// `POST epconv`: a call's start. A 1:1 call's conversation is answered and the callee's calling endpoints ring; a
/// group call's is [`super::group::start`].
pub(super) async fn start(State(app): State<Arc<App>>, caller: Caller, body: Bytes) -> Result<Response, Failure> {
    let body = json(&body)?;
    let asked = match asked(&caller, &body)? {
        Asked::Group(start) => return super::group::start(&app, &caller, start, &body).await,
        Asked::OneToOne(asked) => asked,
    };
    let invited = users::by_id(&app.db, asked.callee)
        .await?
        .filter(|user| !user.disabled)
        .ok_or(Refusal::Missing)?;
    let (endpoint, links) = needed_links(&app, &caller, &body).await?;
    if !["acceptance", "end"].iter().all(|name| links.contains_key(*name)) {
        return Err(Refusal::Bad.into());
    }
    let now = now_ms();
    let mut from = party(&caller.id, &caller.name, asked.from);
    from.keep(&endpoint, links);
    from.active = Some(endpoint);
    let chat = call_events::chat_of(&app, &caller.id, &invited.id).await?;
    // read before the call exists: a start that fails leaves no call ringing and no event in the chat
    let tenant = crate::db::meta::tenant(&app.db).await?;
    let ringing = calling_endpoints(&app, &invited.id).await?;
    let call = Call {
        id: uuid::Uuid::new_v4().to_string(),
        conversation: uuid::Uuid::new_v4().simple().to_string(),
        phase: Phase::Ringing,
        parties: vec![from, party(&invited.id, &invited.display_name, asked.to)],
        offer: asked.offer.clone(),
        started_at: now,
        connected_at: None,
        ended_at: None,
        due_at: now / 1000 + super::ring_seconds(&app),
        roster_sequence: 0,
        negotiation: None,
        negotiations: 0,
        answered: None,
        held_by: None,
        chat,
        accepted_as: None,
        kind: Kind::OneToOne,
        media: None,
        next_source: 0,
        watchers: Vec::new(),
        alive_due: 0,
    };
    repo::insert(&app.db, &call).await?;
    // before anyone can end the call: the chat orders its events by when they were written
    tell_chat_started(&app, &caller, &call, None).await;
    let invitation = wire::invitation(&app.config.origin(), &call, &tenant, &app.config.region);
    ring(&app, &call.id, &invitation, ringing).await;
    let origin = app.config.origin();
    Ok(Json(wire::conversation(&origin, &app.config.region, &call)).into_response())
}

/// Writes the call's start to its chat, when it has one: with where it runs, when `conversation` names it.
pub(super) async fn tell_chat_started(app: &App, caller: &Caller, call: &Call, conversation: Option<&str>) {
    let Some(chat) = &call.chat else { return };
    let told = call_events::started(app, chat, (&caller.id, &caller.name), (&call.id, conversation)).await;
    if let Err(error) = told {
        tracing::warn!(%error, "a call's start wasn't written to its chat");
    }
}

/// Delivers the call `call`'s `invitation` to each of `endpoints`, the calling endpoints its callee registered.
pub(super) async fn ring(app: &App, call: &str, invitation: &Value, endpoints: Vec<String>) {
    for endpoint in endpoints {
        let delivery = super::state::Delivery {
            endpoint,
            path: String::new(),
            body: invitation.clone(),
        };
        super::publish(app, call, &delivery).await;
    }
}

/// `POST conv/<id>`: a party's subscription to its call's conversation. A 1:1 call takes no one else in; a group
/// call's is [`super::group::subscribe`].
pub(super) async fn subscribe(
    State(app): State<Arc<App>>,
    caller: Caller,
    Path(path): Path<Params>,
    body: Bytes,
) -> Result<Response, Failure> {
    hosted(&path, CONVERSATION_HOST)?;
    let key = Key::Conversation(param(&path, "conversation"));
    let body = json(&body)?;
    let kept = repo::find(&app.db, key).await?.ok_or(Refusal::Missing)?;
    if kept.call.kind == Kind::Group {
        return super::group::subscribe(&app, &caller, key, &kept.call, &body).await;
    }
    let (call, _) = party_of(&app, key, &caller).await?;
    if body
        .get("callInvitation")
        .is_some_and(|invitation| !invitation.is_null())
    {
        return Err(Refusal::Forbidden.into());
    }
    if let Some((endpoint, links)) = own_links(&app, &caller, &body).await? {
        step(&app, key, &caller, |call, seat, _, _| {
            call.more_links(seat, &endpoint, links.clone());
            Ok(())
        })
        .await?;
    }
    let origin = app.config.origin();
    Ok(Json(wire::conversation(&origin, &app.config.region, &call)).into_response())
}

/// `DELETE conv/<id>`: the call ended at its conversation, as a leave ends a 1:1 call.
pub(super) async fn end_for_all(
    State(app): State<Arc<App>>,
    caller: Caller,
    Path(path): Path<Params>,
    body: Bytes,
) -> Result<Response, Failure> {
    hosted(&path, CONVERSATION_HOST)?;
    let body = json(&body)?;
    let key = Key::Conversation(param(&path, "conversation"));
    step(&app, key, &caller, |call, seat, at, out| {
        call.leave(seat, &body, at, out);
        Ok(())
    })
    .await?;
    Ok(status(StatusCode::NO_CONTENT))
}

/// An operation on a call's conversation: `leave`, the endpoint's state and metadata, its later callbacks. One that
/// comes after the call ended (a write that was on its way) is taken and changes nothing: a refusal would read as
/// the call failing.
pub(super) async fn conversation(
    State(app): State<Arc<App>>,
    caller: Caller,
    Path(path): Path<Params>,
    body: Bytes,
) -> Result<Response, Failure> {
    hosted(&path, CONVERSATION_HOST)?;
    let key = Key::Conversation(param(&path, "conversation"));
    party_of(&app, key, &caller).await?;
    let body = json(&body)?;
    match param(&path, "op") {
        "leave" => {
            step(&app, key, &caller, |call, seat, at, out| {
                call.leave(seat, &body, at, out);
                Ok(())
            })
            .await?;
        }
        "updateEndpointState" => {
            step(&app, key, &caller, |call, seat, at, out| {
                call.endpoint_state(seat, &body, at, out)
            })
            .await?;
        }
        "notificationLinks" => {
            if let Some((endpoint, links)) = own_links(&app, &caller, &body).await? {
                step(&app, key, &caller, |call, seat, _, _| {
                    call.more_links(seat, &endpoint, links.clone());
                    Ok(())
                })
                .await?;
            }
        }
        "add" => return super::group::add(&app, &caller, key, &body).await,
        "updateEndpointMetadata" | "updateParticipantProperties" => {}
        _ => return Err(Refusal::Missing.into()),
    }
    Ok(status(StatusCode::OK))
}

/// A call leg's own link: the call's keep-alive.
pub(super) async fn keep_alive(
    State(app): State<Arc<App>>,
    caller: Caller,
    Path(path): Path<Params>,
) -> Result<Response, Failure> {
    hosted(&path, CONTROLLER_HOST)?;
    if param(&path, "kind") != "active" {
        return Err(Refusal::Missing.into());
    }
    let key = Key::Id(param(&path, "call"));
    let (call, seat) = party_of(&app, key, &caller).await?;
    own_leg(&call, seat, &path)?;
    step(&app, key, &caller, |call, seat, at, _| call.keep_alive(seat, at)).await?;
    Ok(status(StatusCode::ACCEPTED))
}

/// A group call's controller links name the party's own seat; one naming another's is none of the account's.
fn own_leg(call: &Call, seat: Seat, path: &Params) -> Result<(), Failure> {
    if param(path, "leg") == super::wire::leg_of(call, seat).to_string() {
        Ok(())
    } else {
        Err(Refusal::Missing.into())
    }
}

/// An operation on a call controller's link.
pub(super) async fn controller(
    State(app): State<Arc<App>>,
    caller: Caller,
    Path(path): Path<Params>,
    body: Bytes,
) -> Result<Response, Failure> {
    hosted(&path, CONTROLLER_HOST)?;
    let key = Key::Id(param(&path, "call"));
    let (call, seat) = party_of(&app, key, &caller).await?;
    own_leg(&call, seat, &path)?;
    let kind = param(&path, "kind");
    // a 1:1 call's ringing leg is its callee's alone, whatever the request carries
    if matches!(kind, "forked" | "incoming") && seat == Seat::CALLER && call.kind == Kind::OneToOne {
        return Err(Refusal::Forbidden.into());
    }
    // a group call's invitation links name the party's ring: an earlier ring's are over
    if kind == "forked" && call.kind == Kind::Group && param(&path, "number") != call.party(seat).rings.to_string() {
        return Err(Refusal::Gone.into());
    }
    let body = json(&body)?;
    let asked = Leg {
        app: &app,
        caller: &caller,
        group: call.kind == Kind::Group,
        key,
        number: param(&path, "number"),
        body: &body,
    };
    asked.run(kind, param(&path, "op")).await
}

/// A call controller request of a party of its call.
struct Leg<'a> {
    app: &'a App,
    caller: &'a Caller,
    /// The call is a group call's.
    group: bool,
    key: Key<'a>,
    /// The link's number: a negotiation's own, for its answer, rejection and acknowledgement.
    number: &'a str,
    body: &'a Value,
}

impl Leg<'_> {
    /// The operation `op` on a link of `kind`.
    async fn run(&self, kind: &str, op: &str) -> Result<Response, Failure> {
        match (kind, op) {
            ("forked", "attach") => self.attach().await,
            ("forked" | "incoming", "progress") => self.simple(Call::progress).await,
            ("incoming", "accept") => self.accept().await,
            ("forked" | "incoming", "reject") => self.reject().await,
            ("active", "renegotiate") => self.renegotiate().await,
            ("negotiations", "answer") => self.answer().await,
            ("negotiations", "reject") => self.turn_away().await,
            ("mediaAnswers", "acknowledge") => self.acknowledge_answer().await,
            ("callAcceptance", "acknowledge") => self.acknowledge_acceptance().await,
            ("active", "hold") => self.hold().await,
            ("holdSession", "resume") => self.resume().await,
            ("active", "updateMediaDescriptions") | ("mcProxy", "applyChannelParameters") => {
                self.simple(Call::described).await
            }
            _ => Err(Refusal::Missing.into()),
        }
    }

    /// The negotiation the link is of.
    fn negotiation(&self) -> Result<u64, Failure> {
        self.number.parse().map_err(|_| Refusal::Missing.into())
    }

    /// An operation that changes nothing a party is told: checked against the call, answered 200.
    async fn simple(&self, check: impl Fn(&Call, Seat) -> Result<(), Refusal>) -> Result<Response, Failure> {
        step(self.app, self.key, self.caller, |call, seat, _, _| check(call, seat)).await?;
        Ok(status(StatusCode::OK))
    }

    async fn attach(&self) -> Result<Response, Failure> {
        let (endpoint, links) = needed_links(self.app, self.caller, self.body).await?;
        let answer = step(self.app, self.key, self.caller, |call, seat, at, _| {
            call.attach(seat, &endpoint, links.clone(), at)
        })
        .await?;
        Ok(Json(answer).into_response())
    }

    async fn accept(&self) -> Result<Response, Failure> {
        if self.group {
            return super::group::accept(self.app, self.caller, self.key, self.body).await;
        }
        let (endpoint, links) = needed_links(self.app, self.caller, self.body).await?;
        let answer = step(self.app, self.key, self.caller, |call, seat, at, out| {
            call.accept(seat, &endpoint, (links.clone(), self.body), at, out)
        })
        .await?;
        Ok(Json(answer).into_response())
    }

    async fn reject(&self) -> Result<Response, Failure> {
        step(self.app, self.key, self.caller, |call, seat, at, out| {
            call.reject(seat, self.body, at, out)
        })
        .await?;
        Ok(status(StatusCode::OK))
    }

    async fn renegotiate(&self) -> Result<Response, Failure> {
        if self.group {
            return super::group::renegotiate(self.app, self.caller, self.key, self.body).await;
        }
        let (endpoint, links) = needed_links(self.app, self.caller, self.body).await?;
        step(self.app, self.key, self.caller, |call, seat, at, out| {
            call.renegotiate(seat, &endpoint, (links.clone(), self.body), at, out)
        })
        .await?;
        Ok(status(StatusCode::OK))
    }

    async fn answer(&self) -> Result<Response, Failure> {
        let number = self.negotiation()?;
        let (endpoint, links) = needed_links(self.app, self.caller, self.body).await?;
        step(self.app, self.key, self.caller, |call, seat, at, out| {
            call.answer(seat, number, (&endpoint, links.clone(), self.body), at, out)
        })
        .await?;
        Ok(status(StatusCode::OK))
    }

    async fn turn_away(&self) -> Result<Response, Failure> {
        let number = self.negotiation()?;
        step(self.app, self.key, self.caller, |call, seat, at, out| {
            call.turn_away(seat, number, self.body, at, out)
        })
        .await?;
        Ok(status(StatusCode::OK))
    }

    async fn acknowledge_answer(&self) -> Result<Response, Failure> {
        let number = self.negotiation()?;
        step(self.app, self.key, self.caller, |call, seat, at, out| {
            call.acknowledge_answer(seat, number, at, out)
        })
        .await?;
        Ok(status(StatusCode::OK))
    }

    /// The caller's acknowledgement of its acceptance by request: its later callbacks, as its delivery response
    /// would have handed them out.
    async fn acknowledge_acceptance(&self) -> Result<Response, Failure> {
        if let Some((endpoint, links)) = own_links(self.app, self.caller, self.body).await? {
            step(self.app, self.key, self.caller, |call, seat, _, _| {
                call.more_links(seat, &endpoint, links.clone());
                Ok(())
            })
            .await?;
        }
        Ok(status(StatusCode::OK))
    }

    async fn hold(&self) -> Result<Response, Failure> {
        let (endpoint, links) = needed_links(self.app, self.caller, self.body).await?;
        let answer = step(self.app, self.key, self.caller, |call, seat, at, out| {
            call.hold(seat, &endpoint, links.clone(), at, out)
        })
        .await?;
        Ok((StatusCode::ACCEPTED, Json(answer)).into_response())
    }

    async fn resume(&self) -> Result<Response, Failure> {
        let (endpoint, links) = needed_links(self.app, self.caller, self.body).await?;
        step(self.app, self.key, self.caller, |call, seat, at, out| {
            call.resume(seat, &endpoint, links.clone(), at, out)
        })
        .await?;
        Ok(status(StatusCode::ACCEPTED))
    }
}
