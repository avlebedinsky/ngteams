//! The call registry: the calls this run's fence permitted, and the links their services handed out for each one.
//!
//! A follow-up on a call (`docs/protocol/calling-echo.md`: the endpoint's metadata, an `add`, the `leave`, the
//! acceptance's acknowledgement, a media line's channel parameters) reaches the call's participants, and no source reads
//! a call's members. So the fence trusts only the calls it permitted itself, and a follow-up passes only when it goes to
//! exactly a link the call's own services handed out for that operation (CLAUDE.md, "Calls go only to the lab, both
//! ways"). The bindings, each only as the echo call's capture shows it:
//!
//! 1. **The start.** A permit [`crate::Fence::check`] issued for a call start, with the start's body, makes a *pending*
//!    call ([`crate::Fence::pend_start`]): its caller, its participants (the permit's), and the endpoint's own callback
//!    links the start named (`callInvitation.links`, `conversationRequest.links`). The permit must be this fence's and
//!    is taken once; the body is read again and must give the permit's target.
//! 2. **The start's answer.** A readable answer to that start registers the call ([`crate::Fence::register_start`],
//!    which takes the pending call's [`PendingStart`] once): `conversationController` (on the flight proxy,
//!    `/api/v2/ep/<host>/conv/<id>`) and, of its `links`, only those of a mapped operation whose URL is the controller's
//!    path plus the operation's captured segment, with the controller's query (`updateEndpointMetadata`, `leave`, and
//!    `addParticipantAndModality`, which is the `/add` the client posts to).
//! 3. **The acceptance.** A delivery that arrives at exactly the path of the start's own `acceptance` callback
//!    ([`crate::Fence::observe_delivery`]) binds the call controller's links its body names (`callAcceptance.links`:
//!    `acknowledgement`, `applyChannelParameters`), all on the flight proxy under one call controller host and one call
//!    id; links naming another call, or a call other than one bound before, bind nothing. It may come before the answer
//!    (then it waits on the pending call, which grants nothing until it is registered).
//!
//! A `conversationUpdate` binds nothing: none of the captured operations needs it. A follow-up is then
//! [`Scope::CallOperation`] with its exact URL, and it resolves only to a live call holding that URL for that operation,
//! with the call's caller as its sender where its body names one, invitees only on an `add`, and every callback link it
//! hands the call's services (an acknowledgement's, an `add`'s) on the start's own endpoint and call agent, as in the
//! acceptance's delivery response (below); anything else is
//! [`PartialReason::UnregisteredCall`], [`PartialReason::CallEnded`] or [`DenyReason::CallMismatch`].
//!
//! **The end.** A permitted `leave` moves the call to *ending*, where only that `leave` passes again (the web client
//! resends it); a delivery at the start's own `conversationEnd` or call `end` callback ends it, and nothing passes on
//! it any more. A call is never removed or restored: an ended one stays ended for the run, whatever arrives later.
//!
//! **The acceptance's delivery response.** The one Trouter delivery response the fence lets through: the acceptance's
//! acknowledgement, answering a pending acceptance delivery of a live call on the same connection, once
//! ([`crate::Fence::check_delivery_response`]): the captured envelope (`id`, `status` 200, exactly the headers `MS-CV`,
//! `trouter-client`, and `trouter-request`, `X-Microsoft-Skype-Chain-ID` and `X-Microsoft-Skype-Message-ID` each equal
//! to the delivery's own), and a `callAcceptanceAcknowledgement` whose links are all callbacks on the start's own
//! endpoint and call agent. Every other delivery response stays denied.

use std::collections::{BTreeSet, HashSet};
use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::Serialize;
use serde_json::{Map, Value};
use url::Url;

use crate::contacting::call::{ACKNOWLEDGE, PROXY_HOST, Shape, StartEvidence, fits};
use crate::contacting::json::strict_json;
use crate::decision::{DenyReason, PartialReason};
use crate::target::{CallOperation, Mri, Scope, Target};

/// A call start the fence permitted, waiting for its answer: what [`crate::Fence::pend_start`] gives and
/// [`crate::Fence::register_start`] takes, once. Only the fence that made it can register it.
#[derive(Debug)]
pub struct PendingStart {
    fence: u64,
    call: usize,
}

