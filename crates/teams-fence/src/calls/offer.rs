//! Offered invitations: an incoming call the account's fence would let it answer, and what its client may send by
//! itself while it rings (`docs/protocol/calling.md`, "What the callee sends while it rings").
//!
//! An invitation is a Trouter delivery at the root of the account's calling endpoint (`/v4/f/<endpoint>/`) whose body
//! is `{"evt": 107 | 109, "gp": <base64 JSON>}` holding a `callNotification` ([`read_invitation`]). The fence
//! (`crate::Fence::observe_delivery`) offers it only when it is readable, addressed to the fence's own account, and its
//! caller is a lab account or an approved service; a group call's invitation names only its caller, its callee and its
//! group chat, so every decision on it also reads that chat fresh (`crate::Fence::check`), and a group call's
//! invitation that names no group chat doesn't read. What an offered invitation grants, each bound to it:
//!
//! - **its delivery response**: the captured envelope (`id`, `status` 200, `trouter-client` present, `trouter-request`
//!   equal to the delivery's, `MS-CV` or not, an empty body), on the connection the delivery came on, once per delivery
//!   ([`judge_receipt`]);
//! - **the `attach`**: to exactly the invitation's `attach` link, its one `join` action to exactly the invitation's
//!   conversation (roster only: it joins no call), its sender the callee, every callback it hands out on one call agent
//!   of the endpoint the invitation was delivered to. A permitted attach records the callbacks the call's end comes back
//!   to (`call/end`, `conversationEnd`): a delivery at one of them ends the ring for good. Its answer, read at the
//!   response stage ([`Offer::bind_answer`]), binds the call leg's `progress`, `acceptance` and `callLeg` links and the
//!   conversation's links;
//! - while it rings, **`progress`** (`ringing`), the **endpoint state**, and the user's **answer** (`accept`) or
//!   **decline** (`reject`, `603` only), each to exactly a bound link;
//! - once answered, the answered call's follow-ups to the links the attach's answer, the `accept`'s answer
//!   ([`Offer::bind_acceptance`]) and the call's media deliveries ([`Offer::bind_media`]) handed out, until the callee
//!   leaves;
//! - **a join** of the running call: the invitation binds its conversation to its group chat ([`Offer::binds`]), so a
//!   join of exactly that conversation from the callee, its chat read fresh, is a start in that chat, even after the
//!   ring ended.
//!
//! An ended invitation is never offered again. A delivery at the root that may be an invitation but doesn't read
//! ([`may_be_invitation`]: a `callNotification` the fence can't read, an unread `evt` 107 or 109, a body that isn't
//! strictly JSON) is ignored like one it doesn't offer: no consumer is handed an invitation the fence can't judge.

use std::collections::BTreeSet;

use base64::Engine as _;
use serde_json::Value;
use url::Url;

use super::{Callback, Controller, Delivery, DeliveryDenyReason, Link, controller_path, parse_callback, proxy_link};
use crate::contacting::json::strict_json;
use crate::membership::is_group_chat_id;
use crate::target::{CallOperation, Mri};

/// The event numbers of a call invitation delivered over Trouter: a 1:1 call's (107) and a group call's (109).
pub const INVITATION_EVENTS: [u64; 2] = [107, 109];

/// What an invitation delivery says, read: who calls whom, in which group chat, and its links.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Invitation {
    pub(crate) caller: Mri,
    pub(crate) callee: Mri,
    pub(crate) group_chat: Option<String>,
    /// The endpoint root it was delivered to (`/v4/f/<endpoint>/`).
    endpoint: String,
    /// `conversationInvitation.conversationController`, exactly.
    controller: String,
    /// `callNotification.links.attach`, exactly.
    attach: String,
    /// The forked call leg the links name (the call controller's host and call id).
    leg: Controller,
}

/// An invitation delivery waiting for its response.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Receipt {
    connection: String,
    id: i64,
    trouter_request: String,
}

/// Where an offered invitation is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OfferState {
    /// Ringing: its ring-time sends may go, and its answer or decline.
    Ringing,
    /// Answered by a permitted `accept`: that `accept` passes again (the browser decides a request on two layers,
    /// and the web client resends it), and the answered call's follow-ups to its bound links.
    Answered,
    /// Answered, and its callee left by a permitted `leave`: only that `leave` passes again (the web client resends it).
    Leaving,
    /// Declined by a permitted `reject`: only that `reject` passes again (as an answered invitation's `accept`).
    Declined,
    /// Its ring ended: nothing passes on it any more.
    Ended,
}

