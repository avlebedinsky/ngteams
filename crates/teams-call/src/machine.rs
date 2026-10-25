//! The machine ([`Calls`]): one call at a time, from the account's action or an invitation to its end.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use base64::Engine as _;
use teams_fence::{Delivery, Mri, Observed};
use teams_proto::{
    AcceptanceAcknowledgement, CallContext, CallLink, CallParticipant, ClientInfo, InvitationReceipt, MediaContent,
};

use crate::agent::{CallAgent, Kind, guid};
use crate::call::{Acknowledgement, Call, Ending, KeepAlive, Phase, Side, Step};
use crate::read::{self, Invitation, RemoteEnd};
use crate::{CallWrite, WriteId, build};

/// The account as its calls name it: its MRI and name, the calling endpoint it registered (`endpointId`, the
/// calling socket's `epid`), its language, its tenant, what the client says of itself, and the account's region and
/// partition (the call requests' `MS-Teams-Region` and `MS-Teams-Partition`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub mri: Mri,
    pub display_name: Option<String>,
    pub endpoint_id: String,
    pub language_id: Option<String>,
    pub tenant_id: String,
    pub client: ClientInfo,
    pub region: Option<String>,
    pub partition: Option<String>,
}

/// One call of the machine's life.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CallId(u64);

impl CallId {
    /// The number, for a log line.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}

/// What the account calls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Placement {
    /// The test call: a conversation of the account alone, the echo service `echo` added once the media controller
    /// accepted it (`docs/protocol/calling-echo.md`).
    TestCall { echo: Mri },
    /// A call from a chat: a 1:1 chat's one callee (`group_chat` `None`), or a group chat and its other members.
    Chat {
        callees: Vec<Mri>,
        group_chat: Option<String>,
    },
}

/// Where a call is, as the account is told.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallState {
    /// The account's start or join went out.
    Placing,
    /// The account's call rings at its callees.
    Ringing,
    /// The account's join of a running call was taken; its acceptance comes next.
    Joining,
    /// An invitation rings here ([`CallEvent::Incoming`]).
    Offered,
    /// The account's answer went out.
    Answering,
    /// The account's decline went out.
    Declining,
    /// The call is up, as signaling goes (the media comes up on its own).
    Connected,
    /// The account leaves.
    Leaving,
}

/// How a call ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum End {
    /// The account hung up (or cancelled its call while it rang).
    HungUp,
    /// The account declined the invitation.
    Declined,
    /// The call's services ended it, as delivered.
    Remote(RemoteEnd),
    /// A step failed: the write named didn't go or wasn't taken. The machine ended the call (and sent a `leave` when
    /// the account was in it).
    Failed(&'static str),
}

/// An invitation that rings here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Incoming {
    pub call: CallId,
    pub caller: Mri,
    /// The caller's name as the invitation gives it (no profile lookup).
    pub caller_name: Option<String>,
    /// The group chat of a group call.
    pub group_chat: Option<String>,
    /// The caller's (or a group call's mixer's) offer.
    pub offer: Option<MediaContent>,
}

/// What the account is told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallEvent {
    /// An invitation rings here: answer it ([`Calls::answer`]) or decline it ([`Calls::decline`]); nothing answers it
    /// otherwise.
    Incoming(Incoming),
    /// An offered invitation's ring ended before the account answered or declined it (the caller cancelled, someone
    /// answered elsewhere, it timed out: as delivered). The call is over here.
    RingEnded { call: CallId, end: RemoteEnd },
    /// The call moved on.
    State { call: CallId, state: CallState },
    /// The answer to the account's offer (a placed or joined call's acceptance).
    RemoteMedia { call: CallId, media: MediaContent },
    /// The call ended. Every call shown ends with this or [`CallEvent::RingEnded`], once.
    Ended { call: CallId, end: End },
    /// The keep-alive interval the call's services asked for has passed again. Nothing is sent for it: no capture
    /// shows the keep-alive request yet (the plan's ➕ in Task 16).
    KeepAliveDue { call: CallId },
}