impl PendingStart {
    pub(crate) fn fence(&self) -> u64 {
        self.fence
    }

    /// The pending call it stands for, using it up.
    fn into_call(self) -> usize {
        self.call
    }
}

/// Why a start's answer registered nothing. The pending call stays unregistered for good.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RegisterError {
    /// The pending start was made by another fence.
    #[error("the pending start is another fence's")]
    OtherFence,
    /// The call isn't waiting for its answer any more.
    #[error("the call isn't waiting for its answer")]
    NotPending,
    /// The answer isn't a conversation in the captured shape.
    #[error("the answer isn't a conversation in the captured shape")]
    Unreadable,
}

/// One Trouter delivery as the socket received it (a socket.io `3:::` request), its body decoded: the connection it
/// came on (an opaque key of the caller's, the same for the responses sent on that connection), its signed id, the
/// path it was delivered to, its `trouter-request`, `X-Microsoft-Skype-Chain-ID` and `X-Microsoft-Skype-Message-ID`
/// headers and its body.
#[derive(Clone, PartialEq, Eq)]
pub struct Delivery {
    pub connection: String,
    pub id: i64,
    pub path: String,
    pub trouter_request: Option<String>,
    pub chain_id: Option<String>,
    pub message_id: Option<String>,
    pub body: Vec<u8>,
}

impl fmt::Debug for Delivery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Delivery")
            .field("connection", &self.connection)
            .field("id", &self.id)
            .field("body_len", &self.body.len())
            .finish_non_exhaustive()
    }
}

/// What a delivery did to the registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Observed {
    /// It is no delivery of a registered or pending call's callback the fence reads.
    Nothing,
    /// A call's acceptance: its call controller links are bound (or were already, to the same call), and its delivery
    /// response is pending.
    Acceptance,
    /// A call's acceptance whose links bound nothing (unreadable, or naming another call).
    AcceptanceUnbound,
    /// The end of a call: it has ended.
    Ended,
}

/// What [`crate::Fence::check_delivery_response`] decided.
#[derive(Debug)]
pub enum DeliveryDecision {
    /// No pending delivery on this connection has this id (or the response has no id): nothing was decided or logged,
    /// and the caller judges the frame as any other.
    NotPending,
    /// The response may go: logged, and its pending delivery taken.
    Permitted(DeliveryPermit),
    /// The response answers a pending delivery but may not go: logged, and its pending delivery taken.
    Denied(DeliveryDenyReason),
}

/// The proof that one delivery response passed the fence. Only [`crate::Fence::check_delivery_response`] makes one.
#[derive(Debug)]
pub struct DeliveryPermit {
    delivery: i64,
}

impl DeliveryPermit {
    pub(crate) fn new(delivery: i64) -> Self {
        Self { delivery }
    }

    /// The id of the delivery it answers.
    #[must_use]
    pub fn delivery(&self) -> i64 {
        self.delivery
    }
}

/// Why a delivery response was denied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryDenyReason {
    /// Its call isn't live (still waiting for its answer, ending or ended).
    #[error("its call isn't live")]
    NotLive,
    /// Not the captured envelope or acknowledgement, or a link that isn't a callback of the call's own endpoint.
    #[error("not the captured acknowledgement of the call's acceptance")]
    Shape,
    /// Its `trouter-request`, chain id or message id isn't the delivery's.
    #[error("it doesn't answer the delivery's request")]
    Uncorrelated,
    /// It would pass, but its decision couldn't be written to `fence.log`.
    #[error("fence.log can't be written")]
    LogUnavailable,
}

/// Where a call is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// A permitted start, waiting for its answer.
    Pending,
    Live,
    /// A permitted `leave` went: only it passes again.
    Ending,
    /// Ended, or never registered: nothing passes.
    Ended,
}

/// A call controller: its host and call id.
type Controller = (String, String);

/// A link a mapped operation may go to, exactly.
type Link = (CallOperation, String);

/// An endpoint callback the start named: its URL's authority (host and port) and path.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Callback {
    authority: String,
    path: String,
}