/// One offered invitation of the run.
#[derive(Debug)]
pub(crate) struct Offer {
    pub(crate) state: OfferState,
    pub(crate) invitation: Invitation,
    /// The links a ring-time send may go to, exactly.
    links: Vec<Link>,
    /// The call agent every callback of the callee's attach shares, once one was permitted.
    agent: Option<Callback>,
    /// The callbacks the call's end comes back to, from the permitted attach.
    ends: Vec<Callback>,
    receipts: Vec<Receipt>,
}

impl Offer {
    pub(crate) fn new(invitation: Invitation) -> Self {
        let links = vec![(CallOperation::Attach, invitation.attach.clone())];
        Self {
            state: OfferState::Ringing,
            invitation,
            links,
            agent: None,
            ends: Vec::new(),
            receipts: Vec::new(),
        }
    }

    /// Whether `invitation` is another delivery of this one (the same leg, the same parties).
    pub(crate) fn is_redelivery(&self, invitation: &Invitation) -> bool {
        self.invitation == *invitation
    }

    /// Keeps `delivery` waiting for its response, when it carries the `trouter-request` its response echoes.
    pub(crate) fn expect_receipt(&mut self, delivery: &Delivery) {
        if let Some(trouter_request) = &delivery.trouter_request {
            self.receipts.push(Receipt {
                connection: delivery.connection.clone(),
                id: delivery.id,
                trouter_request: trouter_request.clone(),
            });
        }
    }

    /// Whether a delivery at `path` ends the ring.
    pub(crate) fn ends_at(&self, path: &str) -> bool {
        self.ends.iter().any(|end| end.path == path)
    }

    /// Ends the ring for good.
    pub(crate) fn end(&mut self) {
        self.state = OfferState::Ended;
        self.receipts.clear();
    }

    /// Whether it holds exactly `url` for `operation`.
    pub(crate) fn holds(&self, operation: CallOperation, url: &str) -> bool {
        self.links.iter().any(|(held, link)| *held == operation && link == url)
    }

    /// Whether a receipt on `connection` with `id` waits here.
    pub(crate) fn awaits(&self, connection: &str, id: i64) -> bool {
        self.receipts
            .iter()
            .any(|receipt| receipt.connection == connection && receipt.id == id)
    }

    /// Takes the receipt on `connection` with `id` and judges `envelope` as its response ([`judge_receipt`]).
    pub(crate) fn take_receipt(
        &mut self,
        connection: &str,
        id: i64,
        envelope: &Value,
    ) -> Result<(), DeliveryDenyReason> {
        let at = self
            .receipts
            .iter()
            .position(|receipt| receipt.connection == connection && receipt.id == id)
            .ok_or(DeliveryDenyReason::NotLive)?;
        let receipt = self.receipts.remove(at);
        judge_receipt(envelope, &receipt.trouter_request)
    }

    /// Drops the receipts of a connection that closed.
    pub(crate) fn close_connection(&mut self, connection: &str) {
        self.receipts.retain(|receipt| receipt.connection != connection);
    }

    /// Whether `operation` from `from`, joining `joins` and handing out `callbacks`, fits this invitation (which
    /// operations its state is open for is [`Self::open_for`]'s):
    /// - an `attach`, from the callee, joining exactly the invitation's conversation, every callback on one call agent
    ///   of the endpoint the invitation was delivered to (the one an earlier permitted attach used);
    /// - a `progress`, an endpoint state or metadata, or a `leave`, from the callee, joining nothing and handing out
    ///   nothing;
    /// - an `accept` (while it rings, and again once answered), the callee's own participant properties, a media
    ///   answer or a renegotiation, from the callee, joining nothing, every callback on the call agent the permitted
    ///   attach bound;
    /// - a `reject`, a media acknowledgement, media descriptions or channel parameters, naming no sender and handing
    ///   out nothing.
    pub(crate) fn fits(
        &self,
        operation: CallOperation,
        from: Option<&Mri>,
        joins: Option<&str>,
        callbacks: &[String],
    ) -> bool {
        let callee = from == Some(&self.invitation.callee);
        let plain = joins.is_none() && callbacks.is_empty();
        match operation {
            CallOperation::Attach => {
                callee && joins == Some(self.invitation.controller.as_str()) && self.agent_of(callbacks).is_some()
            }
            CallOperation::Progress
            | CallOperation::UpdateEndpointState
            | CallOperation::UpdateEndpointMetadata
            | CallOperation::Leave => callee && plain,
            CallOperation::UpdateParticipantProperties
            | CallOperation::MediaAnswer
            | CallOperation::Renegotiate
            | CallOperation::Accept => {
                callee && joins.is_none() && self.agent.is_some() && self.agent_of(callbacks).is_some()
            }
            CallOperation::Reject
            | CallOperation::MediaAcknowledge
            | CallOperation::UpdateMediaDescriptions
            | CallOperation::ApplyChannelParameters
            | CallOperation::KeepAlive => from.is_none() && plain,
            _ => false,
        }
    }

