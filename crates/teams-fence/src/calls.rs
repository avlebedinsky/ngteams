//! The call registry: the calls this run's fence permitted, and the links their services handed out for each one.
//!
//! Every fence keeps one. Under the lab's call rule (`crate::Fence::check`, `CallRule::Entry`) it decides nothing but
//! which invitations are offered and which offered invitation an answer or a decline acts on
//! ([`CallRegistry::resolve_answer`]); a personal fence decides nothing by it (`decision/audit.rs`) but which
//! invitations are offered. What it binds otherwise is every fence's bookkeeping, which the native client's call
//! machine reads its deliveries by ([`Observed`]). Under the call registry's own rule (`CallRule::Registry`, which no
//! build's fence decides by: tests turn it on with `Fence::tracking_calls`) it decides every operation on a call, as
//! follows.
//!
//! A follow-up on a call (`docs/protocol/calling-echo.md`: the endpoint's metadata, an `add`, the `leave`, the
//! acceptance's acknowledgement, a media line's channel parameters) reaches the call's participants, and no source reads
//! a call's members. So the fence trusts only the calls it permitted itself, and a follow-up passes only when it goes to
//! exactly a link the call's own services handed out for that operation. The bindings, each only as the echo call's
//! capture shows it:
//!
//! 1. **The start.** A permit [`crate::Fence::check`] issued for a call start, with the start's body, makes a *pending*
//!    call ([`crate::Fence::pend_start`]): its caller, its participants (the permit's), and the endpoint's own callback
//!    links the start named (`callInvitation.links`, `conversationRequest.links`). The permit must be this fence's and
//!    is taken once; the body is read again and must give the permit's target.
//! 2. **The start's answer.** A readable answer to that start registers the call ([`crate::Fence::register_start`],
//!    which takes the pending call's [`PendingStart`] once): `conversationController` (on the flight proxy,
//!    `/api/v2/ep/<host>/conv/<id>`) and, of its `links`, only those of a mapped operation whose URL is the controller's
//!    path plus the operation's captured segment, with the controller's query (`updateEndpointMetadata`, `leave`, and
//!    `addParticipantAndModality`, which is the `/add` the client posts to). A start answered without success ends
//!    unregistered ([`crate::Fence::abandon_start`]): nothing it named binds, so the start sent again (its own
//!    pending call, with the same callbacks) is the one an acceptance binds to.
//! 3. **The acceptance.** A delivery that arrives at exactly the path of the start's own `acceptance` callback
//!    ([`crate::Fence::observe_delivery`]) binds the call controller's links its body names (`callAcceptance.links`:
//!    `acknowledgement`, `applyChannelParameters`, `mediaRenegotiation`, `updateMediaDescriptions`, and `callLeg`, the
//!    keep-alive's link), all on the flight proxy under one call controller host and one call
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
//! A rejoin (a later start or join of this run answered with the same conversation) takes the conversation's links
//! from an ending or ended call, so they resolve to the new call alone; a live call keeps its links, and a link two
//! calls hold resolves to none. A re-invitation to that conversation answered since holds its links too (its attach's
//! answer binds them): an operation on them acts on the invitation's call while it is open for the operation, and on
//! the left call's tombstone only when no such invitation holds them.
//!
//! **The acceptance's delivery response.** The one Trouter delivery response the fence lets through: the acceptance's
//! acknowledgement, answering a pending acceptance delivery of a live call on the same connection, once
//! ([`crate::Fence::check_delivery_response`]): the captured envelope (`id`, `status` 200, exactly the headers `MS-CV`,
//! `trouter-client`, and `trouter-request`, `X-Microsoft-Skype-Chain-ID` and `X-Microsoft-Skype-Message-ID` each equal
//! to the delivery's own), and a `callAcceptanceAcknowledgement` whose links are all callbacks on the start's own
//! endpoint and call agent. Every other delivery response stays denied but an offered invitation's own ([`offer`]).
//!
//! **Offered invitations** ([`offer`]): the callee's side. An incoming call the fence would let the account answer is
//! offered: its ring-time sends pass on it (its delivery response, the `attach`, `progress`), and the user's answer or
//! decline, until its ring ends; an answered one's follow-ups pass to the links its answers handed out; and a join of
//! the running call it bound to its group chat registers that call as a start does.
//!
//! **Media negotiation.** A renegotiation or a renegotiation's answer delivered at a registered call's or an answered
//! invitation's own `mediaRenegotiation` or `mediaAnswer` callback binds the link it names (`answer`, `acknowledge`)
//! of that call.