/// The Trouter delivery a response answers: its connection (the fence's key for it) and its id.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DeliveryKey {
    pub connection: String,
    pub id: i64,
}

impl DeliveryKey {
    fn of(delivery: &Delivery) -> Self {
        Self {
            connection: delivery.connection.clone(),
            id: delivery.id,
        }
    }
}

/// A typed delivery response, to go on the connection the delivery came on (through that delivery's responder).
#[derive(Clone, Debug)]
pub enum DeliveryReply {
    /// An offered invitation's receipt.
    Receipt(InvitationReceipt),
    /// The acknowledgement of a placed or joined call's acceptance.
    Acknowledgement(AcceptanceAcknowledgement),
}

/// What the engine does for the machine, in order.
#[derive(Clone, Debug)]
pub enum Effect {
    /// Send `write` (fence first), and report its outcome under `id` ([`Calls::on_outcome`]).
    Send { id: WriteId, write: Box<CallWrite> },
    /// Answer `delivery` with `reply`; report a failure ([`Calls::on_response_failed`]).
    Respond {
        delivery: DeliveryKey,
        reply: DeliveryReply,
    },
    /// Tell the account.
    Event(CallEvent),
}

/// How a write went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The service answered with `status` and `body`.
    Answered { status: u16, body: Vec<u8> },
    /// It wasn't sent (the fence denied it, the transport refused it) or its outcome isn't known (a timeout, a broken
    /// connection). It is never sent again.
    Failed,
}

impl Outcome {
    /// The body of a `2xx` answer.
    fn body(&self) -> Option<&[u8]> {
        match self {
            Self::Answered { status, body } if (200..300).contains(status) => Some(body),
            _ => None,
        }
    }
}

/// Why an action of the account's was refused. Nothing was sent.
#[derive(Debug, thiserror::Error)]
pub enum CallError {
    /// A call is up (one call at a time).
    #[error("a call is already up")]
    Busy,
    /// The calling endpoint's push URL isn't known yet ([`Calls::set_push_url`]).
    #[error("the calling endpoint isn't up yet")]
    NoEndpoint,
    /// No such call is up.
    #[error("no such call is up")]
    NoSuchCall,
    /// The call can't take that action where it is.
    #[error("the call can't do that now")]
    NotNow,
    /// No running call is known in that chat.
    #[error("no running call is known in that chat")]
    NotJoinable,
    /// The write isn't one the fence maps.
    #[error(transparent)]
    Write(#[from] teams_proto::CallWriteError),
    /// The system's random source failed.
    #[error("the system's random source failed: {0}")]
    Random(getrandom::Error),
}

impl From<getrandom::Error> for CallError {
    fn from(error: getrandom::Error) -> Self {
        Self::Random(error)
    }
}

/// The effects waiting for the engine, and the write ids handed out.
#[derive(Debug, Default)]
struct Out {
    effects: VecDeque<Effect>,
    next_write: u64,
}

impl Out {
    fn send(&mut self, call: &mut Call, step: Step, write: CallWrite) -> WriteId {
        self.next_write += 1;
        let id = WriteId(self.next_write);
        call.writes.insert(id, step);
        self.effects.push_back(Effect::Send {
            id,
            write: Box::new(write),
        });
        id
    }

    /// `writes` one after another: the first now, each next only once the one before it was answered `2xx`
    /// ([`Self::release`]). A step that fails ends the call, and the writes after it never go.
    fn send_in_turn(&mut self, call: &mut Call, writes: Vec<(Step, CallWrite)>) {
        let mut writes = writes.into_iter();
        let Some((step, write)) = writes.next() else { return };
        let first = self.send(call, step, write);
        call.held = writes.collect();
        call.held_after = (!call.held.is_empty()).then_some(first);
    }

    /// The write `answered` was answered `2xx`: the call's next held write goes, if it waited for that one.
    fn release(&mut self, call: &mut Call, answered: WriteId) {
        if call.held_after != Some(answered) {
            return;
        }
        call.held_after = None;
        if let Some((step, write)) = call.held.pop_front() {
            let next = self.send(call, step, write);
            call.held_after = (!call.held.is_empty()).then_some(next);
        }
    }