    /// Whether `operation` may act on the invitation as it stands: while it rings, its ring-time sends, its answer
    /// (the endpoint state and the `accept`) or its decline; once answered, its `accept` again and the answered call's
    /// captured operations (the endpoint's state and metadata, its own participant properties, its `leave`); once its
    /// callee left, only that `leave` again; once declined, only its `reject` again.
    pub(crate) fn open_for(&self, operation: CallOperation) -> bool {
        match self.state {
            OfferState::Ringing => matches!(
                operation,
                CallOperation::Attach
                    | CallOperation::Progress
                    | CallOperation::UpdateEndpointState
                    | CallOperation::Accept
                    | CallOperation::Reject
            ),
            OfferState::Answered => matches!(
                operation,
                CallOperation::Accept
                    | CallOperation::UpdateEndpointState
                    | CallOperation::UpdateEndpointMetadata
                    | CallOperation::UpdateParticipantProperties
                    | CallOperation::Leave
                    | CallOperation::MediaAnswer
                    | CallOperation::MediaAcknowledge
                    | CallOperation::Renegotiate
                    | CallOperation::UpdateMediaDescriptions
                    | CallOperation::ApplyChannelParameters
                    | CallOperation::KeepAlive
            ),
            OfferState::Leaving => operation == CallOperation::Leave,
            OfferState::Declined => operation == CallOperation::Reject,
            OfferState::Ended => false,
        }
    }

    /// After a permitted `accept`: answered. After a permitted `reject`: declined, so its ring ended. After the
    /// answered call's permitted `leave`: leaving.
    pub(crate) fn decided(&mut self, operation: CallOperation) {
        match (operation, self.state) {
            (CallOperation::Accept, OfferState::Ringing) => {
                self.state = OfferState::Answered;
                self.receipts.clear();
            }
            (CallOperation::Reject, OfferState::Ringing) => {
                self.state = OfferState::Declined;
                self.receipts.clear();
            }
            (CallOperation::Leave, OfferState::Answered) => self.state = OfferState::Leaving,
            _ => {}
        }
    }

    /// Binds the links a permitted `attach`'s answer hands out: of the call leg (`callInvitation.links`), on the flight
    /// proxy, `…/cc/v1/incoming/<call>/…/<op>` of the invitation's own call, its `progress`, its `acceptance`
    /// (`…/accept`) and its `callLeg` (`…/reject`); of the conversation its `join` subscribed to (the additional
    /// action's response for exactly the invitation's conversation), the `updateEndpointState` at the conversation's
    /// own path with its query. Whether anything bound.
    pub(crate) fn bind_answer(&mut self, answer: &[u8]) -> bool {
        let Some(answer) = strict_json(answer) else {
            return false;
        };
        let leg = [
            (CallOperation::Progress, "progress"),
            (CallOperation::Accept, "acceptance"),
            (CallOperation::Reject, "callLeg"),
        ];
        let mut found: Vec<Link> = leg
            .iter()
            .filter_map(|(operation, name)| {
                let url = proxy_link(answer.pointer("/callInvitation/links")?.get(*name)?)?;
                let fits = controller_path(&url).is_some_and(|(_, call, kind, last)| {
                    call == self.invitation.leg.1 && kind == "incoming" && last == operation.segment()
                });
                fits.then(|| (*operation, url.as_str().to_owned()))
            })
            .collect();
        found.extend(self.conversation_links(&answer));
        let mut bound = false;
        for (operation, url) in found {
            if !self.holds(operation, &url) {
                self.links.push((operation, url));
                bound = true;
            }
        }
        bound
    }

