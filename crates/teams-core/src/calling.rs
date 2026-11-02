//! The account's calls in the engine (M3 Task 14): the call session that joins the signaling machine (`teams-call`),
//! the calling stack's socket (`teams-trouter`'s `Trouter::for_calls`), the outbox's call kind and the call's media
//! (`teams-media`).
//!
//! - **Actions** ([`CallAction`], `EngineRequests::call`): a call from a chat or the test call, a join of a chat's
//!   running call, an answer or a decline of an invitation the engine offered ([`Event::IncomingCall`]), a hang-up,
//!   muting the microphone, the camera turned on or off, a renegotiation of the call's media. One call at a time: an
//!   action while one is up (or set up) is refused. Nothing answers or declines an invitation but its action: the engine never does by itself.
//! - **The media comes first, the devices last**: a start, a join or an answer goes only once its media is set up
//!   ([`CallMedia`]: the relay's credentials read, an allocation on the relay, the session started without its devices,
//!   our offer or our answer in Teams' dialect); a setup that fails refuses the action and sends nothing (an invitation
//!   keeps ringing, still answerable or declinable). The microphone and the speaker open only once the start, the join
//!   or the answer passed the fence and was taken: when the machine tells the call ringing at its callees, joining, or
//!   connected. Devices that can't be opened then fail the call (`End::Failed("media")`).
//! - **The writes** go out through the outbox's call kind (`Outbox::try_call`), one in flight at a time: beside it the
//!   machine's events and delivery responses go at once, its next writes wait in its own queue (so a call that ends
//!   drops them); a write refused without being acted on goes again only within `CallRetries`' allowance and only if
//!   the machine still wants it once its wait is over (`Calls::may_send_again`, asked again then: a hang-up during the
//!   wait fails it unsent); its answer (any status, with its body) goes back to the machine. Each attempt is a fresh
//!   `Fence::check` with its `fence.log` line; the transport and the calling socket feed the fence's call registry (a
//!   start's answer registers its call, one that isn't a success ends it unregistered, an acceptance binds its links),
//!   so a follow-up on a call the fence didn't register is refused before it is sent.
//! - **A renegotiation of the account's** begins in the machine (`Calls::begin_renegotiation`) before the media session
//!   makes its offer, since a new offer replaces the one the session waits on: a second one, or one while the call
//!   isn't up or the far end's waits for our answer, is refused with the session untouched. The camera (a 1:1 call's
//!   video, [`CallVideo`]; off when a call starts) is one: turned on or off, our video line is offered going that
//!   way, the call's modalities naming `Video` while a video line is in use. One the call controller turned away
//!   (`491`) for the far end's (both cameras turned on together) gives way: the far end's is answered with our
//!   camera's line only receiving, then that line is offered again as the camera goes. A camera that stops by itself
//!   (its device gone) is off, told as [`Event::CameraOff`]; a microphone or speaker whose stream
//!   ended for good (not moved) is told as [`Event::AudioClosed`], the call going on.
//! - **The media's SDP steps run one at a time, in the order the signaling asked for them** (the far end's answer
//!   applied, its renegotiation answered, our own offered): each waits for the one before, so an acceptance followed
//!   at once by a renegotiation reaches the session in that order.
//! - **A group call's video** (M3 Task 19): its camera goes on or off without an SDP change (its main video line goes
//!   both ways throughout, as the web client's): the camera turned, then its lines described again to the call's
//!   services (`Calls::describe_media`, with its send capabilities when it went on). Who is in the call comes from its
//!   roster (`CallEvent::Participants`); whose video each receiving line shows is the gallery's to say
//!   ([`gallery`]), sent to the mixer as source requests once its control plane is up; told as
//!   [`Event::CallParticipants`] with who speaks.
//! - **The calling socket's deliveries** go to the machine as the fence read them; a delivery response goes only
//!   through the delivery's own responder, decided by the fence; an invitation the fence doesn't offer never gets here.
//! - **What the account is told**: [`Event::IncomingCall`], [`Event::RingEnded`] and [`Event::Call`] with the call's
//!   [`CallStatus`]: *connecting* once the signaling says the call is up until the media connected, *reconnecting*
//!   while the media is lost, *ended* once, with how.
//! - **What a failure does** (Task 14's decision with Codex): a step that fails (not sent, denied, unknown, or not
//!   answered `2xx`) ends the call, with a `leave` when the account is in it (`teams-call`); the far end's SDP that
//!   doesn't apply, a renegotiation that can't be answered, the session failing, media that doesn't connect within
//!   [`CONNECT_WITHIN`] of the far end's SDP or stays lost past [`RECOVER_WITHIN`] fail the call the same way
//!   (`End::Failed("media")`).
//! - **The engine stopping** hangs the call up (its `leave`, or the decline of an answer whose `accept` hasn't gone),
//!   lets an invitation that rings go unanswered, waits up to [`SHUTDOWN_GRACE`] for those last writes (a start or a
//!   join in flight up to its request's timeout more, for the answer its `leave` needs), closes the
//!   media and stops the calling socket.
//!
//! The calling runs as a task of its own on the engine's thread ([`Calling::spawn`]; a `LocalSet`'s, as the outbox it
//! writes with isn't `Send`), so the write in flight, the calling socket's deliveries and their responses go on while
//! the engine awaits a chat's work; the engine reaches it through its [`CallingTask`].
//!
//! A call survives the calling socket's reconnect: its links stay as they were (the capture shows a dropped socket
//! changes none), the socket registers again on the same endpoint, and the media runs on its own threads throughout.

mod gallery;
mod media;

pub use gallery::{CallParticipant, CallView, HEARING_LEVEL, SPEAKING_HOLD, SPEAKING_LEVEL};
pub use media::{CLOSED_GRACE, CONNECT_WITHIN, CallMedia, CallVideo, GROUP_SLOTS, RECOVER_WITHIN, Relay, VideoOutput};

use std::collections::{HashMap, VecDeque};
use std::future::{Future, pending};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use teams_call::{
    CallError, CallEvent, CallId, CallState, CallWrite, Calls, DeliveryKey, DeliveryReply, Effect, End, Identity,
    Outcome, Participant, Placement, WriteId, WriteSink,
};
use teams_fence::Mri;
use teams_proto::{
    ChannelParameters, Client, Credentials, EndpointRegistrar, HttpError, HttpTransport, MediaContent, MeetingInfo,
    Mutating, Transport,
};
use teams_trouter::{CallResponder, CallResponse, Ignored, PushEvent, Trouter, TrouterCredentials, TrouterHandle};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;