mod offer;

pub use offer::INVITATION_EVENTS;
pub(crate) use offer::{Invitation, may_be_invitation, read_invitation};

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

/// An `attach` or an `accept` the fence permitted on an offered invitation, waiting for its answer: what
/// [`crate::Fence::pend_answer`] gives and [`crate::Fence::bind_answer`] takes, once. Only the fence that made it can
/// bind it.
#[derive(Debug)]
pub struct PendingAnswer {
    fence: u64,
    offer: usize,
    operation: CallOperation,
}

impl PendingAnswer {
    pub(crate) fn fence(&self) -> u64 {
        self.fence
    }

    /// The offer and the operation it stands for, using it up.
    fn into_parts(self) -> (usize, CallOperation) {
        (self.offer, self.operation)
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
    /// An incoming call's invitation the fence offers (or another delivery of one it offers): its ring-time sends may
    /// go.
    Offered,
    /// An incoming call's invitation the fence doesn't offer (not addressed to the account, a caller outside the lab, a
    /// ring that ended, or one it can't read): nothing may be sent for it, and it is handed on to nobody.
    Ignored,
    /// The end of an offered invitation's ring (or of its answered call): nothing passes on it any more.
    RingEnded,
    /// A media negotiation delivery of a live call or an answered invitation's (a renegotiation, an answer): the link
    /// its answer or acknowledgement goes to is bound.
    Renegotiation,
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
    /// Its call isn't live (still waiting for its answer, ending or ended), or its invitation's ring ended.
    #[error("its call isn't live")]
    NotLive,
    /// An offered group call's chat isn't lab-only by a fresh, complete member read (or can't be read), or the
    /// invitation names someone outside the lab.
    #[error("the invitation's parties or group chat aren't lab-only by a fresh, complete read")]
    Parties,
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

impl Call {
    /// Whether a delivery at `path` carries a link of this live call's media negotiation ([`media_link`]): at a
    /// `mediaRenegotiation` or `mediaAnswer` callback on the start's own call agent.
    fn negotiates_at(&self, path: &str) -> bool {
        self.state == State::Live && self.agent.as_ref().is_some_and(|agent| is_media_callback(agent, path))
    }

    /// Binds the link [`media_link`] reads from a delivery at `path`, of the call the acceptance bound. Whether it bound.
    fn bind_media(&mut self, path: &str, body: &[u8]) -> bool {
        let Some(link) = self
            .controller
            .as_ref()
            .and_then(|(_, call)| media_link(path, body, call))
        else {
            return false;
        };
        if !self.links.contains(&link) {
            self.links.push(link);
        }
        true
    }
}

/// Whether `path` is a media negotiation callback (`…/call/mediaRenegotiation/`, `…/call/mediaAnswer/`, and
/// `…/call/mediaAcknowledgement/`, where the answerer hears its answer was acknowledged) on `agent`.
fn is_media_callback(agent: &Callback, path: &str) -> bool {
    path.starts_with(&agent.path)
        && (path.ends_with("/call/mediaRenegotiation/")
            || path.ends_with("/call/mediaAnswer/")
            || is_acknowledged_at(path))
}

/// Whether `path` is a media acknowledgement callback (`…/call/mediaAcknowledgement/`): it binds no link, its
/// delivery waits only for its receipt.
fn is_acknowledged_at(path: &str) -> bool {
    path.ends_with("/call/mediaAcknowledgement/")
}

/// The link a media negotiation delivery at `path` hands out for the call `call` (the call controller's call id), on
/// the flight proxy: a renegotiation's (at a `mediaRenegotiation` callback) `mediaNegotiation.links.mediaAnswer`,
/// `…/cc/v1/negotiations/<call>/…/answer`; an answer's (at a `mediaAnswer` callback)
/// `mediaAnswer.links.mediaAcknowledgement`, `…/cc/v1/mediaAnswers/<call>/…/acknowledge`. `None` for anything else.
fn media_link(path: &str, body: &[u8], call: &str) -> Option<Link> {
    let (operation, pointer, kind) = if path.ends_with("/call/mediaRenegotiation/") {
        (
            CallOperation::MediaAnswer,
            "/mediaNegotiation/links/mediaAnswer",
            "negotiations",
        )
    } else if path.ends_with("/call/mediaAnswer/") {
        (
            CallOperation::MediaAcknowledge,
            "/mediaAnswer/links/mediaAcknowledgement",
            "mediaAnswers",
        )
    } else {
        return None;
    };
    let url = proxy_link(strict_json(body)?.pointer(pointer)?)?;
    let (_, at, of, last) = controller_path(&url)?;
    (at == call && of == kind && last == operation.segment()).then(|| (operation, url.as_str().to_owned()))
}

/// The registry. One per fence.
#[derive(Default)]
pub(crate) struct CallRegistry {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    calls: Vec<Call>,
    offers: Vec<offer::Offer>,
    /// Renegotiation deliveries (and their answers') waiting for their receipt.
    receipts: Vec<NegotiationReceipt>,
    /// The serials of the permits already taken by [`CallRegistry::pend`].
    taken: HashSet<u64>,
}

impl Inner {
    /// Whether the account is in a call or one rings for it: a registered call that is live, an offered invitation
    /// answered and not left, or one that rings and whose `attach` bound where its ring ends (so its end is seen). A
    /// start still waiting for its answer doesn't count (one whose answer never came would count for the rest of the
    /// run; the call machine takes no invitation meanwhile), nor an offer never attached, nor one the call machine let
    /// go before it rang ([`CallRegistry::let_go`] ends it).
    fn busy(&self) -> bool {
        self.calls.iter().any(|call| call.state == State::Live)
            || self.offers.iter().any(|offer| match offer.state {
                offer::OfferState::Answered => true,
                offer::OfferState::Ringing => offer.ring_end_known(),
                _ => false,
            })
    }
}

impl fmt::Debug for CallRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CallRegistry")
            .field("calls", &self.lock().calls.len())
            .finish()
    }
}

