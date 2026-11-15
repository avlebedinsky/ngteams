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
use super::state::{Call, Party, Phase, Refusal, Side};
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

/// The call `key` names and `caller`'s side of it; someone who is no party of it finds none.
async fn party_of(app: &App, key: Key<'_>, caller: &Caller) -> Result<(Call, Side), Failure> {
    let kept = repo::find(&app.db, key).await?.ok_or(Refusal::Missing)?;
    let side = kept.call.side_of(&caller.id).ok_or(Refusal::Missing)?;
    Ok((kept.call, side))
}

/// What a start names: the caller as it names itself, and its one callee.
struct Asked<'a> {
    from: &'a Value,
    to: &'a Value,
    callee: &'a str,
    offer: &'a Value,
}

/// Reads a start; a group call's, a meeting's and the test call's aren't served.
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
    let other_kind = [
        "/groupChat",
        "/meetingInfo",
        "/meetingData",
        "/conversationRequest/scenario",
    ]
    .into_iter()
    .any(set);
    let [to] = callees.as_slice() else {
        return Err(Failure::NotServed);
    };
    if other_kind {
        return Err(Failure::NotServed);
    }
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
    Ok(Asked {
        from,
        to,
        callee: invited,
        offer,
    })
}

fn party(id: &str, name: &str, named: &Value) -> Party {
    let mut party = Party {
        id: id.to_owned(),
        name: name.to_owned(),
        participant: Value::Null,
        endpoints: Vec::new(),
        active: None,
        muted: false,
        state_sequence: 0,
        joined: None,
    };
    party.participant = wire::named_by(named, &party);
    party
}

/// `POST epconv`: a 1:1 call's start. The conversation is answered and the callee's calling endpoints ring.
pub(super) async fn start(State(app): State<Arc<App>>, caller: Caller, body: Bytes) -> Result<Response, Failure> {
    let body = json(&body)?;
    let asked = asked(&caller, &body)?;
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
        caller: from,
        callee: party(&invited.id, &invited.display_name, asked.to),
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
    };
    repo::insert(&app.db, &call).await?;
    // before anyone can end the call: the chat orders its events by when they were written
    tell_chat_started(&app, &caller, &call).await;
    ring(&app, &call, &tenant, ringing).await;
    let origin = app.config.origin();
    Ok(Json(wire::conversation(&origin, &app.config.region, &call)).into_response())
}

/// Writes the call's start to the chat of its two parties, when they have one.
async fn tell_chat_started(app: &App, caller: &Caller, call: &Call) {
    let Some(chat) = &call.chat else { return };
    let told = call_events::started(app, chat, (&caller.id, &caller.name), &call.id).await;
    if let Err(error) = told {
        tracing::warn!(%error, "a call's start wasn't written to its chat");
    }
}

/// Delivers the call's invitation to each of `endpoints`, the calling endpoints its callee registered.
async fn ring(app: &App, call: &Call, tenant: &str, endpoints: Vec<String>) {
    let invitation = wire::invitation(&app.config.origin(), call, tenant, &app.config.region);
    for endpoint in endpoints {
        let delivery = super::state::Delivery {
            endpoint,
            path: String::new(),
            body: invitation.clone(),
        };
        super::publish(app, &call.id, &delivery).await;
    }
}