use crate::engine::{EngineError, Event, Push, emit_to};
use crate::outbox::{CallRetries, CallTried, Outbox};
use media::{LiveMedia, MediaChange, MediaDone, Prepared, Setup, VideoKind};

/// How long the engine, stopping, waits for a call's last writes (its `leave`, its decline).
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// How often a running call's media is looked at.
const MEDIA_LOOK: Duration = Duration::from_millis(50);

/// The account's calling, built and not started: its calling socket (`Trouter::for_calls`, over the transport the
/// outbox writes with, so its deliveries and the call writes feed one call registry), who the account is in its calls
/// and what their media is made of.
pub struct CallParts {
    push: Push,
    identity: Identity,
    media: CallMedia,
}

impl CallParts {
    /// The calling of `identity` (its `endpoint_id` taken from `trouter`, the calling endpoint it registers) on
    /// `trouter`, the calling stack's socket, its calls' media made of `media`.
    #[must_use]
    pub fn new<T, K>(trouter: Trouter<T, K>, mut identity: Identity, media: CallMedia) -> Self
    where
        T: EndpointRegistrar + Send + 'static,
        K: TrouterCredentials + 'static,
    {
        trouter.endpoint_id().clone_into(&mut identity.endpoint_id);
        Self {
            push: Push::new(trouter),
            identity,
            media,
        }
    }
}

impl std::fmt::Debug for CallParts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallParts")
            .field("identity", &self.identity.mri)
            .field("media", &self.media)
            .finish_non_exhaustive()
    }
}

/// What the account asks of its calls (`EngineRequests::call`); each answered with the call it acted on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallAction {
    /// A call from the chat `chat`: a 1:1 chat's other member, or a group chat's other members as the store holds
    /// them.
    Call { chat: String },
    /// The test call of the echo service `echo`.
    TestCall { echo: Mri },
    /// A join of the running call in the group chat `chat`, one an invitation named.
    Join { chat: String },
    /// The account goes into the call of the meeting whose chat is `chat`: it starts the call when nobody has yet, and
    /// joins it when it runs.
    JoinMeeting { chat: String, meeting: MeetingInfo },
    /// The answer of an invitation the engine offered ([`Event::IncomingCall`]).
    Answer { call: CallId },
    /// The decline of an invitation the engine offered.
    Decline { call: CallId },
    /// A hang-up: a cancel while it rings at its callees, a `leave` once it is up, a decline of an answer whose
    /// `accept` hasn't gone.
    HangUp { call: CallId },
    /// The microphone muted (`true`) or not: silence goes in its place, nothing is sent to the call's services (no
    /// capture shows the web client telling them).
    Mute { call: CallId, muted: bool },
    /// The call's media offered again (its lines as they are) as a renegotiation.
    Renegotiate { call: CallId },
    /// The camera of a call that takes video turned on (`true`) or off, the camera's frames sent while it is on: a 1:1
    /// call's video line offered again going that way (a renegotiation); a group call's lines described again
    /// (`updateMediaDescriptions`) with no SDP change, only once the call is up.
    Camera { call: CallId, on: bool },
}

/// Where a call is, as the account is told ([`Event::Call`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallStatus {
    /// The account's start or join goes out.
    Placing,
    /// The account's call rings at its callees.
    Ringing,
    /// The account's join was taken; its acceptance comes next.
    Joining,
    /// The account's answer goes out.
    Answering,
    /// The account's decline goes out.
    Declining,
    /// The call is up as signaling goes; its media isn't connected (yet).
    Connecting,
    /// The call is up and its media connected.
    Connected,
    /// The media was lost and may come back ([`RECOVER_WITHIN`]).
    Reconnecting,
    /// The account leaves.
    Leaving,
    /// The call is over, as `End` says.
    Ended(End),
}

/// An invitation that rings here: answer it or decline it ([`CallAction::Answer`], [`CallAction::Decline`]); nothing
/// does otherwise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncomingCall {
    pub call: CallId,
    pub caller: Mri,
    /// The caller's name as the invitation gives it (no profile lookup).
    pub caller_name: Option<String>,
    /// The group chat of a group call.
    pub group_chat: Option<String>,
}

/// Why a call action was refused. Nothing was sent.
#[derive(Debug, thiserror::Error)]
pub enum CallActionError {
    /// The engine runs without calling (`Parts::calls` `None`).
    #[error("this engine has no calling")]
    NoCalling,
    /// A call is up or being set up: one at a time.
    #[error("a call is already up")]
    Busy,
    /// The chat names nobody to call.
    #[error("that chat names nobody to call")]
    NotAChat,
    /// No media runs for that call.
    #[error("no media runs for that call")]
    NoMedia,
    /// That call's media carries no video: the engine has no video for its calls, the settings' video switch was off
    /// when it was set up, or the codec refused its setup.
    #[error("that call carries no video")]
    NoVideo,
    /// The call's media couldn't be set up, or its SDP made.
    #[error("the call's media couldn't be set up: {0}")]
    Media(String),
    /// The machine refused it.
    #[error(transparent)]
    Call(#[from] CallError),
}

/// Where an action's answer goes.
pub(crate) type Waiting = oneshot::Sender<Result<CallId, EngineError>>;

/// An action the engine resolved (a chat's call into its placement).
pub(crate) enum Act {
    Place(Placement),
    Join(String),
    Answer(CallId),
    Decline(CallId),
    HangUp(CallId),
    Mute(CallId, bool),
    Renegotiate(CallId),
    Camera(CallId, bool),
}

type Attempt = Pin<Box<dyn Future<Output = CallTried>>>;
type Again = Box<dyn FnMut() -> Attempt>;

/// The one call write in flight: where it is, how to make another attempt, and the further attempts it has used.
struct Writing {
    id: WriteId,
    name: &'static str,
    flight: Flight,
    again: Again,
    retries: CallRetries,
}

/// Where the write in flight is.
enum Flight {
    /// An attempt is under way.
    Attempt(Attempt),
    /// Another attempt is due once this wait is over, if the machine still wants the write then.
    Waiting(Pin<Box<tokio::time::Sleep>>),
}

/// What the write in flight came to.
enum Landed {
    /// Its attempt is over.
    Tried(CallTried),
    /// Its wait for another attempt is over.
    Due,
}

/// Media being set up for an action, with the action's answer.
struct Preparing {
    /// What it is set up for: a start or join (`None`), or the answer of an offered invitation.
    answering: Option<CallId>,
    act: Act,
    reply: Waiting,
    setup: Pin<Box<dyn Future<Output = Result<Prepared, String>>>>,
}

/// What [`Calling::next`] found to do.
enum CallStep {
    Push(Option<PushEvent>),
    Written(CallTried),
    Due,
    Prepared(Box<Result<Prepared, String>>),
    Media(MediaDone),
    /// A media step ended without its outcome (it panicked).
    MediaLost,
    Responded(DeliveryKey, bool),
    Look,
}

/// The engine's hold on its calling, which runs as a task of its own ([`Calling::spawn`]).
pub(crate) struct CallingTask {
    actions: mpsc::UnboundedSender<(Act, Waiting)>,
    task: tokio::task::JoinHandle<()>,
}

impl CallingTask {
    /// Hands the account's action to the calling; its answer goes to `reply`.
    pub(crate) fn act(&self, act: Act, reply: Waiting) {
        if let Err(refused) = self.actions.send((act, reply)) {
            let (_, reply) = refused.0;
            let _ = reply.send(Err(EngineError::Stopped));
        }
    }