/// What a call operation resolved to: the call's participants (with the group chat to read fresh, for an offered group
/// call's invitation), and what to move on once it is permitted.
pub(crate) struct Resolved {
    pub(crate) participants: BTreeSet<Mri>,
    /// The group chat an offered invitation names: every decision on it reads it fresh.
    pub(crate) chat: Option<String>,
    subject: Subject,
}

impl Resolved {
    /// An operation bound to nothing the registry moves on: judged by `participants` and the group chat `chat` alone
    /// (the lab's join, read by the chat it names, and its `add`, by its invitees).
    pub(crate) fn unbound(participants: BTreeSet<Mri>, chat: Option<String>) -> Self {
        Self {
            participants,
            chat,
            subject: Subject::Unbound,
        }
    }
}

/// What a resolved operation acts on.
enum Subject {
    /// A registered call; `leave` when the operation is its `leave`.
    Call { index: usize, leave: bool },
    /// An offered invitation, and the callbacks the operation hands out.
    Offer {
        index: usize,
        operation: CallOperation,
        callbacks: Box<[String]>,
    },
    /// Nothing to move on: a join of a running call (bound to its chat by an invitation under the registry's rule, by
    /// the chat it names under the lab's; a join with media pends as a start does, `crate::Fence::pend_start`), or
    /// the lab's `add`.
    Unbound,
}

/// A renegotiation delivered at a live call's (or an answered invitation's) own `mediaRenegotiation` or `mediaAnswer`
/// callback, waiting for its receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
struct NegotiationReceipt {
    connection: String,
    id: i64,
    trouter_request: String,
    of: Negotiated,
}

/// What a renegotiation delivery was of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Negotiated {
    Call(usize),
    Offer(usize),
}

/// A delivery response the registry found waiting (see [`CallRegistry::respond`]).
pub(crate) enum Response {
    /// A renegotiation's (or its answer's) receipt, decided: the delivery's id, or why it is denied.
    NegotiationReceipt(Result<i64, DeliveryDenyReason>),
    /// A call's acceptance acknowledgement, decided: the delivery's id, or why it is denied.
    Acknowledgement(Result<i64, DeliveryDenyReason>),
    /// An offered invitation's delivery response in its captured shape, its receipt taken: still to be judged by the
    /// invitation's parties and group chat, and then only while it still rings ([`CallRegistry::still_ringing`]).
    Receipt {
        delivery: i64,
        offer: usize,
        participants: BTreeSet<Mri>,
        chat: Option<String>,
    },
    /// An offered invitation's delivery response, denied.
    ReceiptDenied(DeliveryDenyReason),
}