    /// The call's writes that weren't handed out yet, and those held, never go: the call leaves or is over. A
    /// `leave` already queued still goes.
    fn drop_queued(&mut self, call: &mut Call) {
        call.held.clear();
        call.held_after = None;
        let writes = &mut call.writes;
        self.effects.retain(|effect| {
            let Effect::Send { id, .. } = effect else {
                return true;
            };
            match writes.get(id) {
                None | Some(Step::Leave) => true,
                Some(_) => {
                    writes.remove(id);
                    false
                }
            }
        });
    }

    fn event(&mut self, event: CallEvent) {
        self.effects.push_back(Effect::Event(event));
    }

    fn state(&mut self, call: &Call) {
        if let Some(state) = call.phase.state() {
            self.event(CallEvent::State { call: call.id, state });
        }
    }

    fn respond(&mut self, delivery: DeliveryKey, reply: DeliveryReply) {
        self.effects.push_back(Effect::Respond { delivery, reply });
    }
}

/// The calls of one account: one at a time (see the crate docs).
#[derive(Debug)]
pub struct Calls {
    identity: Identity,
    agent: Option<CallAgent>,
    call: Option<Call>,
    /// The running calls invitations named, by their group chat: the conversation a join goes to. An entry goes when
    /// its conversation's end is delivered, when a join to it fails, or when a newer invitation in the chat names
    /// another.
    running: BTreeMap<String, CallLink>,
    next_call: u64,
    out: Out,
}

impl Calls {
    #[must_use]
    pub fn new(identity: Identity) -> Self {
        Self {
            identity,
            agent: None,
            call: None,
            running: BTreeMap::new(),
            next_call: 0,
            out: Out::default(),
        }
    }

    /// The calling endpoint's push URL (its `trouter.connected`'s `surl`): calls from now on hand out callbacks under
    /// a call agent of their own there. A call that is up keeps its own.
    ///
    /// # Errors
    /// [`CallError::NoEndpoint`] for a push URL that isn't `https://<host>/v4/f/<endpoint>/`.
    pub fn set_push_url(&mut self, push_url: &str) -> Result<(), CallError> {
        let agent = CallAgent::new(push_url, &guid()?).ok_or(CallError::NoEndpoint)?;
        self.agent = Some(agent);
        Ok(())
    }

    /// The next thing the engine does, oldest first.
    pub fn poll(&mut self) -> Option<Effect> {
        self.out.effects.pop_front()
    }

    /// The call that is up, and where it is.
    #[must_use]
    pub fn current(&self) -> Option<(CallId, Option<CallState>)> {
        self.call.as_ref().map(|call| (call.id, call.phase.state()))
    }

    /// The keep-alive interval the call's services asked for, once the call is up.
    #[must_use]
    pub fn keep_alive_interval(&self) -> Option<Duration> {
        self.call
            .as_ref()
            .and_then(|call| call.keep_alive)
            .map(|keep| keep.interval)
    }

    /// When [`Self::tick`] next has something to do.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        self.call
            .as_ref()
            .and_then(|call| call.keep_alive)
            .map(|keep| keep.next)
    }

    /// Whether an invitation named a running call in `chat` that [`Self::join`] can join. It says what the machine
    /// was told, not that the call still runs: a call whose end the machine wasn't delivered (once the ring ended
    /// here it hears no more of the call) stays joinable until a join to it fails, which forgets it.
    #[must_use]
    pub fn joinable(&self, chat: &str) -> bool {
        self.running.contains_key(chat)
    }

    /// The account places a call to `placement` with `offer`.
    ///
    /// # Errors
    /// [`CallError`]; nothing was sent.
    pub fn place(&mut self, placement: &Placement, offer: MediaContent, now: Instant) -> Result<CallId, CallError> {
        let echo = match placement {
            Placement::TestCall { echo } => Some(echo.clone()),
            Placement::Chat { .. } => None,
        };
        let mut call = self.new_call(Side::Placed { echo }, now)?;
        let write = build::start(&mut call, &self.identity, placement, offer)?;
        Ok(self.begin(call, Step::Start, write))
    }