/// `POST conv/<id>`: a party's subscription to its call's conversation. A 1:1 call takes no one else in.
pub(super) async fn subscribe(
    State(app): State<Arc<App>>,
    caller: Caller,
    Path(path): Path<Params>,
    body: Bytes,
) -> Result<Response, Failure> {
    hosted(&path, CONVERSATION_HOST)?;
    let key = Key::Conversation(param(&path, "conversation"));
    let body = json(&body)?;
    let (call, _) = party_of(&app, key, &caller).await?;
    if body
        .get("callInvitation")
        .is_some_and(|invitation| !invitation.is_null())
    {
        return Err(Refusal::Forbidden.into());
    }
    if let Some((endpoint, links)) = own_links(&app, &caller, &body).await? {
        step(&app, key, &caller, |call, side, _, _| {
            call.more_links(side, &endpoint, links.clone());
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
    step(&app, key, &caller, |call, side, at, out| {
        call.leave(side, &body, at, out);
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
            step(&app, key, &caller, |call, side, at, out| {
                call.leave(side, &body, at, out);
                Ok(())
            })
            .await?;
        }
        "updateEndpointState" => {
            step(&app, key, &caller, |call, side, at, out| {
                call.endpoint_state(side, &body, at, out)
            })
            .await?;
        }
        "notificationLinks" => {
            if let Some((endpoint, links)) = own_links(&app, &caller, &body).await? {
                step(&app, key, &caller, |call, side, _, _| {
                    call.more_links(side, &endpoint, links.clone());
                    Ok(())
                })
                .await?;
            }
        }
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
    step(&app, key, &caller, |call, side, at, _| call.keep_alive(side, at)).await?;
    Ok(status(StatusCode::ACCEPTED))
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
    let (_, side) = party_of(&app, key, &caller).await?;
    let kind = param(&path, "kind");
    // the ringing leg is the callee's alone, whatever the request carries
    if matches!(kind, "forked" | "incoming") && side != Side::Callee {
        return Err(Refusal::Forbidden.into());
    }
    let body = json(&body)?;
    let asked = Leg {
        app: &app,
        caller: &caller,
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
    async fn simple(&self, check: impl Fn(&Call, Side) -> Result<(), Refusal>) -> Result<Response, Failure> {
        step(self.app, self.key, self.caller, |call, side, _, _| check(call, side)).await?;
        Ok(status(StatusCode::OK))
    }

    async fn attach(&self) -> Result<Response, Failure> {
        let (endpoint, links) = needed_links(self.app, self.caller, self.body).await?;
        let answer = step(self.app, self.key, self.caller, |call, side, at, _| {
            call.attach(side, &endpoint, links.clone(), at)
        })
        .await?;
        Ok(Json(answer).into_response())
    }

    async fn accept(&self) -> Result<Response, Failure> {
        let (endpoint, links) = needed_links(self.app, self.caller, self.body).await?;
        let answer = step(self.app, self.key, self.caller, |call, side, at, out| {
            call.accept(side, &endpoint, (links.clone(), self.body), at, out)
        })
        .await?;
        Ok(Json(answer).into_response())
    }

    async fn reject(&self) -> Result<Response, Failure> {
        step(self.app, self.key, self.caller, |call, side, at, out| {
            call.reject(side, self.body, at, out)
        })
        .await?;
        Ok(status(StatusCode::OK))
    }

    async fn renegotiate(&self) -> Result<Response, Failure> {
        let (endpoint, links) = needed_links(self.app, self.caller, self.body).await?;
        step(self.app, self.key, self.caller, |call, side, at, out| {
            call.renegotiate(side, &endpoint, (links.clone(), self.body), at, out)
        })
        .await?;
        Ok(status(StatusCode::OK))
    }

    async fn answer(&self) -> Result<Response, Failure> {
        let number = self.negotiation()?;
        let (endpoint, links) = needed_links(self.app, self.caller, self.body).await?;
        step(self.app, self.key, self.caller, |call, side, at, out| {
            call.answer(side, number, (&endpoint, links.clone(), self.body), at, out)
        })
        .await?;
        Ok(status(StatusCode::OK))
    }

    async fn turn_away(&self) -> Result<Response, Failure> {
        let number = self.negotiation()?;
        step(self.app, self.key, self.caller, |call, side, at, out| {
            call.turn_away(side, number, self.body, at, out)
        })
        .await?;
        Ok(status(StatusCode::OK))
    }

    async fn acknowledge_answer(&self) -> Result<Response, Failure> {
        let number = self.negotiation()?;
        step(self.app, self.key, self.caller, |call, side, at, out| {
            call.acknowledge_answer(side, number, at, out)
        })
        .await?;
        Ok(status(StatusCode::OK))
    }

    /// The caller's acknowledgement of its acceptance by request: its later callbacks, as its delivery response
    /// would have handed them out.
    async fn acknowledge_acceptance(&self) -> Result<Response, Failure> {
        if let Some((endpoint, links)) = own_links(self.app, self.caller, self.body).await? {
            step(self.app, self.key, self.caller, |call, side, _, _| {
                call.more_links(side, &endpoint, links.clone());
                Ok(())
            })
            .await?;
        }
        Ok(status(StatusCode::OK))
    }

    async fn hold(&self) -> Result<Response, Failure> {
        let (endpoint, links) = needed_links(self.app, self.caller, self.body).await?;
        let answer = step(self.app, self.key, self.caller, |call, side, at, out| {
            call.hold(side, &endpoint, links.clone(), at, out)
        })
        .await?;
        Ok((StatusCode::ACCEPTED, Json(answer)).into_response())
    }

    async fn resume(&self) -> Result<Response, Failure> {
        let (endpoint, links) = needed_links(self.app, self.caller, self.body).await?;
        step(self.app, self.key, self.caller, |call, side, at, out| {
            call.resume(side, &endpoint, links.clone(), at, out)
        })
        .await?;
        Ok(status(StatusCode::ACCEPTED))
    }
}