impl CallRegistry {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether the account holds the call floor ([`crate::CallFloor`]): a registered call that is live, or an
    /// invitation it offered answered and not left or still ringing (`offer::Offer::holds_floor`: one never attached
    /// for a ring's length at most; one the call machine lets go is retired by [`CallRegistry::let_go`]). So two
    /// accounts never ring at once: the second invitation isn't offered while the first rings.
    pub(crate) fn holds_floor(&self) -> bool {
        let inner = self.lock();
        inner.calls.iter().any(|call| call.state == State::Live) || inner.offers.iter().any(offer::Offer::holds_floor)
    }

    /// A pending call from a call start permit (its `serial`, never taken before) and the start's evidence; `None`
    /// when the permit was taken already.
    pub(crate) fn pend(&self, fence: u64, serial: u64, target: &Target, start: &StartEvidence) -> Option<PendingStart> {
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
            caller: start.caller.clone(),
            // a join adds nobody: its call's participants are the one who joined (the chat was read when it was judged)
            participants: target.added().iter().chain([&start.caller]).cloned().collect(),
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
        retire(&mut inner.calls, index, &links);
        let call = inner.calls.get_mut(index).ok_or(RegisterError::NotPending)?;
        call.links.extend(links);
        call.state = State::Live;
        Ok(())
    }

    /// Ends the pending call unregistered: its start was answered without success (see the module docs).
    pub(crate) fn abandon(&self, pending: PendingStart) {
        let index = pending.into_call();
        let mut inner = self.lock();
        if let Some(call) = inner.calls.get_mut(index).filter(|call| call.state == State::Pending) {
            call.state = State::Ended;
            call.deliveries.clear();
        }
    }

    /// A pending answer from an `attach` or an `accept` permit (its `serial`, never taken before) for the invitation
    /// holding `url` for that `operation` (still ringing for an `attach`; answered for an `accept`, which its permit
    /// answered); `None` when the permit was taken already or no such invitation holds it.
    pub(crate) fn pend_answer(
        &self,
        fence: u64,
        serial: u64,
        operation: CallOperation,
        url: &str,
    ) -> Option<PendingAnswer> {
        let mut inner = self.lock();
        let state = match operation {
            CallOperation::Attach => offer::OfferState::Ringing,
            CallOperation::Accept => offer::OfferState::Answered,
            _ => return None,
        };
        let offer = inner
            .offers
            .iter()
            .position(|offer| offer.state == state && offer.holds(operation, url))?;
        inner.taken.insert(serial).then_some(PendingAnswer {
            fence,
            offer,
            operation,
        })
    }

    /// Binds the links the pending answer hands out to its invitation: an `attach`'s while the invitation rings
    /// ([`offer::Offer::bind_answer`]), an `accept`'s once it was answered ([`offer::Offer::bind_acceptance`]).
    pub(crate) fn bind_answer(&self, pending: PendingAnswer, answer: &[u8]) -> Result<(), RegisterError> {
        let (offer, operation) = pending.into_parts();
        let mut inner = self.lock();
        let offer = inner.offers.get_mut(offer).ok_or(RegisterError::NotPending)?;
        let bound = match (operation, offer.state) {
            (CallOperation::Attach, offer::OfferState::Ringing) => offer.bind_answer(answer),
            (CallOperation::Accept, offer::OfferState::Answered) => offer.bind_acceptance(answer),
            _ => return Err(RegisterError::NotPending),
        };
        if bound { Ok(()) } else { Err(RegisterError::Unreadable) }
    }

    /// Retires the ringing invitation whose `attach` link is exactly `attach`: the call machine let it go before it rang
    /// here (its attach failed for good, its answer didn't read, its media failed), so its ring ends here for good. The
    /// service may never have taken the callbacks that attach handed out (a credential failure, a refusal), so no
    /// delivery at its `call/end` or `conversationEnd` would ever end the ring; ended, it holds no call open
    /// ([`Inner::busy`]) and nothing passes on it any more. It only ends an invitation: an answered one moves nothing.
    pub(crate) fn let_go(&self, attach: &str) {
        let mut inner = self.lock();
        for offer in inner
            .offers
            .iter_mut()
            .filter(|offer| offer.state == offer::OfferState::Ringing && offer.holds(CallOperation::Attach, attach))
        {
            offer.end();
        }
    }