    /// The account joins the running call in the group chat `chat` (one an invitation named) with `offer`.
    ///
    /// # Errors
    /// [`CallError`]; nothing was sent.
    pub fn join(&mut self, chat: &str, offer: MediaContent, now: Instant) -> Result<CallId, CallError> {
        let conversation = self.running.get(chat).cloned().ok_or(CallError::NotJoinable)?;
        let side = Side::Joined {
            chat: chat.to_owned(),
            conversation: conversation.clone(),
        };
        let mut call = self.new_call(side, now)?;
        let write = build::join(&mut call, &self.identity, &conversation, chat, offer)?;
        Ok(self.begin(call, Step::Join, write))
    }

    fn new_call(&mut self, side: Side, now: Instant) -> Result<Call, CallError> {
        if self.call.is_some() {
            return Err(CallError::Busy);
        }
        let agent = self.agent.clone().ok_or(CallError::NoEndpoint)?;
        let context = CallContext::fresh()?.with_region(self.identity.region.clone(), self.identity.partition.clone());
        self.next_call += 1;
        let me = self.participant()?;
        Ok(Call::new(CallId(self.next_call), side, context, me, agent, now))
    }

    fn participant(&self) -> Result<CallParticipant, getrandom::Error> {
        Ok(CallParticipant {
            mri: self.identity.mri.clone(),
            display_name: self.identity.display_name.clone(),
            endpoint_id: self.identity.endpoint_id.clone(),
            participant_id: Some(guid()?),
            language_id: self.identity.language_id.clone(),
        })
    }

    fn begin(&mut self, mut call: Call, step: Step, write: CallWrite) -> CallId {
        let id = call.id;
        self.out.send(&mut call, step, write);
        self.out.state(&call);
        self.call = Some(call);
        id
    }

    /// The account answers the invitation `call` rings with, its `answer` to the offer: its endpoint state, then (once
    /// that was taken) its `accept`.
    ///
    /// # Errors
    /// [`CallError`]; nothing was sent.
    pub fn answer(&mut self, call: CallId, answer: MediaContent) -> Result<(), CallError> {
        let current = ringing(&mut self.call, call)?;
        let writes = build::answer(current, &self.identity, answer)?;
        self.out.send_in_turn(current, writes);
        current.phase = Phase::Answering;
        self.out.state(current);
        Ok(())
    }

    /// The account declines the invitation `call` rings with: the ring stops here and on the account's other
    /// endpoints. As captured (`docs/protocol/calling.md`, "The decline"), in a 1:1 call the call controller then
    /// forwards the call to the account's voicemail, so the caller's call connects to it; in a group call the caller
    /// is told this callee failed to join.
    ///
    /// # Errors
    /// [`CallError`]; nothing was sent.
    pub fn decline(&mut self, call: CallId) -> Result<(), CallError> {
        let current = ringing(&mut self.call, call)?;
        let write = build::decline(current)?;
        self.out.send(current, Step::Decline, write);
        current.phase = Phase::Declining;
        self.out.state(current);
        Ok(())
    }
}

/// The offered invitation `call`, if it is `current`.
fn ringing(current: &mut Option<Call>, call: CallId) -> Result<&mut Call, CallError> {
    let current = current
        .as_mut()
        .filter(|current| current.id == call)
        .ok_or(CallError::NoSuchCall)?;
    if current.phase == Phase::Offered && current.leg.is_some() {
        Ok(current)
    } else {
        Err(CallError::NotNow)
    }
}