    /// Stops the calling as [`Calling::shut_down`] does, once that is done.
    pub(crate) async fn shut_down(self) {
        drop(self.actions);
        if let Err(error) = self.task.await {
            log::warn!("the calling's task ended badly: {error}");
        }
    }
}

/// The engine's calling (see the module docs).
pub(crate) struct Calling<C, T> {
    calls: Calls,
    outbox: Arc<Outbox<T>>,
    client: Arc<Client<HttpTransport<C>>>,
    media: CallMedia,
    push: Option<TrouterHandle>,
    unstarted: Option<Push>,
    responders: HashMap<DeliveryKey, CallResponder>,
    writing: Option<Writing>,
    preparing: Option<Preparing>,
    media_steps: JoinSet<MediaDone>,
    /// The SDP steps waiting for the one running to finish, in the order the signaling asked for them.
    sdp_steps: VecDeque<Box<dyn FnOnce() -> MediaDone + Send>>,
    /// An SDP step runs now.
    sdp_running: bool,
    responses: JoinSet<(DeliveryKey, bool)>,
    live: Option<LiveMedia>,
    /// The offers of invitations the engine offered, by call (an answer answers it), with whether the invitation is
    /// a group call's.
    offers: HashMap<CallId, (Option<MediaContent>, bool)>,
    /// The call the signaling says is up: told *connecting* until its media connected.
    up: Option<CallId>,
    look: Option<tokio::time::Interval>,
    events: mpsc::UnboundedSender<Event>,
}

/// The engine's sink for one call write: it keeps the write (to try again) and makes each attempt through the outbox.
struct Sink<T> {
    outbox: Arc<Outbox<T>>,
    made: Option<(Attempt, Again)>,
}

impl<T: Transport<Error = HttpError> + 'static> WriteSink for Sink<T> {
    fn submit<R: Mutating + Clone + Send + 'static>(&mut self, _id: WriteId, write: R) {
        let outbox = Arc::clone(&self.outbox);
        let mut again: Again = Box::new(move || {
            let (outbox, write) = (Arc::clone(&outbox), write.clone());
            Box::pin(async move { outbox.try_call(write).await })
        });
        self.made = Some((again(), again));
    }
}