    /// Offers `invitation`, delivered as `delivery` (the fence decided it may be offered): a new offer, or another
    /// delivery of one already offered, whose response then waits too. An invitation whose ring ended stays ignored,
    /// and so does a new one while the account is busy with a call when `one_at_a_time` (the personal build's rule:
    /// [`Inner::busy`]).
    pub(crate) fn offer(&self, invitation: Invitation, delivery: &Delivery, one_at_a_time: bool) -> Observed {
        let mut inner = self.lock();
        if let Some(offer) = inner.offers.iter_mut().find(|offer| offer.is_redelivery(&invitation)) {
            if offer.state == offer::OfferState::Ended {
                return Observed::Ignored;
            }
            offer.expect_receipt(delivery);
            return Observed::Offered;
        }
        if one_at_a_time && inner.busy() {
            return Observed::Ignored;
        }
        let mut offer = offer::Offer::new(invitation);
        offer.expect_receipt(delivery);
        inner.offers.push(offer);
        Observed::Offered
    }

    /// Whether `invitation` was offered already and its ring hasn't ended: another delivery of it is still taken.
    pub(crate) fn offers(&self, invitation: &Invitation) -> bool {
        self.lock()
            .offers
            .iter()
            .any(|offer| offer.is_redelivery(invitation) && offer.state != offer::OfferState::Ended)
    }

    /// What `delivery` does (see the module docs).
    pub(crate) fn observe(&self, delivery: &Delivery) -> Observed {
        let mut inner = self.lock();
        if let Some(offer) = inner
            .offers
            .iter_mut()
            .find(|offer| offer.state != offer::OfferState::Ended && offer.ends_at(&delivery.path))
        {
            offer.end();
            return Observed::RingEnded;
        }
        let negotiated = if let Some((index, offer)) = inner
            .offers
            .iter_mut()
            .enumerate()
            .find(|(_, offer)| offer.negotiates_at(&delivery.path))
        {
            let bound = is_acknowledged_at(&delivery.path) || offer.bind_media(&delivery.path, &delivery.body);
            Some((bound, Negotiated::Offer(index)))
        } else {
            inner
                .calls
                .iter_mut()
                .enumerate()
                .find(|(_, call)| call.negotiates_at(&delivery.path))
                .map(|(index, call)| {
                    let bound = is_acknowledged_at(&delivery.path) || call.bind_media(&delivery.path, &delivery.body);
                    (bound, Negotiated::Call(index))
                })
        };
        if let Some((bound, of)) = negotiated {
            if !bound {
                return Observed::Nothing;
            }
            // its receipt may go, once, in the captured shape (a delivery without the header it echoes gets none)
            if let Some(trouter_request) = &delivery.trouter_request {
                inner.receipts.push(NegotiationReceipt {
                    connection: delivery.connection.clone(),
                    id: delivery.id,
                    trouter_request: trouter_request.clone(),
                    of,
                });
            }
            return Observed::Renegotiation;
        }
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
        let mut inner = self.lock();
        for call in &mut inner.calls {
            call.deliveries.retain(|delivery| delivery.connection != connection);
        }
        for offer in &mut inner.offers {
            offer.close_connection(connection);
        }
        inner.receipts.retain(|receipt| receipt.connection != connection);
    }

    /// The call `target` (a [`Scope::CallOperation`]) is an operation on, or why it is none (see the module docs).
    pub(crate) fn resolve(&self, target: &Target) -> Result<Resolved, DenyReason> {
        let Scope::CallOperation {
            operation,
            url,
            from,
            callbacks,
            joins,
        } = target.scope()
        else {
            return Err(DenyReason::CallMismatch);
        };
        let inner = self.lock();
        let on_a_call = held_by_a_call(&inner, *operation, url);
        if *operation == CallOperation::Join {
            return resolve_join(&inner.offers, target);
        }
        if !on_a_call {
            return resolve_on_offer(&inner.offers, target, *operation, url);
        }
        if joins.is_some() {
            return Err(DenyReason::CallMismatch);
        }
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
        let names_people = matches!(
            operation,
            CallOperation::Add | CallOperation::Admit | CallOperation::RemoveParticipant
        );
        let added_fits = names_people != target.added().is_empty();
        let hands_callbacks = matches!(
            operation,
            CallOperation::Acknowledge
                | CallOperation::Add
                | CallOperation::Admit
                | CallOperation::RemoveParticipant
                | CallOperation::Renegotiate
                | CallOperation::MediaAnswer
        );
        let callbacks_fit = (hands_callbacks || callbacks.is_empty())
            && callbacks
                .iter()
                .all(|link| call.agent.as_ref().is_some_and(|agent| agent.owns(link)));
        if target.kind() != operation.kind() || !sender_fits || !added_fits || !callbacks_fit {
            return Err(DenyReason::CallMismatch);
        }
        Ok(Resolved {
            participants: call.participants.clone(),
            chat: None,
            subject: Subject::Call {
                index,
                leave: *operation == CallOperation::Leave,
            },
        })
    }