/// Hanging up, leaving, failing and the outcomes of writes.
impl Calls {
    /// The account hangs up `call`: cancels it while it rings at its callees, leaves it once it is up. An invitation
    /// that rings here is declined instead ([`Self::decline`]), and so is one being answered whose `accept` hasn't
    /// gone out yet (the account isn't in the call before it, and only a decline ends the ring).
    ///
    /// # Errors
    /// [`CallError`]; nothing was sent.
    pub fn hang_up(&mut self, call: CallId, now: Instant) -> Result<(), CallError> {
        let current = self
            .call
            .as_mut()
            .filter(|current| current.id == call)
            .ok_or(CallError::NoSuchCall)?;
        match current.phase {
            Phase::Leaving { .. } => Ok(()),
            Phase::Attaching | Phase::Offered | Phase::Declining => Err(CallError::NotNow),
            Phase::Placing => {
                // the conversation's `leave` link comes with the start's answer: the leave goes then
                current.phase = Phase::Leaving {
                    sent: false,
                    end: Ending::HungUp,
                };
                self.out.state(current);
                Ok(())
            }
            Phase::Answering => {
                // built first, so a refusal leaves everything as it was
                let decline = build::decline(current)?;
                self.out.drop_queued(current);
                if current.writes.values().any(|step| *step == Step::Accept) {
                    self.leave(Ending::HungUp, now);
                } else {
                    // the `accept` never went (held for the endpoint state's answer, or not taken yet): the account
                    // isn't in the call and the fence still holds the invitation as ringing, so a decline ends it
                    self.out.send(current, Step::Decline, decline);
                    current.phase = Phase::Declining;
                    self.out.state(current);
                }
                Ok(())
            }
            Phase::Ringing | Phase::Joining | Phase::Connected => {
                self.leave(Ending::HungUp, now);
                Ok(())
            }
        }
    }

    /// The call leaves for `ending`: its `leave` sent when it may go, else the call ends here at once.
    fn leave(&mut self, ending: Ending, now: Instant) {
        let Some(call) = self.call.as_mut() else { return };
        let write = if call.may_leave() {
            build::leave(call, now).ok()
        } else {
            None
        };
        match write {
            Some(write) => {
                self.out.drop_queued(call);
                self.out.send(call, Step::Leave, write);
                call.phase = Phase::Leaving {
                    sent: true,
                    end: ending,
                };
                self.out.state(call);
            }
            None => self.finish(ending.end()),
        }
    }

    /// The call is over here: none of its writes not handed out yet goes, but a `leave`.
    fn end_call(&mut self) -> Option<Call> {
        let mut call = self.call.take()?;
        self.out.drop_queued(&mut call);
        Some(call)
    }

    /// The call ends here with `end`.
    fn finish(&mut self, end: End) {
        if let Some(call) = self.end_call() {
            self.out.event(CallEvent::Ended { call: call.id, end });
        }
    }

    /// The running group call the call is in isn't joinable any more (its conversation ended, or a join to it
    /// failed), unless a newer invitation named another in its chat.
    fn forget_running(&mut self) {
        let Some((chat, conversation)) = self.call.as_ref().and_then(Call::running) else {
            return;
        };
        if self.running.get(chat) == Some(conversation) {
            let chat = chat.to_owned();
            self.running.remove(&chat);
        }
    }

    /// A step failed: the call leaves if it may and ends; an invitation not shown yet goes away unseen. A failed join
    /// forgets the running call it went to.
    fn fail(&mut self, step: Step, now: Instant) {
        if step == Step::Join {
            self.forget_running();
        }
        match self.call.as_ref().map(|call| call.phase) {
            Some(Phase::Attaching) => drop(self.end_call()),
            // an answer turned into a decline by a hang-up: its endpoint state failing changes nothing, the decline
            // goes on and its own outcome ends the call
            Some(Phase::Declining) if step != Step::Decline => {}
            Some(_) => self.leave(Ending::Failed(step), now),
            None => {}
        }
        if let Some(call) = self.call.as_ref()
            && matches!(call.phase, Phase::Leaving { sent: true, .. })
        {
            // the call ends now; its `leave` goes out unwatched
            self.finish(End::Failed(step.name()));
        }
    }