    /// The conversation links of an `attach` answer's `join` response for exactly the invitation's conversation: its
    /// `updateEndpointState`, the conversation's path plus the segment, with the conversation's query.
    fn conversation_links(&self, answer: &Value) -> Vec<Link> {
        let Ok(controller) = Url::parse(&self.invitation.controller) else {
            return Vec::new();
        };
        let responses = answer
            .get("additionalActionResponses")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        responses
            .iter()
            .filter(|response| {
                response.get("url").and_then(Value::as_str) == Some(self.invitation.controller.as_str())
                    && response
                        .pointer("/output/conversationController")
                        .and_then(Value::as_str)
                        == Some(self.invitation.controller.as_str())
            })
            .flat_map(|response| {
                [
                    CallOperation::UpdateEndpointState,
                    CallOperation::UpdateEndpointMetadata,
                    CallOperation::UpdateParticipantProperties,
                    CallOperation::Leave,
                ]
                .into_iter()
                .filter_map(|operation| {
                    let url = proxy_link(response.pointer("/output/links")?.get(operation.segment())?)?;
                    let fits = url.path() == format!("{}/{}", controller.path(), operation.segment())
                        && url.query() == controller.query();
                    fits.then(|| (operation, url.as_str().to_owned()))
                })
                .collect::<Vec<_>>()
            })
            .collect()
    }

    /// The one call agent all of `callbacks` are on, under this invitation's endpoint (and the agent already bound, if
    /// any); `None` when there are none or they aren't.
    fn agent_of(&self, callbacks: &[String]) -> Option<Callback> {
        let mut agent: Option<Callback> = None;
        for link in callbacks {
            let callback = parse_callback(link)?;
            let this = super::agent_of(&callback)?;
            if !this
                .path
                .starts_with(&format!("{}callAgent/", self.invitation.endpoint))
            {
                return None;
            }
            match &agent {
                Some(bound) if *bound != this => return None,
                _ => agent = Some(this),
            }
        }
        let agent = agent?;
        self.agent.as_ref().is_none_or(|bound| *bound == agent).then_some(agent)
    }

    /// After a permitted `attach`: its call agent is bound, and its `call/end` and `conversationEnd` callbacks end the
    /// ring.
    pub(crate) fn attached(&mut self, callbacks: &[String]) {
        if let Some(agent) = self.agent_of(callbacks) {
            self.agent = Some(agent);
        }
        for link in callbacks {
            let Some(callback) = parse_callback(link) else { continue };
            let ends =
                callback.path.ends_with("/call/end/") || callback.path.ends_with("/conversation/conversationEnd/");
            if ends && !self.ends.contains(&callback) {
                self.ends.push(callback);
            }
        }
    }

    /// Binds the answered call's links a permitted `accept`'s answer hands out (`callAcceptanceAcknowledgement.links`), on
    /// the flight proxy, of the invitation's own call: its `mediaRenegotiation` (`…/cc/v1/active/<call>/…/renegotiate`),
    /// its `updateMediaDescriptions` (`…/cc/v1/active/<call>/…/updateMediaDescriptions`), its `callLeg` (the keep-alive's,
    /// `…/cc/v1/active/<call>/<n>/<leg>/<n>`) and its `applyChannelParameters`
    /// (`…/cc/v1/mcProxy/<call>/…/applyChannelParameters`, a group call's). Whether anything bound.
    pub(crate) fn bind_acceptance(&mut self, answer: &[u8]) -> bool {
        const NAMES: [(CallOperation, &str, &str); 4] = [
            (CallOperation::Renegotiate, "mediaRenegotiation", "active"),
            (CallOperation::KeepAlive, "callLeg", "active"),
            (
                CallOperation::UpdateMediaDescriptions,
                "updateMediaDescriptions",
                "active",
            ),
            (
                CallOperation::ApplyChannelParameters,
                "applyChannelParameters",
                "mcProxy",
            ),
        ];
        let Some(answer) = strict_json(answer) else {
            return false;
        };
        let mut bound = false;
        for (operation, name, kind) in NAMES {
            let Some(url) = answer
                .pointer("/callAcceptanceAcknowledgement/links")
                .and_then(|links| links.get(name))
                .and_then(proxy_link)
            else {
                continue;
            };
            let fits = controller_path(&url).is_some_and(|(_, call, at, last)| {
                call == self.invitation.leg.1 && at == kind && super::link_is(operation, &url, &last)
            });
            if fits && !self.holds(operation, url.as_str()) {
                self.links.push((operation, url.as_str().to_owned()));
            }
            bound |= fits;
        }
        bound
    }

    /// Whether a delivery at `path` carries a link of the answered call's media negotiation ([`super::media_link`]): at
    /// a `mediaRenegotiation` or `mediaAnswer` callback on the call agent the permitted attach bound.
    pub(crate) fn negotiates_at(&self, path: &str) -> bool {
        self.state == OfferState::Answered
            && self
                .agent
                .as_ref()
                .is_some_and(|agent| super::is_media_callback(agent, path))
    }