impl<C, T> Calling<C, T>
where
    C: Credentials + 'static,
    T: Transport<Error = HttpError> + Send + 'static,
{
    pub(crate) fn new(
        parts: CallParts,
        outbox: Arc<Outbox<T>>,
        client: Arc<Client<HttpTransport<C>>>,
        events: mpsc::UnboundedSender<Event>,
    ) -> Self {
        Self {
            calls: Calls::new(parts.identity),
            outbox,
            client,
            media: parts.media,
            push: None,
            unstarted: Some(parts.push),
            responders: HashMap::new(),
            writing: None,
            preparing: None,
            media_steps: JoinSet::new(),
            sdp_steps: VecDeque::new(),
            sdp_running: false,
            responses: JoinSet::new(),
            live: None,
            offers: HashMap::new(),
            up: None,
            look: None,
            events,
        }
    }

    /// Starts the calling socket and runs the calling as a task of its own on the engine's thread (a `LocalSet`'s:
    /// the outbox it writes with isn't `Send`), taking the account's actions, until its [`CallingTask`] shuts it down.
    pub(crate) fn spawn(mut self) -> CallingTask {
        let (actions, mut asked) = mpsc::unbounded_channel::<(Act, Waiting)>();
        let task = tokio::task::spawn_local(async move {
            self.start();
            loop {
                tokio::select! {
                    action = asked.recv() => match action {
                        Some((act, reply)) => self.act(act, reply),
                        None => break,
                    },
                    step = self.next() => self.step(step),
                }
            }
            self.shut_down().await;
        });
        CallingTask { actions, task }
    }

    /// Starts the calling socket (on the engine's runtime).
    fn start(&mut self) {
        if let Some(push) = self.unstarted.take() {
            self.push = Some(push.spawn());
        }
        let mut look = tokio::time::interval(MEDIA_LOOK);
        look.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        self.look = Some(look);
    }

    fn emit(&self, event: Event) {
        emit_to(&self.events, event);
    }

    /// The next thing to do; never, while there is nothing.
    async fn next(&mut self) -> CallStep {
        let looking = self.live.is_some();
        tokio::select! {
            event = next_push(self.push.as_mut()) => CallStep::Push(event),
            landed = land(self.writing.as_mut()) => match landed {
                Landed::Tried(tried) => CallStep::Written(tried),
                Landed::Due => CallStep::Due,
            },
            prepared = prepared(self.preparing.as_mut()) => CallStep::Prepared(Box::new(prepared)),
            Some(done) = self.media_steps.join_next(), if !self.media_steps.is_empty() => match done {
                Ok(done) => CallStep::Media(done),
                Err(_) => CallStep::MediaLost,
            },
            Some(done) = self.responses.join_next(), if !self.responses.is_empty() => match done {
                Ok((key, sent)) => CallStep::Responded(key, sent),
                Err(_) => CallStep::Look,
            },
            () = look(self.look.as_mut()), if looking => CallStep::Look,
        }
    }

    /// Does what `step` asks, then carries out what the machine asks in turn.
    fn step(&mut self, step: CallStep) {
        let now = Instant::now();
        match step {
            CallStep::Push(event) => self.pushed(event, now),
            CallStep::Written(tried) => self.written(tried, now),
            CallStep::Due => self.due(now),
            CallStep::Prepared(prepared) => self.prepared(*prepared, now),
            CallStep::Media(done) => self.media_done(done, now),
            CallStep::MediaLost => {
                self.sdp_running = false;
                self.next_sdp_step();
            }
            CallStep::Responded(key, sent) => {
                if !sent {
                    self.calls.on_response_failed(&key);
                }
            }
            CallStep::Look => self.look_at_media(now),
        }
        self.calls.tick(now);
        self.drive(now);
    }

    // --- the calling socket ---

    fn pushed(&mut self, event: Option<PushEvent>, now: Instant) {
        let Some(event) = event else {
            self.push = None;
            return;
        };
        match event {
            PushEvent::Connected { endpoint, push_url, .. } => {
                log::info!("calling socket connected (endpoint {endpoint})");
                if let Err(error) = self.calls.set_push_url(&push_url) {
                    log::warn!("the calling endpoint's push URL isn't one a call can use: {error}");
                }
            }
            PushEvent::Call(delivery) => {
                let key = DeliveryKey {
                    connection: delivery.delivery.connection.clone(),
                    id: delivery.delivery.id,
                };
                self.responders.insert(key.clone(), delivery.responder());
                self.calls.on_delivery(&delivery.delivery, delivery.observed, now);
                // a delivery is answered (if at all) while it is read: its responder isn't kept past that
                self.drive(now);
                self.responders.remove(&key);
            }
            PushEvent::Ignored(Ignored::IncomingCall) => {
                log::info!("an incoming call the fence doesn't offer: ignored");
            }
            PushEvent::MessageLoss(loss) => loss.caught_up(),
            PushEvent::Disconnected { down, .. } => log::info!("calling socket down: {down:?}"),
            PushEvent::Stopped(stop) => log::warn!("calling socket stopped: {stop:?}"),
            PushEvent::Registration(report) => self.emit(Event::CallingRegistration(report)),
            PushEvent::Registered(_) | PushEvent::Delivery(_) | PushEvent::Presence(_) | PushEvent::Ignored(_) => {}
        }
    }

    // --- the machine's effects ---

    /// Carries out what the machine asks: one write in flight, everything else at once.
    fn drive(&mut self, now: Instant) {
        loop {
            let effect = if self.writing.is_some() {
                self.calls.poll_beside_write()
            } else {
                self.calls.poll()
            };
            match effect {
                None => return,
                Some(Effect::Send { id, write }) => self.send(id, *write),
                Some(Effect::Respond { delivery, reply }) => self.respond(delivery, reply),
                Some(Effect::Event(event)) => self.told(event, now),
                // an invitation dropped before it rang: its attach failed for good (no further attempt goes), its
                // answer didn't read, its media failed; the fence ends its offer, which then holds no call open
                Some(Effect::LetGo { attach }) => self.outbox.fence().let_go(attach.as_str()),
            }
        }
    }

    fn send(&mut self, id: WriteId, write: CallWrite) {
        let name = write.name();
        let mut sink = Sink {
            outbox: Arc::clone(&self.outbox),
            made: None,
        };
        write.submit_to(id, &mut sink);
        if let Some((attempt, again)) = sink.made {
            log::info!("call write {name} ({})", id.get());
            self.writing = Some(Writing {
                id,
                name,
                flight: Flight::Attempt(attempt),
                again,
                retries: CallRetries::default(),
            });
        }
    }

    /// The write in flight's attempt is over: the wait for another one, or its outcome to the machine.
    fn written(&mut self, tried: CallTried, now: Instant) {
        let Some(mut writing) = self.writing.take() else {
            return;
        };
        if let Some(wait) = writing.retries.again(&tried)
            && self.calls.may_send_again(writing.id)
        {
            log::info!(
                "call write {} ({}): {}, once more in {} ms if still wanted",
                writing.name,
                writing.id.get(),
                tried.summary(),
                wait.as_millis()
            );
            writing.flight = Flight::Waiting(Box::pin(tokio::time::sleep(wait)));
            self.writing = Some(writing);
            return;
        }
        log::info!(
            "call write {} ({}): {}",
            writing.name,
            writing.id.get(),
            tried.summary()
        );
        let outcome = match tried {
            CallTried::Answered(answer) => Outcome::Answered {
                status: answer.status(),
                body: answer.body().to_vec(),
            },
            _ => Outcome::Failed,
        };
        self.calls.on_outcome(writing.id, &outcome, now);
    }

    /// The wait before the write in flight's next attempt is over: the attempt goes if the machine still wants the
    /// write (a hang-up during the wait wants no more of it), else the write failed, unsent.
    fn due(&mut self, now: Instant) {
        let Some(mut writing) = self.writing.take() else {
            return;
        };
        if self.calls.may_send_again(writing.id) {
            writing.flight = Flight::Attempt((writing.again)());
            self.writing = Some(writing);
            return;
        }
        log::info!(
            "call write {} ({}): not sent again, its call wants no more of it",
            writing.name,
            writing.id.get()
        );
        self.calls.on_outcome(writing.id, &Outcome::Failed, now);
    }

    fn respond(&mut self, delivery: DeliveryKey, reply: DeliveryReply) {
        let Some(responder) = self.responders.get(&delivery).cloned() else {
            self.calls.on_response_failed(&delivery);
            return;
        };
        let response = match reply {
            DeliveryReply::Receipt(receipt) => CallResponse::from(receipt),
            DeliveryReply::Acknowledgement(acknowledgement) => CallResponse::from(acknowledgement),
        };
        self.responses.spawn(async move {
            let sent = responder.respond(response).await.is_ok();
            (delivery, sent)
        });
    }

    /// What the machine tells: to the account, and to the media.
    fn told(&mut self, event: CallEvent, now: Instant) {
        match event {
            CallEvent::Incoming(incoming) => {
                let group = incoming.group_chat.is_some();
                self.offers.insert(incoming.call, (incoming.offer, group));
                self.emit(Event::IncomingCall(IncomingCall {
                    call: incoming.call,
                    caller: incoming.caller,
                    caller_name: incoming.caller_name,
                    group_chat: incoming.group_chat,
                }));
            }
            CallEvent::RingEnded { call, end } => {
                self.offers.remove(&call);
                self.emit(Event::RingEnded { call, end });
            }
            CallEvent::State { call, state } => self.state(call, state),
            CallEvent::RemoteMedia { call, media } => self.on_media(call, |live| media::apply(live, media), now),
            CallEvent::MediaOffer { call, offer } => self.on_media(call, |live| media::answer(live, offer), now),
            CallEvent::RenegotiationGaveWay { call } => {
                if let Some(live) = self.live.as_mut().filter(|live| live.call == call) {
                    log::info!("call {}: its renegotiation gave way to the far end's", call.get());
                    live.reoffer_owed = live.camera.is_some();
                }
            }
            CallEvent::Ended { call, end } => self.ended(call, end),
            CallEvent::Voicemail { call } => self.emit(Event::Voicemail { call }),
            CallEvent::Participants { call, participants } => self.participants(call, participants, now),
            CallEvent::KeepAliveDue { call, sent } => {
                log::info!("call {}: its keep-alive fell due (sent: {sent})", call.get());
            }
        }
    }

    fn state(&mut self, call: CallId, state: CallState) {
        let status = match state {
            CallState::Placing => CallStatus::Placing,
            CallState::Ringing => {
                self.open_devices(call);
                CallStatus::Ringing
            }
            CallState::Joining => {
                self.open_devices(call);
                CallStatus::Joining
            }
            CallState::Answering => CallStatus::Answering,
            CallState::Declining => CallStatus::Declining,
            CallState::Leaving => CallStatus::Leaving,
            // told by its own event, as it rings
            CallState::Offered => return,
            CallState::Connected => {
                self.open_devices(call);
                self.up = Some(call);
                if self.media_connected(call) {
                    CallStatus::Connected
                } else {
                    CallStatus::Connecting
                }
            }
        };
        self.emit(Event::Call { call, status });
    }

    /// The start, the join or the answer of `call` passed the fence and was taken: its media's microphone and speaker
    /// open now, once, off the engine's thread.
    fn open_devices(&mut self, call: CallId) {
        let Some(live) = self
            .live
            .as_mut()
            .filter(|live| live.call == call && !live.devices_opened)
        else {
            return;
        };
        live.devices_opened = true;
        log::info!("call {}: its devices open", call.get());
        self.media_steps
            .spawn_blocking(media::open_devices(live, Arc::clone(&self.media.devices)));
    }

    fn media_connected(&self, call: CallId) -> bool {
        self.live
            .as_ref()
            .is_some_and(|live| live.call == call && live.connected)
    }

    /// An SDP step of `call`, off the engine's thread, after the ones asked for before it; the call fails when no
    /// media runs for it.
    fn on_media<F>(&mut self, call: CallId, step: impl FnOnce(&LiveMedia) -> F, now: Instant)
    where
        F: FnOnce() -> MediaDone + Send + 'static,
    {
        match self.live.as_ref().filter(|live| live.call == call) {
            Some(live) => {
                let step = step(live);
                self.sdp_step(step);
            }
            None => self.calls.fail_media(call, now),
        }
    }

    /// Queues an SDP step behind the ones asked for before it.
    fn sdp_step(&mut self, step: impl FnOnce() -> MediaDone + Send + 'static) {
        self.sdp_steps.push_back(Box::new(step));
        self.next_sdp_step();
    }

    /// Starts the next SDP step, unless one runs.
    fn next_sdp_step(&mut self) {
        if self.sdp_running {
            return;
        }
        if let Some(step) = self.sdp_steps.pop_front() {
            self.sdp_running = true;
            self.media_steps.spawn_blocking(step);
        }
    }

    fn ended(&mut self, call: CallId, end: End) {
        if self.up == Some(call) {
            self.up = None;
        }
        self.offers.remove(&call);
        if self.live.as_ref().is_some_and(|live| live.call == call) {
            self.close_media();
        }
        self.emit(Event::Call {
            call,
            status: CallStatus::Ended(end),
        });
    }

    /// Closes the running media off the engine's thread (its close joins the session's threads).
    fn close_media(&mut self) {
        // what waits for the call's media won't run: its answers go unanswered (the action hears the engine stopped)
        self.sdp_steps.clear();
        if let Some(live) = self.live.take() {
            let (call, session) = (live.call, live.session);
            tokio::task::spawn_blocking(move || close(call, session));
        }
    }

    // --- the media ---

    fn media_done(&mut self, done: MediaDone, now: Instant) {
        if !matches!(done, MediaDone::Opened { .. } | MediaDone::GroupCamera { .. }) {
            self.sdp_running = false;
        }
        self.media_step_done(done, now);
        self.next_sdp_step();
        // an SDP step may have changed the receiving lines: what they show follows
        self.show_participants(now);
    }

    /// Who is in `call` now, as its roster told it: the gallery follows, its source requests go.
    fn participants(&mut self, call: CallId, participants: Vec<Participant>, now: Instant) {
        if let Some(live) = self.live.as_mut().filter(|live| live.call == call) {
            live.gallery.set_participants(participants);
            self.show_participants(now);
        }
    }

    /// The running call's receiving lines as its dialect names them now, what they are to show sent to the mixer,
    /// and its view told when it changed.
    fn show_participants(&mut self, now: Instant) {
        let Some(live) = self.live.as_mut() else { return };
        if live.is_group() {
            let slots = media::slots(live);
            live.gallery.set_slots(slots);
            let wanted = live.gallery.assign();
            if !wanted.is_empty()
                && let Err(error) = live.session.request_sources(wanted)
            {
                log::warn!("call {}: its source requests didn't go: {error}", live.call.get());
            }
        }
        if let Some(view) = live.gallery.changed(now) {
            let call = live.call;
            self.emit(Event::CallParticipants { call, view });
        }
    }

    /// A group call's camera turned (or not): its lines described again, its send capabilities given when it went on.
    /// A camera turned on whose lines can't be described (the call stopped being describable while it opened) is shut
    /// again and refused (told off when it was on); one turned off stays off, descriptions that can't go only logged.
    fn group_camera_turned(
        &mut self,
        call: CallId,
        on: bool,
        result: Result<(), String>,
    ) -> Result<(), CallActionError> {
        let Some(live) = self.live.as_mut().filter(|live| live.call == call) else {
            return Err(CallActionError::NoMedia);
        };
        if let Err(why) = result {
            if on && live.camera == Some(true) {
                live.camera = Some(false);
                self.emit(Event::CameraOff { call });
            }
            return Err(CallActionError::Media(why));
        }
        let was = live.camera;
        live.camera = Some(on);
        let described = live.describe();
        let main = media::lock(&live.dialect).main_video_mid().map(str::to_owned);
        let parameters = main
            .filter(|_| on)
            .map(|mid| send_capabilities(mid, live.capabilities + 1));
        match self.calls.describe_media(call, &described, parameters) {
            Ok(()) => {
                live.capabilities += u64::from(on);
                log::info!("call {}: camera {}", call.get(), if on { "on" } else { "off" });
                Ok(())
            }
            Err(error) if on => {
                log::warn!(
                    "call {}: its camera's lines can't be described ({error}): shut again",
                    call.get()
                );
                live.camera = Some(false);
                let session = Arc::clone(&live.session);
                tokio::task::spawn_blocking(move || {
                    if let Err(error) = session.set_camera(None) {
                        log::warn!("the camera didn't shut: {error}");
                    }
                });
                if was == Some(true) {
                    self.emit(Event::CameraOff { call });
                }
                Err(error.into())
            }
            Err(error) => {
                log::warn!("call {}: camera off, its lines not described: {error}", call.get());
                Ok(())
            }
        }
    }

    fn media_step_done(&mut self, done: MediaDone, now: Instant) {
        match done {
            MediaDone::Applied { call, result } => match result {
                Ok(()) => {
                    if let Some(live) = self.live.as_mut().filter(|live| live.call == call) {
                        live.applied(now);
                    }
                }
                Err(why) => self.media_failed(call, &why, now),
            },
            MediaDone::Answered { call, result } => {
                let live = self.live.as_mut().filter(|live| live.call == call);
                let answered = result.and_then(|answer| {
                    let content = match live {
                        Some(live) if live.is_group() => live.answer_content(&answer),
                        // a 1:1 call's answer names the `Video` modality while a video line is in use; a group
                        // call's names the call's own, as the web client's
                        _ => {
                            self.calls.set_video(call, answer.carries_video());
                            media::content(&answer)
                        }
                    };
                    self.calls
                        .answer_media(call, content)
                        .map_err(|error| error.to_string())
                });
                match answered {
                    Ok(()) => self.offer_camera_again(call),
                    Err(why) => self.media_failed(call, &why, now),
                }
            }
            MediaDone::Reoffered { call, result, reply } => {
                let renegotiated = match result {
                    Ok(offer) => {
                        let sent = self.calls.renegotiate(call, &media::content(&offer));
                        if sent.is_err() {
                            // the session waits on an offer that won't go
                            self.media_failed(call, "its renegotiation's offer can't go", now);
                        }
                        sent.map_err(CallActionError::from)
                    }
                    Err(why) => {
                        self.calls.give_up_renegotiation(call);
                        Err(CallActionError::Media(why))
                    }
                };
                let _ = reply.send(renegotiated.map(|()| call).map_err(EngineError::from));
            }
            MediaDone::Opened { call, result } => {
                if let Err(why) = result {
                    self.media_failed(call, &why, now);
                }
            }
            MediaDone::Camera {
                call,
                on,
                result,
                reply,
            } => {
                let turned = self.camera_turned(call, on, result, now);
                let _ = reply.send(turned.map(|()| call).map_err(EngineError::from));
            }
            MediaDone::GroupCamera {
                call,
                on,
                result,
                reply,
            } => {
                let turned = self.group_camera_turned(call, on, result);
                let _ = reply.send(turned.map(|()| call).map_err(EngineError::from));
            }
        }
    }

    /// Once the far end's renegotiation that ours gave way to is answered (our camera's line only receiving), our
    /// camera's line offered again as the camera goes, when it is on; tried again after the next answer when another
    /// renegotiation is under way.
    fn offer_camera_again(&mut self, call: CallId) {
        let Some(live) = self.live.as_mut().filter(|live| live.call == call && live.reoffer_owed) else {
            return;
        };
        if live.camera != Some(true) {
            live.reoffer_owed = false;
            return;
        }
        match self.calls.begin_renegotiation(call) {
            Ok(()) => {
                live.reoffer_owed = false;
                // nobody waits for this one's answer
                let (reply, _) = oneshot::channel();
                let step = media::offer_camera(live, true, reply);
                self.sdp_step(step);
            }
            Err(error) => log::info!(
                "call {}: its camera's line waits to be offered again: {error}",
                call.get()
            ),
        }
    }

    /// The camera of `call` turned (or not), with our renegotiation's offer to send (none when our line already went
    /// that way). An offer that can't be made turns the camera off: told when it was on (our camera's line offered
    /// again, [`Self::offer_camera_again`]).
    fn camera_turned(
        &mut self,
        call: CallId,
        on: bool,
        result: Result<Option<teams_media::sdp::NgcSdp>, String>,
        now: Instant,
    ) -> Result<(), CallActionError> {
        let turned = match result {
            Ok(Some(offer)) => {
                self.calls.set_video(call, offer.carries_video());
                let sent = self.calls.renegotiate(call, &media::content(&offer));
                if sent.is_err() {
                    // the session waits on an offer that won't go
                    self.media_failed(call, "its renegotiation's offer can't go", now);
                }
                sent.map_err(CallActionError::from)
            }
            Ok(None) => {
                self.calls.give_up_renegotiation(call);
                Ok(())
            }
            Err(why) => {
                self.calls.give_up_renegotiation(call);
                Err(CallActionError::Media(why))
            }
        };
        if let Some(live) = self.live.as_mut().filter(|live| live.call == call) {
            if turned.is_ok() {
                live.camera = live.camera.map(|_| on);
                log::info!("call {}: camera {}", call.get(), if on { "on" } else { "off" });
            } else if on && live.camera == Some(true) {
                live.camera = Some(false);
                self.emit(Event::CameraOff { call });
            }
        }
        turned
    }

    fn media_failed(&mut self, call: CallId, why: &str, now: Instant) {
        log::warn!("call {}: its media failed: {why}", call.get());
        self.calls.fail_media(call, now);
    }

    fn look_at_media(&mut self, now: Instant) {
        let Some(live) = self.live.as_mut() else { return };
        let call = live.call;
        let levels = live.session.levels();
        live.gallery.levels(levels, now);
        for change in live.poll(now) {
            match change {
                MediaChange::Connected if self.up == Some(call) => self.emit(Event::Call {
                    call,
                    status: CallStatus::Connected,
                }),
                MediaChange::Lost if self.up == Some(call) => self.emit(Event::Call {
                    call,
                    status: CallStatus::Reconnecting,
                }),
                MediaChange::Connected | MediaChange::Lost => {}
                MediaChange::CameraOff => {
                    log::warn!("call {}: its camera went off", call.get());
                    self.emit(Event::CameraOff { call });
                }
                MediaChange::AudioClosed { microphone } => self.emit(Event::AudioClosed { call, microphone }),
                MediaChange::Speakers(speakers) => {
                    if let Some(live) = self.live.as_mut() {
                        live.gallery.speakers(&speakers);
                    }
                }
                MediaChange::Failed(why) => {
                    self.media_failed(call, &why, now);
                    return;
                }
                MediaChange::Closed => {
                    log::info!(
                        "call {}: the far end closed the media and the call's end didn't come: it left",
                        call.get()
                    );
                    self.calls.media_closed(call, now);
                    return;
                }
            }
        }
        // who speaks, as the levels and the mixer told it
        if let Some(live) = self.live.as_mut()
            && let Some(view) = live.gallery.changed(now)
        {
            let call = live.call;
            self.emit(Event::CallParticipants { call, view });
        }
    }
}