    /// How a write went (see [`Effect::Send`]). An outcome of a call that ended is dropped.
    pub fn on_outcome(&mut self, write: WriteId, outcome: &Outcome, now: Instant) {
        let Some(call) = self.call.as_mut() else { return };
        let Some(step) = call.writes.remove(&write) else {
            return;
        };
        let body = outcome.body();
        if body.is_some() {
            self.out.release(call, write);
        }
        match (step, body) {
            (Step::Start | Step::Join, Some(body)) => self.started(step, body, now),
            (Step::Attach, Some(body)) => self.attached(body),
            (Step::Accept, Some(body)) => self.connected_by_accept(body, now),
            (Step::Decline, Some(_)) => self.finish(End::Declined),
            (Step::Leave, Some(_)) => self.left(),
            (_, Some(_)) => {}
            (step, None) => self.fail(step, now),
        }
    }

    /// The `leave` was taken: the call is over here.
    fn left(&mut self) {
        let end = match self.call.as_ref().map(|call| call.phase) {
            Some(Phase::Leaving { end, .. }) => end.end(),
            _ => End::HungUp,
        };
        self.finish(end);
    }

    /// A start's or a join's answer: the conversation it opened.
    fn started(&mut self, step: Step, body: &[u8], now: Instant) {
        let Some(conversation) = read::conversation(body) else {
            self.fail(step, now);
            return;
        };
        let Some(call) = self.call.as_mut() else { return };
        call.conversation = conversation;
        match call.phase {
            Phase::Leaving { sent: false, end } => {
                call.phase = Phase::Ringing;
                self.leave(end, now);
            }
            Phase::Placing => {
                call.phase = match call.side {
                    Side::Joined { .. } => Phase::Joining,
                    Side::Placed { .. } | Side::Invited(_) => Phase::Ringing,
                };
                self.out.state(call);
                if let Some((delivery, observed)) = call.early_acceptance.take() {
                    self.accepted(&delivery, observed, now);
                }
            }
            _ => {}
        }
    }
}

/// Deliveries, responses and time.
impl Calls {
    /// A call delivery as the fence read it (`teams_trouter::CallDelivery`'s `delivery` and `observed`).
    pub fn on_delivery(&mut self, delivery: &Delivery, observed: Observed, now: Instant) {
        if observed == Observed::Offered && !delivery.path.contains("/callAgent/") {
            self.invited(delivery, now);
            return;
        }
        let Some(callback) = self.call.as_ref().and_then(|call| call.callback_at(&delivery.path)) else {
            return;
        };
        match callback {
            (Kind::Call, "acceptance") => self.accepted(delivery, observed, now),
            (Kind::Call, "end") => self.ended(delivery),
            (Kind::Conversation, "conversationEnd") => {
                self.forget_running();
                self.ended(delivery);
            }
            // progress, roster and conversation updates, an add's outcome: read by nothing yet
            _ => {}
        }
    }

    /// An invitation the fence offered (see the crate docs).
    fn invited(&mut self, delivery: &Delivery, now: Instant) {
        let Some(invitation) = read::invitation(&delivery.body) else {
            return;
        };
        if invitation.callee != self.identity.mri {
            return;
        }
        if let Some(chat) = &invitation.group_chat {
            self.running.insert(chat.clone(), invitation.conversation.clone());
        }
        if let Some(call) = self.call.as_ref() {
            let again = call
                .invitation()
                .is_some_and(|ringing| ringing.attach == invitation.attach);
            if again && matches!(call.phase, Phase::Attaching | Phase::Offered) {
                self.receipt(delivery);
            }
            // a second invitation while a call is up isn't offered: nothing is sent for it
            return;
        }
        let _ = self.ring(delivery, invitation, now);
    }

    /// A new invitation rings: its receipt, then the `attach` (it is shown once the `attach` is answered).
    fn ring(&mut self, delivery: &Delivery, invitation: Invitation, now: Instant) -> Result<(), CallError> {
        let agent = self.agent.clone().ok_or(CallError::NoEndpoint)?;
        let context = CallContext::fresh()?.with_region(self.identity.region.clone(), self.identity.partition.clone());
        let me = self.participant()?;
        let mut call = Call::new(
            CallId(self.next_call + 1),
            Side::Invited(Box::new(invitation)),
            context,
            me,
            agent,
            now,
        );
        let write = build::attach(&mut call, &self.identity)?;
        self.next_call += 1;
        self.receipt(delivery);
        self.out.send(&mut call, Step::Attach, write);
        self.call = Some(call);
        Ok(())
    }