    /// Binds the link [`super::media_link`] reads from a delivery at `path`, of the invitation's own call. Whether it
    /// bound.
    pub(crate) fn bind_media(&mut self, path: &str, body: &[u8]) -> bool {
        let Some(link) = super::media_link(path, body, &self.invitation.leg.1) else {
            return false;
        };
        if !self.links.contains(&link) {
            self.links.push(link);
        }
        true
    }

    /// Whether this invitation binds the conversation at exactly `url` to the group chat `chat`: its controller and
    /// the group chat it named.
    pub(crate) fn binds(&self, url: &str, chat: Option<&str>) -> bool {
        self.invitation.controller == url && chat.is_some() && self.invitation.group_chat.as_deref() == chat
    }

    /// Whether a join from `from` handing out `callbacks` fits this invitation: the account it was addressed to sends
    /// it, every callback on one call agent of the endpoint the invitation was delivered to (a new call session may
    /// take a new agent).
    pub(crate) fn fits_join(&self, from: Option<&Mri>, callbacks: &[String]) -> bool {
        let mut agents = callbacks.iter().map(|link| {
            parse_callback(link).as_ref().and_then(super::agent_of).filter(|agent| {
                agent
                    .path
                    .starts_with(&format!("{}callAgent/", self.invitation.endpoint))
            })
        });
        let first = agents.next().flatten();
        from == Some(&self.invitation.callee)
            && first.is_some()
            && agents.all(|agent| agent.is_some() && agent == first)
    }

    /// The parties the invitation names.
    pub(crate) fn parties(&self) -> BTreeSet<Mri> {
        [self.invitation.caller.clone(), self.invitation.callee.clone()]
            .into_iter()
            .collect()
    }
}

/// The invitation `delivery` is, read (see the module docs); `None` for any other delivery or an invitation that
/// doesn't read: at an endpoint's root, `evt` an invitation's, `gp` base64 of a JSON object with a caller and a callee,
/// an `attach` link and a conversation on the flight proxy, the leg's links all of one forked call, no call it
/// replaces, no group context (a meeting), and the call's kind in a captured shape: a 1:1 call's (107) names no group
/// chat and isn't multi-party (`conversationInvitation.isMultiParty` false); a group call's (109) is multi-party and
/// names its group chat by a group chat's id, the only field naming its other participants.
pub(crate) fn read_invitation(delivery: &Delivery) -> Option<Invitation> {
    let endpoint = endpoint_root(&delivery.path)?;
    let body = strict_json(&delivery.body)?;
    let event = body.get("evt")?.as_u64()?;
    if !INVITATION_EVENTS.contains(&event) {
        return None;
    }
    let gp = base64::engine::general_purpose::STANDARD
        .decode(body.get("gp")?.as_str()?)
        .ok()?;
    let gp = strict_json(&gp)?;
    let notification = gp.get("callNotification")?;
    let text = |value: &Value, pointer: &str| value.pointer(pointer).and_then(Value::as_str).map(str::to_owned);
    let unset = |pointer: &str| gp.pointer(pointer).is_none_or(Value::is_null);
    if !unset("/callNotification/replaces") || !unset("/groupContext") {
        return None;
    }
    let attach_link = proxy_link(notification.pointer("/links/attach")?)?;
    let (host, call, kind, last) = controller_path(&attach_link)?;
    if kind != "forked" || last != "attach" {
        return None;
    }
    let controller = conversation_of(&gp)?;
    let group_chat = match gp.get("groupChat") {
        None | Some(Value::Null) => None,
        Some(chat) => Some(text(chat, "/threadId").filter(|id| is_group_chat_id(id))?),
    };
    if !kind_fits(event, &gp, group_chat.is_some()) {
        return None;
    }
    Some(Invitation {
        caller: Mri::new(text(notification, "/from/id")?),
        callee: Mri::new(text(notification, "/to/id")?),
        group_chat,
        endpoint,
        controller: controller.as_str().to_owned(),
        attach: attach_link.as_str().to_owned(),
        leg: (host, call),
    })
}