/// An acceptance delivery waiting for its response.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingDelivery {
    connection: String,
    id: i64,
    trouter_request: String,
    chain_id: String,
    message_id: String,
}

/// One call of the run.
#[derive(Debug)]
struct Call {
    state: State,
    caller: Mri,
    participants: BTreeSet<Mri>,
    acceptance: Option<Callback>,
    /// The start's `call/end` and `conversationEnd` callbacks.
    ends: Vec<Callback>,
    /// The authority and the `/v4/f/<endpoint>/callAgent/<agent>/` path prefix every callback of the start shares.
    agent: Option<Callback>,
    /// Each link a mapped operation may go to, exactly.
    links: Vec<Link>,
    /// The call controller's host and call id, once an acceptance bound them.
    controller: Option<Controller>,
    deliveries: Vec<PendingDelivery>,
}

/// The registry. One per fence.
#[derive(Default)]
pub(crate) struct CallRegistry {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    calls: Vec<Call>,
    /// The serials of the permits already taken by [`CallRegistry::pend`].
    taken: HashSet<u64>,
}

impl fmt::Debug for CallRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CallRegistry")
            .field("calls", &self.lock().calls.len())
            .finish()
    }
}

/// What a call operation resolved to: the call's participants, and the call to move on after a permitted `leave`.
pub(crate) struct Resolved {
    pub(crate) participants: BTreeSet<Mri>,
    call: usize,
    leave: bool,
}

impl CallRegistry {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A pending call from a call start permit (its `serial`, never taken before) and the start's evidence; `None`
    /// when the permit was taken already.
    pub(crate) fn pend(&self, fence: u64, serial: u64, target: &Target, start: StartEvidence) -> Option<PendingStart> {
        let mut inner = self.lock();
        if !inner.taken.insert(serial) {
            return None;
        }
        let acceptance = callback(start.call_links.get("acceptance"));
        let ends = [
            start.call_links.get("end"),
            start.conversation_links.get("conversationEnd"),
        ]
        .into_iter()
        .filter_map(callback)
        .collect();
        let agent = acceptance.as_ref().and_then(agent_of);
        inner.calls.push(Call {
            state: State::Pending,
            caller: start.caller,
            participants: target.added().clone(),
            acceptance,
            ends,
            agent,
            links: Vec::new(),
            controller: None,
            deliveries: Vec::new(),
        });
        Some(PendingStart {
            fence,
            call: inner.calls.len() - 1,
        })
    }

    /// Registers the pending call with its start's answer (see the module docs); a call whose answer isn't readable
    /// ends unregistered.
    pub(crate) fn register(&self, pending: PendingStart, answer: &[u8]) -> Result<(), RegisterError> {
        let index = pending.into_call();
        let mut inner = self.lock();
        let call = inner.calls.get_mut(index).ok_or(RegisterError::NotPending)?;
        if call.state != State::Pending {
            return Err(RegisterError::NotPending);
        }
        let Some(links) = answer_links(answer) else {
            call.state = State::Ended;
            return Err(RegisterError::Unreadable);
        };
        call.links.extend(links);
        call.state = State::Live;
        Ok(())
    }

    /// What `delivery` does (see the module docs).
    pub(crate) fn observe(&self, delivery: &Delivery) -> Observed {
        let mut inner = self.lock();
        let found = inner.calls.iter_mut().find_map(|call| {
            if call.state == State::Ended {
                return None;
            }
            if call.ends.iter().any(|end| end.path == delivery.path) {
                return Some((call, true));
            }
            let accepted = call
                .acceptance
                .as_ref()
                .is_some_and(|accept| accept.path == delivery.path);
            accepted.then_some((call, false))
        });
        match found {
            None => Observed::Nothing,
            Some((call, true)) => {
                call.state = State::Ended;
                call.deliveries.clear();
                Observed::Ended
            }
            Some((call, false)) => bind_acceptance(call, delivery),
        }
    }

    /// Drops the pending deliveries of a connection that closed: a response on a later connection never answers them.
    pub(crate) fn close_connection(&self, connection: &str) {
        for call in &mut self.lock().calls {
            call.deliveries.retain(|delivery| delivery.connection != connection);
        }
    }