/// The account's actions.
impl<C, T> Calling<C, T>
where
    C: Credentials + 'static,
    T: Transport<Error = HttpError> + Send + 'static,
{
    /// Takes the account's action; its answer goes to `reply`, at once or once its media is set up.
    fn act(&mut self, act: Act, reply: Waiting) {
        let now = Instant::now();
        let answer = match act {
            Act::Place(_) | Act::Join(_) | Act::Answer(_) => return self.set_up(act, reply),
            Act::Decline(call) => self.calls.decline(call).map(|()| call).map_err(CallActionError::from),
            Act::HangUp(call) => self.hang_up(call, now),
            Act::Mute(call, muted) => self.mute(call, muted),
            Act::Renegotiate(call) => match self.live.as_ref().filter(|live| live.call == call) {
                // the machine first: the session's new offer would replace the one it waits on
                Some(live) => match self.calls.begin_renegotiation(call) {
                    Ok(()) => {
                        let step = media::reoffer(live, reply);
                        self.sdp_step(step);
                        return;
                    }
                    Err(error) => Err(error.into()),
                },
                None => Err(CallActionError::NoMedia),
            },
            Act::Camera(call, on) => match self.live.as_ref().filter(|live| live.call == call) {
                // a group call's camera changes no SDP: its lines are described again once it turned; it opens only
                // while they can be (the call up, nothing renegotiating it), else it stays shut
                Some(live) if live.is_group() && live.camera.is_some() => match self.calls.describable(call) {
                    Err(error) if on => Err(error.into()),
                    _ => {
                        let step = media::group_camera(live, Arc::clone(&self.media.devices), on, reply);
                        self.media_steps.spawn_blocking(step);
                        return;
                    }
                },
                Some(live) if live.camera.is_some() => match self.calls.begin_renegotiation(call) {
                    Ok(()) => {
                        let step = media::camera(live, Arc::clone(&self.media.devices), on, reply);
                        self.sdp_step(step);
                        return;
                    }
                    Err(error) => Err(error.into()),
                },
                Some(_) => Err(CallActionError::NoVideo),
                None => Err(CallActionError::NoMedia),
            },
        };
        let _ = reply.send(answer.map_err(EngineError::from));
        self.drive(now);
    }

    fn hang_up(&mut self, call: CallId, now: Instant) -> Result<CallId, CallActionError> {
        if let Some(preparing) = self.preparing.take_if(|preparing| preparing.answering == Some(call)) {
            // its answer never went: the invitation still rings, and only its decline ends it
            let _ = preparing.reply.send(Err(EngineError::Call(CallActionError::Media(
                "hung up before the media was set up".to_owned(),
            ))));
            return self.calls.decline(call).map(|()| call).map_err(Into::into);
        }
        self.calls.hang_up(call, now)?;
        Ok(call)
    }

    fn mute(&self, call: CallId, muted: bool) -> Result<CallId, CallActionError> {
        let live = self
            .live
            .as_ref()
            .filter(|live| live.call == call)
            .ok_or(CallActionError::NoMedia)?;
        live.session.set_muted(muted);
        log::info!("call {}: muted {muted}", call.get());
        Ok(call)
    }

    /// Sets up the media of a start, a join or an answer; the action goes once it is.
    fn set_up(&mut self, act: Act, reply: Waiting) {
        let busy = self.preparing.is_some() || self.live.is_some();
        // a 1:1 call takes its main video line, and so does the test call (its camera turned by a renegotiation, at
        // the link its acceptance hands out, as a 1:1 call's; it hands out no `updateMediaDescriptions` link, so its
        // lines couldn't be described as a group call's are); a group call's (placed, joined, answered) its main line
        // and receiving lines
        let (answering, offer, video) = match &act {
            Act::Answer(call) => {
                let ringing = self.calls.current() == Some((*call, Some(CallState::Offered)));
                match self.offers.get(call) {
                    Some((offer, group)) if ringing && !busy => {
                        let kind = if *group { VideoKind::Group } else { VideoKind::OneToOne };
                        (Some(*call), offer.clone(), kind)
                    }
                    _ => return refuse(reply, CallError::NotNow.into()),
                }
            }
            _ if busy || self.calls.current().is_some() => return refuse(reply, CallActionError::Busy),
            Act::Join(chat) if !self.calls.joinable(chat) => return refuse(reply, CallError::NotJoinable.into()),
            Act::Place(Placement::Chat { group_chat: None, .. } | Placement::TestCall { .. }) => {
                (None, None, VideoKind::OneToOne)
            }
            Act::Place(Placement::Chat { .. } | Placement::Meeting { .. }) | Act::Join(_) => {
                (None, None, VideoKind::Group)
            }
            // only a start, a join or an answer sets its media up
            _ => return refuse(reply, CallError::NotNow.into()),
        };
        if answering.is_some() && offer.is_none() {
            return refuse(
                reply,
                CallActionError::Media("the invitation carries no offer".to_owned()),
            );
        }
        // a test call's `add` names the account's tenant: read from its token when the engine couldn't name it
        let tenant = matches!(act, Act::Place(Placement::TestCall { .. })) && !self.calls.knows_tenant();
        let setup = Box::pin(media::prepare(
            Arc::clone(&self.client),
            self.media.clone(),
            Setup { offer, tenant, video },
        ));
        self.preparing = Some(Preparing {
            answering,
            act,
            reply,
            setup,
        });
    }

    /// The media of an action is set up (or not): the action goes now.
    fn prepared(&mut self, prepared: Result<Prepared, String>, now: Instant) {
        let Some(preparing) = self.preparing.take() else {
            return;
        };
        let mut prepared = match prepared {
            Ok(prepared) => prepared,
            Err(why) => return refuse(preparing.reply, CallActionError::Media(why)),
        };
        if let Some(tenant) = prepared.tenant.take() {
            self.calls.set_tenant(tenant);
        }
        let sdp = media::first_content(&prepared);
        let acted = match &preparing.act {
            Act::Place(placement) => self.calls.place(placement, sdp, now),
            Act::Join(chat) => self.calls.join(chat, sdp, now),
            Act::Answer(call) => self.calls.answer(*call, sdp).map(|()| *call),
            _ => Err(CallError::NotNow),
        };
        match acted {
            Ok(call) => {
                let mut live = LiveMedia::new(call, prepared);
                if preparing.answering.is_some() {
                    // our answer applied the far end's offer: the media connects from here
                    live.applied(now);
                }
                self.live = Some(live);
                let _ = preparing.reply.send(Ok(call));
            }
            Err(error) => {
                tokio::task::spawn_blocking(move || drop(prepared));
                refuse(preparing.reply, error.into());
            }
        }
    }
}