    /// The lab's resolution of an answer (`accept`) or a decline (`reject`), `target`: the invitation this registry
    /// offered whose call leg (`…/cc/v1/incoming/<call>/…`, the leg its `attach` link named) the link names, open for
    /// the operation (the latest such one); the write of its kind, adding nobody, joining nothing, an answer from the
    /// invitation's callee and a decline naming no sender. No link of the invitation's need be bound: the leg is what
    /// ties it to the invitation. [`PartialReason::UnregisteredCall`] when no offered invitation names the leg,
    /// [`PartialReason::CallEnded`] when none naming it is open for the operation any more.
    pub(crate) fn resolve_answer(&self, target: &Target) -> Result<Resolved, DenyReason> {
        let Scope::CallOperation {
            operation,
            url,
            from,
            callbacks,
            joins,
        } = target.scope()
        else {
            return Err(DenyReason::CallMismatch);
        };
        let leg = Url::parse(url)
            .ok()
            .as_ref()
            .and_then(controller_path)
            .filter(|(_, _, kind, last)| kind == "incoming" && last == operation.segment())
            .map(|(_, call, _, _)| call)
            .ok_or(DenyReason::CallMismatch)?;
        let sender_fits = match operation {
            CallOperation::Accept => true,
            CallOperation::Reject => from.is_none(),
            _ => false,
        };
        if target.kind() != operation.kind() || !target.added().is_empty() || joins.is_some() || !sender_fits {
            return Err(DenyReason::CallMismatch);
        }
        let inner = self.lock();
        let mut named = inner
            .offers
            .iter()
            .enumerate()
            .filter(|(_, offer)| offer.invitation.leg.1 == leg)
            .peekable();
        if named.peek().is_none() {
            return Err(DenyReason::Partial(PartialReason::UnregisteredCall));
        }
        let (index, offer) = named
            .rfind(|(_, offer)| offer.open_for(*operation))
            .ok_or(DenyReason::Partial(PartialReason::CallEnded))?;
        if *operation == CallOperation::Accept && from.as_ref() != Some(&offer.invitation.callee) {
            return Err(DenyReason::CallMismatch);
        }
        Ok(Resolved {
            participants: offer.parties(),
            chat: offer.invitation.group_chat.clone(),
            subject: Subject::Offer {
                index,
                operation: *operation,
                callbacks: callbacks.clone(),
            },
        })
    }

    /// What the lab's fence permitted without a lookup does to the registry: when it resolves as the registry's own
    /// rule would ([`Self::resolve`]), what it acts on moves as after a permit ([`Self::permitted`]: a `leave` ends its
    /// call's or its answered invitation's part, an `attach` binds the callbacks its ring ends at); otherwise nothing.
    pub(crate) fn note(&self, target: &Target) {
        if let Ok(resolved) = self.resolve(target) {
            self.permitted(&resolved);
        }
    }

    /// Whether what `resolved` acts on may still be acted on once its group chat was read: an invitation still
    /// ringing (a ring can end during the read). A call's operation needs no read, so it is still as resolved.
    pub(crate) fn still_open(&self, resolved: &Resolved) -> bool {
        match &resolved.subject {
            Subject::Call { .. } | Subject::Unbound => true,
            Subject::Offer { index, operation, .. } => self
                .lock()
                .offers
                .get(*index)
                .is_some_and(|offer| offer.open_for(*operation)),
        }
    }

    /// After a permitted operation: a `leave` moves its call to ending; an `attach` binds its invitation's call agent
    /// and the callbacks its ring ends at.
    pub(crate) fn permitted(&self, resolved: &Resolved) {
        let mut inner = self.lock();
        match &resolved.subject {
            Subject::Call { index, leave: true } => {
                if let Some(call) = inner.calls.get_mut(*index)
                    && call.state == State::Live
                {
                    call.state = State::Ending;
                    call.deliveries.clear();
                }
            }
            Subject::Call { leave: false, .. } | Subject::Unbound => {}
            Subject::Offer {
                index,
                operation,
                callbacks,
            } => {
                if let Some(offer) = inner.offers.get_mut(*index) {
                    if *operation == CallOperation::Attach {
                        offer.attached(callbacks);
                    }
                    offer.decided(*operation);
                }
            }
        }
    }