    /// The call `target` (a [`Scope::CallOperation`]) is an operation on, or why it is none (see the module docs).
    pub(crate) fn resolve(&self, target: &Target) -> Result<Resolved, DenyReason> {
        let Scope::CallOperation {
            operation,
            url,
            from,
            callbacks,
        } = target.scope()
        else {
            return Err(DenyReason::CallMismatch);
        };
        let inner = self.lock();
        let mut holding = inner
            .calls
            .iter()
            .enumerate()
            .filter(|(_, call)| call.links.iter().any(|(held, link)| held == operation && link == url));
        let (Some((index, call)), None) = (holding.next(), holding.next()) else {
            return Err(DenyReason::Partial(PartialReason::UnregisteredCall));
        };
        match (call.state, operation) {
            (State::Pending, _) => return Err(DenyReason::Partial(PartialReason::UnregisteredCall)),
            (State::Live, _) | (State::Ending, CallOperation::Leave) => {}
            (State::Ending | State::Ended, _) => return Err(DenyReason::Partial(PartialReason::CallEnded)),
        }
        let sender_fits = if operation.names_sender() {
            from.as_ref() == Some(&call.caller)
        } else {
            from.is_none()
        };
        let added_fits = (*operation == CallOperation::Add) != target.added().is_empty();
        let hands_callbacks = matches!(operation, CallOperation::Acknowledge | CallOperation::Add);
        let callbacks_fit = (hands_callbacks || callbacks.is_empty())
            && callbacks
                .iter()
                .all(|link| call.agent.as_ref().is_some_and(|agent| agent.owns(link)));
        if target.kind() != operation.kind() || !sender_fits || !added_fits || !callbacks_fit {
            return Err(DenyReason::CallMismatch);
        }
        Ok(Resolved {
            participants: call.participants.clone(),
            call: index,
            leave: *operation == CallOperation::Leave,
        })
    }

    /// After a permitted operation: a `leave` moves its call to ending.
    pub(crate) fn permitted(&self, resolved: &Resolved) {
        if !resolved.leave {
            return;
        }
        if let Some(call) = self.lock().calls.get_mut(resolved.call)
            && call.state == State::Live
        {
            call.state = State::Ending;
            call.deliveries.clear();
        }
    }

    /// Decides a delivery response sent on `connection` (see the module docs): `None` when no pending delivery there
    /// has its id. A decided one takes its pending delivery.
    pub(crate) fn respond(&self, connection: &str, response: &[u8]) -> Option<Result<i64, DeliveryDenyReason>> {
        let envelope = strict_json(response)?;
        let id = envelope.get("id")?.as_i64()?;
        let mut inner = self.lock();
        let call = inner.calls.iter_mut().find(|call| {
            call.deliveries
                .iter()
                .any(|delivery| delivery.connection == connection && delivery.id == id)
        })?;
        let at = call
            .deliveries
            .iter()
            .position(|delivery| delivery.connection == connection && delivery.id == id)?;
        let delivery = call.deliveries.remove(at);
        if call.state != State::Live {
            return Some(Err(DeliveryDenyReason::NotLive));
        }
        Some(judge_response(&envelope, &delivery, call.agent.as_ref()).map(|()| id))
    }
}

impl Callback {
    /// Whether `link` is a callback (see [`callback`]) on this call agent: its authority, under its path prefix.
    fn owns(&self, link: &str) -> bool {
        parse_callback(link).is_some_and(|link| link.authority == self.authority && link.path.starts_with(&self.path))
    }
}

/// An endpoint callback link: `https`, a host, no user info, query or fragment.
fn callback(link: Option<&Value>) -> Option<Callback> {
    parse_callback(link?.as_str()?)
}

/// [`callback`] of a link's text.
fn parse_callback(link: &str) -> Option<Callback> {
    let url = Url::parse(link).ok()?;
    let plain =
        url.username().is_empty() && url.password().is_none() && url.query().is_none() && url.fragment().is_none();
    if url.scheme() != "https" || !plain {
        return None;
    }
    Some(Callback {
        authority: authority(&url)?,
        path: url.path().to_owned(),
    })
}