    /// The receipt of an offered invitation's delivery.
    fn receipt(&mut self, delivery: &Delivery) {
        if let Some(receipt) = InvitationReceipt::new(delivery, 0) {
            self.out
                .respond(DeliveryKey::of(delivery), DeliveryReply::Receipt(receipt));
        }
    }

    /// The `attach`'s answer: the leg's links; it rings here now.
    fn attached(&mut self, body: &[u8]) {
        let Some(invitation) = self.call.as_ref().and_then(|call| call.invitation().cloned()) else {
            return;
        };
        let Some(leg) = read::attached(body, &invitation.conversation) else {
            drop(self.end_call());
            return;
        };
        let Some(call) = self.call.as_mut() else { return };
        call.conversation = leg.conversation.clone();
        let progress = leg.progress.clone();
        call.leg = Some(leg);
        call.phase = Phase::Offered;
        if let Ok(write) = build::ringing(call, &progress) {
            self.out.send(call, Step::Ringing, write);
        }
        self.out.event(CallEvent::Incoming(Incoming {
            call: call.id,
            caller: invitation.caller,
            caller_name: invitation.caller_name,
            group_chat: invitation.group_chat,
            offer: invitation.offer,
        }));
    }

    /// The `accept`'s answer: the call is up.
    fn connected_by_accept(&mut self, body: &[u8], now: Instant) {
        let keep_alive = read::accept_answer(body);
        let Some(call) = self.call.as_mut() else { return };
        if call.phase != Phase::Answering {
            return;
        }
        self.connect(keep_alive, now);
    }

    /// The call is up: told, its keep-alive armed, the account's endpoint metadata sent, then (once that was taken)
    /// the test call's echo added.
    fn connect(&mut self, keep_alive: Option<u64>, now: Instant) {
        let Some(call) = self.call.as_mut() else { return };
        call.phase = Phase::Connected;
        call.keep_alive = keep_alive.map(|seconds| {
            let interval = Duration::from_secs(seconds);
            KeepAlive {
                interval,
                next: now + interval,
            }
        });
        self.out.state(call);
        let mut writes = Vec::new();
        let metadata = call.conversation.endpoint_metadata.clone();
        if let Some(Ok(write)) = metadata.map(|link| build::endpoint_metadata(call, &link)) {
            writes.push((Step::EndpointMetadata, write));
        }
        if let Some(write) = echo_add(call, &self.identity) {
            writes.push((Step::Add, write));
        }
        self.out.send_in_turn(call, writes);
    }

    /// A placed or joined call's acceptance.
    fn accepted(&mut self, delivery: &Delivery, observed: Observed, now: Instant) {
        let Some(phase) = self.call.as_ref().map(|call| call.phase) else {
            return;
        };
        if phase == Phase::Connected {
            // the same acceptance again: its response again
            self.acknowledge(delivery);
            return;
        }
        if phase == Phase::Placing {
            // before the start's or the join's answer: applied once it came (it hands out the links the call needs)
            if let Some(call) = self.call.as_mut() {
                call.early_acceptance = Some((delivery.clone(), observed));
            }
            return;
        }
        if !matches!(phase, Phase::Ringing | Phase::Joining) {
            return;
        }
        let acceptance = read::acceptance(&delivery.body);
        let Some(acceptance) = acceptance.filter(|_| observed == Observed::Acceptance) else {
            self.fail(Step::Acknowledge, now);
            return;
        };
        let links = self
            .call
            .as_mut()
            .map(|call| call.links(Kind::Call, &crate::call::ANSWERED_CALL_LINKS));
        let Some(Ok(links)) = links else {
            self.fail(Step::Acknowledge, now);
            return;
        };
        if let Some(call) = self.call.as_mut() {
            call.acknowledgement = Some(Acknowledgement {
                delivery: None,
                link: acceptance.acknowledgement.clone(),
                links,
                by_request: false,
            });
        }
        self.acknowledge(delivery);
        if let (Some(call), Some(media)) = (self.call.as_ref(), acceptance.answer) {
            self.out.event(CallEvent::RemoteMedia { call: call.id, media });
        }
        self.connect(acceptance.keep_alive, now);
    }