/// The engine stopping.
impl<C, T> Calling<C, T>
where
    C: Credentials + 'static,
    T: Transport<Error = HttpError> + Send + 'static,
{
    /// Hangs the call up, lets an invitation that rings go, sends the last writes within [`SHUTDOWN_GRACE`] (a start or
    /// a join still in flight first gets up to its request's timeout for its answer, so the `leave` that answer makes
    /// possible can go: without it the call would stay running in its chat), closes the media and stops the calling
    /// socket.
    async fn shut_down(mut self) {
        let now = Instant::now();
        if let Some(preparing) = self.preparing.take() {
            let _ = preparing.reply.send(Err(EngineError::Stopped));
        }
        let grace = match self.writing.as_ref().map(|writing| writing.name) {
            Some("start" | "join") => teams_proto::REQUEST_TIMEOUT + SHUTDOWN_GRACE,
            _ => SHUTDOWN_GRACE,
        };
        self.calls.shut_down(now);
        self.drive(now);
        let last_writes = async {
            while self.writing.is_some() {
                match land(self.writing.as_mut()).await {
                    Landed::Tried(tried) => self.written(tried, Instant::now()),
                    Landed::Due => self.due(Instant::now()),
                }
                self.drive(Instant::now());
            }
        };
        if tokio::time::timeout(grace, last_writes).await.is_err() {
            log::warn!("the call's last writes didn't finish within {grace:?}");
        }
        if let Some(live) = self.live.take() {
            let (call, session) = (live.call, live.session);
            let _ = tokio::task::spawn_blocking(move || close(call, session)).await;
        }
        if let Some(push) = self.push.take() {
            push.stop().await;
        }
    }
}