/// A URL's host and port, as the URL gives them (the port only when it isn't the scheme's default).
fn authority(url: &Url) -> Option<String> {
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    })
}

/// The `/v4/f/<endpoint>/callAgent/<agent>/` prefix of a callback (and its authority): every callback of one start
/// shares it.
fn agent_of(callback: &Callback) -> Option<Callback> {
    let segments: Vec<&str> = callback.path.strip_prefix('/')?.split('/').collect();
    match segments.as_slice() {
        ["v4", "f", endpoint, "callAgent", agent, ..] if !endpoint.is_empty() && !agent.is_empty() => Some(Callback {
            authority: callback.authority.clone(),
            path: format!("/v4/f/{endpoint}/callAgent/{agent}/"),
        }),
        _ => None,
    }
}

/// A link on the flight proxy: `https`, [`PROXY_HOST`] with no port, no user info or fragment.
fn proxy_link(link: &Value) -> Option<Url> {
    let url = Url::parse(link.as_str()?).ok()?;
    let plain = url.username().is_empty() && url.password().is_none() && url.fragment().is_none();
    (url.scheme() == "https" && url.host_str() == Some(PROXY_HOST) && url.port().is_none() && plain).then_some(url)
}

/// The conversation links of a start's answer (see the module docs); `None` unless the answer is a JSON object with a
/// `conversationController` on the flight proxy at `/api/v2/ep/<host>/conv/<id>`.
fn answer_links(answer: &[u8]) -> Option<Vec<Link>> {
    const NAMES: [(CallOperation, &str); 3] = [
        (CallOperation::UpdateEndpointMetadata, "updateEndpointMetadata"),
        (CallOperation::Add, "addParticipantAndModality"),
        (CallOperation::Leave, "leave"),
    ];
    let answer = strict_json(answer)?;
    let controller = proxy_link(answer.get("conversationController")?)?;
    let segments: Vec<&str> = controller.path_segments()?.collect();
    if !matches!(segments.as_slice(), ["api", "v2", "ep", host, "conv", id] if !host.is_empty() && !id.is_empty()) {
        return None;
    }
    let links = answer.get("links").and_then(Value::as_object);
    Some(
        NAMES
            .iter()
            .filter_map(|(operation, name)| {
                let link = proxy_link(links?.get(*name)?)?;
                let fits = link.path() == format!("{}/{}", controller.path(), operation.segment())
                    && link.query() == controller.query();
                fits.then(|| (*operation, link.as_str().to_owned()))
            })
            .collect(),
    )
}

/// Binds an acceptance delivery's call controller links to `call` (see the module docs).
fn bind_acceptance(call: &mut Call, delivery: &Delivery) -> Observed {
    let Some((controller, links)) = acceptance_links(&delivery.body) else {
        return Observed::AcceptanceUnbound;
    };
    if call.controller.as_ref().is_some_and(|bound| *bound != controller) {
        return Observed::AcceptanceUnbound;
    }
    call.controller = Some(controller);
    for link in links {
        if !call.links.contains(&link) {
            call.links.push(link);
        }
    }
    // a delivery without the headers its response echoes can't be answered in the captured shape
    if let (Some(trouter_request), Some(chain_id), Some(message_id)) =
        (&delivery.trouter_request, &delivery.chain_id, &delivery.message_id)
    {
        call.deliveries.push(PendingDelivery {
            connection: delivery.connection.clone(),
            id: delivery.id,
            trouter_request: trouter_request.clone(),
            chain_id: chain_id.clone(),
            message_id: message_id.clone(),
        });
    }
    Observed::Acceptance
}