    /// The acceptance's acknowledgement as its delivery response; by request when the response can't be built.
    fn acknowledge(&mut self, delivery: &Delivery) {
        let Some(call) = self.call.as_mut() else { return };
        let Some(acknowledgement) = call.acknowledgement.as_mut() else {
            return;
        };
        let reply = AcceptanceAcknowledgement::new(delivery, correlation(), 0, acknowledgement.links.clone());
        match reply {
            Some(reply) => {
                acknowledgement.delivery = Some(DeliveryKey::of(delivery));
                self.out
                    .respond(DeliveryKey::of(delivery), DeliveryReply::Acknowledgement(reply));
            }
            None => self.acknowledge_by_request(),
        }
    }

    /// The acceptance's acknowledgement by request, once (the web client's way when the response failed).
    fn acknowledge_by_request(&mut self) {
        let Some(call) = self.call.as_mut() else { return };
        let Some(acknowledgement) = call.acknowledgement.as_mut() else {
            return;
        };
        if acknowledgement.by_request {
            return;
        }
        acknowledgement.by_request = true;
        let (link, links) = (acknowledgement.link.clone(), acknowledgement.links.clone());
        if let Some(Ok(write)) = link.map(|link| build::acknowledge(call, &link, &links)) {
            self.out.send(call, Step::Acknowledge, write);
        }
    }

    /// The call's end, or an offered invitation's ring's.
    fn ended(&mut self, delivery: &Delivery) {
        let end = read::remote_end(&delivery.body);
        match self.call.as_ref().map(|call| (call.id, call.phase)) {
            Some((_, Phase::Attaching)) => drop(self.end_call()),
            Some((id, Phase::Offered)) => {
                drop(self.end_call());
                self.out.event(CallEvent::RingEnded { call: id, end });
            }
            Some(_) => self.finish(End::Remote(end)),
            None => {}
        }
    }

    /// The delivery response `delivery` didn't go: an acceptance's is replaced by its acknowledgement by request.
    pub fn on_response_failed(&mut self, delivery: &DeliveryKey) {
        let failed = self
            .call
            .as_ref()
            .and_then(|call| call.acknowledgement.as_ref())
            .is_some_and(|acknowledgement| acknowledgement.delivery.as_ref() == Some(delivery));
        if failed {
            self.acknowledge_by_request();
        }
    }

    /// Time passed: a keep-alive that fell due is told ([`CallEvent::KeepAliveDue`]).
    pub fn tick(&mut self, now: Instant) {
        let Some(call) = self.call.as_mut() else { return };
        let Some(keep) = call.keep_alive.as_mut() else { return };
        if now >= keep.next {
            keep.next = now + keep.interval;
            let id = call.id;
            self.out.event(CallEvent::KeepAliveDue { call: id });
        }
    }
}

/// The `add` of the test call's echo, once its call was accepted: `None` for any other call.
fn echo_add(call: &mut Call, identity: &Identity) -> Option<CallWrite> {
    let Side::Placed { echo: Some(echo) } = call.side.clone() else {
        return None;
    };
    let link = call.conversation.add.clone()?;
    build::add(call, identity, &link, &echo).ok()
}

/// A fresh correlation vector (`MS-CV`) for a delivery response: 22 base64 characters and a step.
fn correlation() -> String {
    let mut bytes = [0_u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        return "AAAAAAAAAAAAAAAAAAAAAA.1".to_owned();
    }
    let base = base64::engine::general_purpose::STANDARD_NO_PAD.encode(bytes);
    format!("{base}.1")
}

#[cfg(test)]
mod tests;