/// Closes a call's media (the close joins the session's threads), its counts logged first: what went out and what came
/// in, never what it carried.
fn close(call: CallId, session: Arc<teams_media::session::MediaSession>) {
    let stats = session.stats();
    let video = stats.video;
    log::info!(
        "call {}: its video closes: {} frames captured, {} sent, {} unsent; {} received, {} shown",
        call.get(),
        video.captured,
        video.sent,
        video.unsent,
        video.received,
        video.shown,
    );
    log::info!(
        "call {}: its media closes: {} audio frames captured, {} sent, {} unsent; {} received, {} malformed; {} \
         played",
        call.get(),
        stats.encoded,
        stats.sent,
        stats.unsent,
        stats.received,
        stats.malformed,
        stats.frames_played,
    );
    drop(session);
}

/// A group call's send capabilities once its camera is on (`applyChannelParameters` on its main video line `mid`, as
/// the web client gives them, numbered `sequence`): the largest picture it sends, one stream (no simulcast).
fn send_capabilities(mid: String, sequence: u64) -> ChannelParameters {
    let caps = serde_json::json!({"maxVideoSendCapabilities": {"caps": {
        "max-width": 1280, "max-height": 1280, "max-fps": 30, "max-streams": 1, "max-layers": 1,
        "sequence-number": sequence,
    }}});
    ChannelParameters {
        mids: vec![mid],
        media_parameter: caps.to_string(),
    }
}