/// The call controller (host, call id) and the links of a mapped operation an acceptance's body names; `None` unless
/// every link under `callAcceptance.links` that names a call names the same one, and at least one does.
fn acceptance_links(body: &[u8]) -> Option<(Controller, Vec<Link>)> {
    const NAMES: [(CallOperation, &str, &str); 2] = [
        (CallOperation::Acknowledge, "acknowledgement", "callAcceptance"),
        (
            CallOperation::ApplyChannelParameters,
            "applyChannelParameters",
            "mcProxy",
        ),
    ];
    let body = strict_json(body)?;
    let links: &Map<String, Value> = body.pointer("/callAcceptance/links")?.as_object()?;
    let mut controller: Option<Controller> = None;
    for link in links.values() {
        let Some(url) = proxy_link(link) else { continue };
        let Some((host, call, _, _)) = controller_path(&url) else {
            continue;
        };
        match &controller {
            Some(bound) if *bound != (host.clone(), call.clone()) => return None,
            _ => controller = Some((host, call)),
        }
    }
    let controller = controller?;
    let bound = NAMES
        .iter()
        .filter_map(|(operation, name, kind)| {
            let url = proxy_link(links.get(*name)?)?;
            let (host, call, at, last) = controller_path(&url)?;
            let fits = (host, call) == controller && at == *kind && last == operation.segment();
            fits.then(|| (*operation, url.as_str().to_owned()))
        })
        .collect();
    Some((controller, bound))
}

/// A call controller link's host, call id, kind (`callAcceptance`, `mcProxy`, `active`, …) and last segment:
/// `/api/v2/ep/<host>/cc/v1/<kind>/<call>/…`.
fn controller_path(url: &Url) -> Option<(String, String, String, String)> {
    let segments: Vec<&str> = url.path_segments()?.collect();
    match segments.as_slice() {
        ["api", "v2", "ep", host, "cc", "v1", kind, call, .., last]
            if !host.is_empty() && !call.is_empty() && !kind.is_empty() =>
        {
            Some((
                (*host).to_owned(),
                (*call).to_owned(),
                (*kind).to_owned(),
                (*last).to_owned(),
            ))
        }
        _ => None,
    }
}

/// The captured response envelope: these keys, and these headers.
const ENVELOPE: [&str; 4] = ["id", "status", "headers", "body"];
const HEADERS: [&str; 5] = [
    "MS-CV",
    "trouter-request",
    "trouter-client",
    "X-Microsoft-Skype-Chain-ID",
    "X-Microsoft-Skype-Message-ID",
];

/// Judges a delivery response to `delivery` (see the module docs).
fn judge_response(
    envelope: &Value,
    delivery: &PendingDelivery,
    agent: Option<&Callback>,
) -> Result<(), DeliveryDenyReason> {
    let shape = DeliveryDenyReason::Shape;
    let object = envelope.as_object().ok_or(shape)?;
    let headers = object.get("headers").and_then(Value::as_object).ok_or(shape)?;
    let envelope_fits = object.len() == ENVELOPE.len()
        && ENVELOPE.iter().all(|key| object.contains_key(*key))
        && object.get("status").and_then(Value::as_u64) == Some(200)
        && headers.len() == HEADERS.len()
        && HEADERS
            .iter()
            .all(|name| headers.get(*name).is_some_and(Value::is_string));
    if !envelope_fits {
        return Err(shape);
    }
    let echoes = [
        ("trouter-request", &delivery.trouter_request),
        ("X-Microsoft-Skype-Chain-ID", &delivery.chain_id),
        ("X-Microsoft-Skype-Message-ID", &delivery.message_id),
    ];
    if echoes
        .iter()
        .any(|(name, value)| headers.get(*name).and_then(Value::as_str) != Some(value.as_str()))
    {
        return Err(DeliveryDenyReason::Uncorrelated);
    }
    let body = object.get("body").and_then(Value::as_str).ok_or(shape)?;
    let acknowledgement = strict_json(body.as_bytes()).ok_or(shape)?;
    if !fits(&acknowledgement, Shape::Object(ACKNOWLEDGE)) {
        return Err(shape);
    }
    let agent = agent.ok_or(shape)?;
    let links = acknowledgement
        .pointer("/callAcceptanceAcknowledgement/links")
        .and_then(Value::as_object)
        .ok_or(shape)?;
    let own = links
        .values()
        .all(|link| link.as_str().is_some_and(|link| agent.owns(link)));
    if own { Ok(()) } else { Err(shape) }
}