    /// Whether the offered invitation `offer` still rings.
    pub(crate) fn still_ringing(&self, offer: usize) -> bool {
        self.lock()
            .offers
            .get(offer)
            .is_some_and(|offer| offer.state == offer::OfferState::Ringing)
    }

    /// Finds the delivery a response sent on `connection` answers (see the module docs) and takes it: a call's
    /// acceptance, decided; an offered invitation's, judged by its envelope and left for the fence to judge by the
    /// invitation's parties. `None` when no pending delivery there has its id.
    pub(crate) fn respond(&self, connection: &str, response: &[u8]) -> Option<Response> {
        let envelope = strict_json(response)?;
        let id = envelope.get("id")?.as_i64()?;
        let mut inner = self.lock();
        if let Some(at) = inner
            .receipts
            .iter()
            .position(|receipt| receipt.connection == connection && receipt.id == id)
        {
            let receipt = inner.receipts.remove(at);
            let live = match receipt.of {
                Negotiated::Call(index) => inner.calls.get(index).is_some_and(|call| call.state == State::Live),
                Negotiated::Offer(index) => inner
                    .offers
                    .get(index)
                    .is_some_and(|offer| offer.state == offer::OfferState::Answered),
            };
            let judged = if live {
                offer::judge_receipt(&envelope, &receipt.trouter_request).map(|()| id)
            } else {
                Err(DeliveryDenyReason::NotLive)
            };
            return Some(Response::NegotiationReceipt(judged));
        }
        if let Some((index, offer)) = inner
            .offers
            .iter_mut()
            .enumerate()
            .find(|(_, offer)| offer.awaits(connection, id))
        {
            let judged = offer.take_receipt(connection, id, &envelope);
            return Some(match (judged, offer.state) {
                (Err(reason), _) => Response::ReceiptDenied(reason),
                (Ok(()), offer::OfferState::Ringing) => Response::Receipt {
                    delivery: id,
                    offer: index,
                    participants: offer.parties(),
                    chat: offer.invitation.group_chat.clone(),
                },
                (Ok(()), _) => Response::ReceiptDenied(DeliveryDenyReason::NotLive),
            });
        }
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
            return Some(Response::Acknowledgement(Err(DeliveryDenyReason::NotLive)));
        }
        Some(Response::Acknowledgement(
            judge_response(&envelope, &delivery, call.agent.as_ref()).map(|()| id),
        ))
    }
}

/// Whether a registered call holds `url` for `operation` and an operation on it acts on that call
/// ([`CallRegistry::resolve`]): a live call holding it, or any call holding it while no offered invitation open for the
/// operation holds it too. A call the account left (ending or ended) keeps its conversation's links as a tombstone, and
/// a re-invitation to that conversation answered since holds them alike (its attach's answer binds them,
/// [`offer::Offer::bind_answer`]): the operation then acts on the invitation's call, not on the tombstone.
fn held_by_a_call(inner: &Inner, operation: CallOperation, url: &str) -> bool {
    let holds = |call: &Call| call.links.iter().any(|(held, link)| *held == operation && link == url);
    let on_an_open_offer = || {
        inner
            .offers
            .iter()
            .any(|offer| offer.holds(operation, url) && offer.open_for(operation))
    };
    inner.calls.iter().any(|call| call.state == State::Live && holds(call))
        || (inner.calls.iter().any(holds) && !on_an_open_offer())
}

/// A rejoin: the call `new` registers with `links` its conversation handed out, which an earlier call of the run (a
/// session of the same conversation the account left) may hold too. An ending or ended call gives them up, so each
/// resolves to the new call alone; a live one keeps them, and a link two live calls hold resolves to neither.
fn retire(calls: &mut [Call], new: usize, links: &[Link]) {
    for (index, call) in calls.iter_mut().enumerate() {
        if index != new && matches!(call.state, State::Ending | State::Ended) {
            call.links.retain(|link| !links.contains(link));
        }
    }
}