/// Whether a delivery that [`read_invitation`] doesn't read may still be an invitation, so that nothing of it is handed
/// on or answered: a delivery at an endpoint's root whose body isn't a strictly read JSON object with a numeric `evt`
/// other than an invitation's ([`INVITATION_EVENTS`]), or that carries a `callNotification` (at its top, or in a `gp`
/// that decodes, one that doesn't counting as one). The root's other captured events (`evt` 115 and 116, a `cp` and no
/// `gp`) aren't.
pub(crate) fn may_be_invitation(delivery: &Delivery) -> bool {
    if endpoint_root(&delivery.path).is_none() {
        return false;
    }
    let Some(Value::Object(body)) = strict_json(&delivery.body) else {
        return true;
    };
    let event = body.get("evt").and_then(Value::as_u64);
    if event.is_none_or(|event| INVITATION_EVENTS.contains(&event)) || body.contains_key("callNotification") {
        return true;
    }
    body.get("gp").is_some_and(|gp| {
        gp.as_str()
            .and_then(|gp| base64::engine::general_purpose::STANDARD.decode(gp).ok())
            .and_then(|gp| strict_json(&gp))
            .is_none_or(|gp| !gp.is_object() || gp.get("callNotification").is_some())
    })
}

/// The conversation an invitation's decoded `gp` invites to (`conversationInvitation.conversationController`), when
/// it is a conversation on the flight proxy (`/api/v2/ep/<host>/conv/<id>`).
fn conversation_of(gp: &Value) -> Option<Url> {
    let controller = proxy_link(gp.pointer("/conversationInvitation/conversationController")?)?;
    let fits = {
        let segments: Vec<&str> = controller.path_segments()?.collect();
        matches!(segments.as_slice(), ["api", "v2", "ep", host, "conv", id] if !host.is_empty() && !id.is_empty())
    };
    fits.then_some(controller)
}

/// Whether an invitation of `event` with this decoded `gp`, naming a group chat or not (`names_chat`), is of its call's
/// kind in a captured shape (see [`read_invitation`]): a 1:1 call's names no group chat and isn't multi-party, a group
/// call's names its group chat and is (its other participants are known only through its chat: one that names none
/// doesn't read).
fn kind_fits(event: u64, gp: &Value, names_chat: bool) -> bool {
    let multi_party = gp
        .pointer("/conversationInvitation/isMultiParty")
        .and_then(Value::as_bool);
    match event {
        107 => !names_chat && multi_party == Some(false),
        109 => names_chat && multi_party == Some(true),
        _ => false,
    }
}

/// `path` when it is an endpoint's root, `/v4/f/<endpoint>/`.
fn endpoint_root(path: &str) -> Option<String> {
    let url = Url::parse(&format!("https://trouter.invalid{path}")).ok()?;
    let segments: Vec<&str> = url.path_segments()?.collect();
    match segments.as_slice() {
        ["v4", "f", endpoint, ""] if !endpoint.is_empty() && url.query().is_none() => Some(path.to_owned()),
        _ => None,
    }
}

/// The captured envelope of an invitation's delivery response: these keys, these headers, and `MS-CV` besides them or
/// not (both forms seen 2026-10-24).
const RECEIPT_ENVELOPE: [&str; 4] = ["id", "status", "headers", "body"];
const RECEIPT_HEADERS: [&str; 2] = ["trouter-request", "trouter-client"];
const RECEIPT_OPTIONAL_HEADER: &str = "MS-CV";

/// Judges an invitation's delivery response (see the module docs): the captured envelope, `trouter-request` equal to
/// the delivery's.
fn judge_receipt(envelope: &Value, trouter_request: &str) -> Result<(), DeliveryDenyReason> {
    let shape = DeliveryDenyReason::Shape;
    let object = envelope.as_object().ok_or(shape)?;
    let headers = object.get("headers").and_then(Value::as_object).ok_or(shape)?;
    let fits = object.len() == RECEIPT_ENVELOPE.len()
        && RECEIPT_ENVELOPE.iter().all(|key| object.contains_key(*key))
        && object.get("status").and_then(Value::as_u64) == Some(200)
        && object.get("body").and_then(Value::as_str) == Some("")
        && headers.iter().all(|(name, value)| {
            value.is_string() && (RECEIPT_HEADERS.contains(&name.as_str()) || name == RECEIPT_OPTIONAL_HEADER)
        })
        && RECEIPT_HEADERS.iter().all(|name| headers.contains_key(*name));
    if !fits {
        return Err(shape);
    }
    if headers.get("trouter-request").and_then(Value::as_str) == Some(trouter_request) {
        Ok(())
    } else {
        Err(DeliveryDenyReason::Uncorrelated)
    }
}