fn refuse(reply: Waiting, error: CallActionError) {
    let _ = reply.send(Err(EngineError::Call(error)));
}

/// The calling socket's next event; never, without one.
async fn next_push(push: Option<&mut TrouterHandle>) -> Option<PushEvent> {
    match push {
        Some(push) => push.next().await,
        None => pending().await,
    }
}

/// What the write in flight comes to next: its attempt over, or its wait for another; never, without one.
async fn land(writing: Option<&mut Writing>) -> Landed {
    match writing.map(|writing| &mut writing.flight) {
        Some(Flight::Attempt(attempt)) => Landed::Tried(attempt.await),
        Some(Flight::Waiting(wait)) => {
            wait.await;
            Landed::Due
        }
        None => pending().await,
    }
}

/// The media set up for an action; never, while none is.
async fn prepared(preparing: Option<&mut Preparing>) -> Result<Prepared, String> {
    match preparing {
        Some(preparing) => (&mut preparing.setup).await,
        None => pending().await,
    }
}

/// The next look at the media; never, before the calling started.
async fn look(look: Option<&mut tokio::time::Interval>) {
    match look {
        Some(look) => {
            look.tick().await;
        }
        None => pending().await,
    }
}

impl From<CallActionError> for EngineError {
    fn from(error: CallActionError) -> Self {
        Self::Call(error)
    }
}