/// [`CallRegistry::resolve`] of a join ([`CallOperation::Join`]): an invitation this fence offered binds the call's
/// conversation (its controller, exactly the join's URL) to the group chat it names (the join's `joins`), whatever
/// its ring did since; the sender is the account the invitation was addressed to, every callback it hands out is on
/// that account's endpoint, and the join adds nobody. The chat is then read fresh (`crate::Fence::check`).
fn resolve_join(offers: &[offer::Offer], target: &Target) -> Result<Resolved, DenyReason> {
    let Scope::CallOperation {
        url,
        from,
        callbacks,
        joins,
        ..
    } = target.scope()
    else {
        return Err(DenyReason::CallMismatch);
    };
    let bound = offers
        .iter()
        .find(|offer| offer.binds(url, joins.as_deref()))
        .ok_or(DenyReason::Partial(PartialReason::UnregisteredCall))?;
    if target.kind() != CallOperation::Join.kind()
        || !target.added().is_empty()
        || !bound.fits_join(from.as_ref(), callbacks)
    {
        return Err(DenyReason::CallMismatch);
    }
    Ok(Resolved {
        participants: from.iter().cloned().collect(),
        chat: joins.clone(),
        subject: Subject::Unbound,
    })
}

/// [`CallRegistry::resolve`] of an operation on an offered invitation: the latest offer holding exactly `url` for
/// `operation` that is open for the operation in its state ([`offer::Offer::open_for`]; one closed for it is a
/// tombstone, [`PartialReason::CallEnded`] when no other holds the link), the write of its kind, adding nobody, and
/// fitting the invitation ([`offer::Offer::fits`]).
fn resolve_on_offer(
    offers: &[offer::Offer],
    target: &Target,
    operation: CallOperation,
    url: &str,
) -> Result<Resolved, DenyReason> {
    let Scope::CallOperation {
        from, callbacks, joins, ..
    } = target.scope()
    else {
        return Err(DenyReason::CallMismatch);
    };
    let mut holding = offers
        .iter()
        .enumerate()
        .filter(|(_, offer)| offer.holds(operation, url))
        .peekable();
    if holding.peek().is_none() {
        return Err(DenyReason::Partial(PartialReason::UnregisteredCall));
    }
    // the invitations of one conversation (a re-invitation after a decline or a leave) hold its links alike: one
    // closed for the operation keeps them only as a tombstone, so the operation acts on the latest one open for it
    let (index, offer) = holding
        .rfind(|(_, offer)| offer.open_for(operation))
        .ok_or(DenyReason::Partial(PartialReason::CallEnded))?;
    let fits = target.kind() == operation.kind()
        && target.added().is_empty()
        && offer.fits(operation, from.as_ref(), joins.as_deref(), callbacks);
    if !fits {
        return Err(DenyReason::CallMismatch);
    }
    Ok(Resolved {
        participants: offer.parties(),
        chat: offer.invitation.group_chat.clone(),
        subject: Subject::Offer {
            index,
            operation,
            callbacks: callbacks.clone(),
        },
    })
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
    const NAMES: [(CallOperation, &str); 5] = [
        (CallOperation::UpdateEndpointMetadata, "updateEndpointMetadata"),
        (CallOperation::Add, "addParticipantAndModality"),
        (CallOperation::Leave, "leave"),
        (CallOperation::Admit, "admit"),
        (CallOperation::RemoveParticipant, "removeParticipant"),
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
    const NAMES: [(CallOperation, &str, &str); 5] = [
        (CallOperation::Acknowledge, "acknowledgement", "callAcceptance"),
        (CallOperation::KeepAlive, "callLeg", "active"),
        (
            CallOperation::ApplyChannelParameters,
            "applyChannelParameters",
            "mcProxy",
        ),
        (CallOperation::Renegotiate, "mediaRenegotiation", "active"),
        (
            CallOperation::UpdateMediaDescriptions,
            "updateMediaDescriptions",
            "active",
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
            let fits = (host, call) == controller && at == *kind && link_is(*operation, &url, &last);
            fits.then(|| (*operation, url.as_str().to_owned()))
        })
        .collect();
    Some((controller, bound))
}

/// Whether a call controller link ending in `last` is `operation`'s (its kind already checked): its captured segment
/// last, or for a keep-alive the call leg's own link (`…/cc/v1/active/<call>/<n>/<leg>/<n>`, nothing after it).
pub(crate) fn link_is(operation: CallOperation, url: &Url, last: &str) -> bool {
    if operation == CallOperation::KeepAlive {
        return url.path_segments().is_some_and(|segments| segments.count() == 11);
    }
    last == operation.segment()
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
