//! A native calling client on the server, as the engine runs one: the account's tracker, its transport and calling
//! socket, and the client's call machine (`teams_call::Calls`) fed with the socket's call deliveries, its writes
//! tracked and sent in order, its responses sent through each delivery's own responder.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::Value;
use teams_auth::secrets::MemorySecretStore;
use teams_auth::session::Session;
use teams_call::{
    CallEvent, CallId, CallState, Calls, DeliveryKey, DeliveryReply, Effect, End, Identity, Incoming, Observed,
    Outcome, Participant, Placement, RemoteEnd, Tracker, WriteId, WriteSink,
};
use teams_proto::{CallLink, ClientInfo, Cloud, HttpTransport, MediaContent, Mri, Transport as _};
use teams_trouter::{CallResponder, CallResponse, CallStack, Options, PushEvent, RegistrationReport, Trouter};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::support::TestServer;
use crate::trouter::{Account, ORIGIN};

/// How long a test waits for an event before it fails.
pub const WAIT: Duration = Duration::from_secs(20);

type Http = HttpTransport<Session<MemorySecretStore>>;

/// An offer of the account's (its content type is all the signaling reads).
pub fn offer() -> MediaContent {
    MediaContent {
        client_location: Some("ZZ".into()),
        ..MediaContent::new("v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\ns=-\r\n", "application/sdp-ngc-1.0")
    }
}

/// An answer of the account's.
pub fn answer() -> MediaContent {
    MediaContent::new("v=0\r\no=- 3 4 IN IP4 127.0.0.1\r\ns=-\r\n", "application/sdp-ngc-1.0")
}

enum Command {
    /// The offer the client makes from now on, instead of [`offer`].
    Offer(MediaContent),
    Place(Placement),
    /// Joins the chat's call at the conversation its events named.
    Join(String, CallLink),
    /// Adds someone of the group chat to the call.
    Add(CallId, Mri, String),
    /// Offers the call's media again.
    Renegotiate(CallId),
    Answer(CallId),
    Decline(CallId),
    HangUp(CallId),
}

/// One native client of an account.
pub struct Client {
    pub mri: String,
    /// Every delivery at the endpoint's root that names an `evt`, as it came (the call machine drops 115 and 116).
    roots: Arc<Mutex<Vec<Value>>>,
    commands: mpsc::UnboundedSender<Command>,
    events: mpsc::UnboundedReceiver<CallEvent>,
    /// Who else is in the call, as last told (whatever the test waited for then).
    participants: Vec<Participant>,
    /// How many deliveries came at the endpoint's root (invitations, each redelivery too).
    invitations: Arc<AtomicUsize>,
    progress: Gate,
    /// Told each time the tracker saw a ring's end on the socket, before the machine is handed it.
    ring_ends: Arc<tokio::sync::Notify>,
    task: JoinHandle<()>,
}

impl Drop for Client {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A held `progress` write ([`Client::hold_progress`]).
pub struct HeldProgress {
    pub reached: oneshot::Receiver<()>,
    pub release: oneshot::Sender<()>,
    pub answered: oneshot::Receiver<()>,
}

type Sending = Pin<Box<dyn Future<Output = Outcome> + Send>>;

/// The next `progress` write's hold: who is told when the write is reached, when it may go, and who is told when the
/// service answered it.
type Gate = Arc<Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>, oneshot::Sender<()>)>>>;

struct Sink {
    tracker: Arc<Tracker>,
    transport: Arc<Http>,
    queued: Vec<(WriteId, Sending)>,
    progress: Gate,
}

impl WriteSink for Sink {
    fn submit<R: teams_proto::Request + Clone + Send + 'static>(&mut self, id: WriteId, write: R) {
        let (tracker, transport) = (Arc::clone(&self.tracker), Arc::clone(&self.transport));
        let url = write.call_url().map(ToString::to_string);
        let held = write
            .call_url()
            .is_some_and(|url| url.path().ends_with("/progress"))
            .then(|| self.progress.lock().unwrap_or_else(PoisonError::into_inner).take())
            .flatten();
        self.queued.push((
            id,
            Box::pin(async move {
                let answered = match held {
                    Some((reached, release, answered)) => {
                        let _ = reached.send(());
                        let _ = release.await;
                        Some(answered)
                    }
                    None => None,
                };
                let attempt = tracker.before(&write);
                let delivered = transport.deliver(write).await;
                if let Some(answered) = answered {
                    let _ = answered.send(());
                }
                match delivered {
                    Ok(answer) => {
                        tracker.answered(attempt, &answer);
                        Outcome::Answered {
                            status: answer.status(),
                            body: answer.body().to_vec(),
                        }
                    }
                    Err(teams_proto::HttpError::Refused(_))
                        if url.as_deref().is_some_and(|url| tracker.ring_ended(url)) =>
                    {
                        Outcome::RingOver
                    }
                    Err(_) => Outcome::Failed,
                }
            }),
        ));
    }
}

fn options() -> Options {
    Options {
        backoff_base: Duration::from_millis(100),
        backoff_cap: Duration::from_millis(400),
        backoff_jitter_percent: 0,
        ..Options::default()
    }
}

/// The account in its calls, as the client's bring-up names it against this server.
pub fn identity(mri: &str, name: &str, tenant: &str, endpoint_id: &str) -> Identity {
    Identity {
        mri: Mri::new(mri),
        display_name: Some(name.to_owned()),
        endpoint_id: endpoint_id.to_owned(),
        language_id: Some("en-us".into()),
        tenant_id: tenant.to_owned(),
        client: ClientInfo {
            endpoint_capabilities: Some(73_463),
            client_endpoint_capabilities: Some(114_259_690),
            platform_name: Some("SyntheticNative/1".into()),
        },
        region: Some("own".into()),
        partition: Some("own01".into()),
    }
}

impl Client {
    /// `account`'s client, its calling socket on `socket` and its requests through `http`, up and registered.
    pub async fn start(account: &Account, name: &str, socket: &TestServer, http: &TestServer) -> Self {
        Self::start_holding(account, name, (socket, http), 0).await
    }

    /// [`Self::start`], its client holding back its first `held` responses to deliveries, as if they were lost.
    pub async fn start_holding(
        account: &Account,
        name: &str,
        (socket, http): (&TestServer, &TestServer),
        held: usize,
    ) -> Self {
        Self::start_at(account, name, (&socket.origin.replace("http://", "ws://"), http), held).await
    }

    /// [`Self::start`], its calling socket opened at `socket` (a `ws://` origin, a proxy's).
    pub async fn start_through(account: &Account, name: &str, socket: &str, http: &TestServer) -> Self {
        Self::start_at(account, name, (socket, http), 0).await
    }

    async fn start_at(account: &Account, name: &str, (socket, http): (&str, &TestServer), held: usize) -> Self {
        let cloud = Cloud::own(ORIGIN).unwrap();
        let tracker = Arc::new(Tracker::for_cloud(Mri::new(account.mri.clone()), cloud.clone()));
        let transport = Arc::new(account.transport(http).with_call_state(tracker.clone()));
        let ring_ends = Arc::new(tokio::sync::Notify::new());
        let (watched, seen) = (Arc::clone(&tracker), Arc::clone(&ring_ends));
        let trouter = Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&account.session), socket)
            .unwrap()
            .with_options(options())
            .for_calls(CallStack::new(Arc::clone(&tracker)).observing(move |delivery| {
                let observed = watched.observe_delivery(delivery);
                if observed == Observed::RingEnded {
                    seen.notify_one();
                }
                observed
            }));
        let tenant = ngteams_server::db::meta::tenant(&http.app.db).await.unwrap();
        let identity = identity(&account.mri, name, &tenant, trouter.endpoint_id());
        let (commands, commanded) = mpsc::unbounded_channel();
        let (told, events) = mpsc::unbounded_channel();
        let (ready, up) = tokio::sync::oneshot::channel();
        let invitations = Arc::new(AtomicUsize::new(0));
        let progress = Gate::default();
        let roots = Arc::new(Mutex::new(Vec::new()));
        let driver = Driver {
            calls: Calls::for_cloud(identity, cloud),
            sink: Sink {
                tracker,
                transport,
                queued: Vec::new(),
                progress: Arc::clone(&progress),
            },
            responders: HashMap::new(),
            told,
            held,
            invitations: Arc::clone(&invitations),
            offer: None,
            roots: Arc::clone(&roots),
        };
        let task = tokio::spawn(driver.run(trouter.spawn(), commanded, ready));
        tokio::time::timeout(WAIT, up)
            .await
            .expect("the calling socket didn't come up")
            .expect("the client stopped");
        Self {
            mri: account.mri.clone(),
            roots,
            commands,
            events,
            participants: Vec::new(),
            invitations,
            progress,
            ring_ends,
            task,
        }
    }

    /// Waits until the client's tracker saw a ring's end on its socket; fails after [`WAIT`].
    pub async fn ring_end_seen(&self) {
        tokio::time::timeout(WAIT, self.ring_ends.notified())
            .await
            .expect("no ring's end reached the client's socket");
    }

    /// Holds the client's next `progress` write until `release` is used: `reached` is told when the write is reached,
    /// `answered` when the service answered it. The client does nothing else meanwhile, as it awaits each write in turn.
    pub fn hold_progress(&self) -> HeldProgress {
        let (reached, at_hold) = oneshot::channel();
        let (release, released) = oneshot::channel();
        let (answered, at_answer) = oneshot::channel();
        *self.progress.lock().unwrap_or_else(PoisonError::into_inner) = Some((reached, released, answered));
        HeldProgress {
            reached: at_hold,
            release,
            answered: at_answer,
        }
    }

    /// How many deliveries came at the endpoint's root: each invitation, and each time it came again.
    pub fn invitations(&self) -> usize {
        self.invitations.load(Ordering::SeqCst)
    }

    fn command(&self, command: Command) {
        self.commands.send(command).unwrap();
    }

    /// The deliveries at the endpoint's root so far that name an `evt`.
    pub fn root_deliveries(&self) -> Vec<Value> {
        self.roots.lock().unwrap().clone()
    }

    /// Makes `offer` the client's offer from now on (a start's, a join's, a renegotiation's).
    pub fn offer_with(&self, offer: MediaContent) {
        self.command(Command::Offer(offer));
    }

    /// Calls `callee` (a 1:1 call).
    pub fn call(&self, callee: &str) {
        self.command(Command::Place(Placement::Chat {
            callees: vec![Mri::new(callee)],
            group_chat: None,
        }));
    }

    /// Calls `callees` in the group chat `chat`.
    pub fn call_group(&self, chat: &str, callees: &[String]) {
        self.command(Command::Place(Placement::Chat {
            callees: callees.iter().map(Mri::new).collect(),
            group_chat: Some(chat.to_owned()),
        }));
    }

    /// Joins the running call of the group chat `chat`, at `conversation`.
    pub fn join(&self, chat: &str, conversation: CallLink) {
        self.command(Command::Join(chat.to_owned(), conversation));
    }

    /// Adds `invitee` to the call `call` in the group chat `chat`.
    pub fn add(&self, call: CallId, invitee: &str, chat: &str) {
        self.command(Command::Add(call, Mri::new(invitee), chat.to_owned()));
    }

    /// Offers the media of `call` again (its answer comes as [`CallEvent::RemoteMedia`]).
    pub fn renegotiate(&self, call: CallId) {
        self.command(Command::Renegotiate(call));
    }

    pub fn answer(&self, call: CallId) {
        self.command(Command::Answer(call));
    }

    pub fn decline(&self, call: CallId) {
        self.command(Command::Decline(call));
    }

    pub fn hang_up(&self, call: CallId) {
        self.command(Command::HangUp(call));
    }

    /// The next event `pick` takes, skipping others; fails after [`WAIT`].
    pub async fn next<T>(&mut self, mut pick: impl FnMut(CallEvent) -> Option<T>) -> T {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let event = tokio::time::timeout_at(deadline, self.events.recv())
                .await
                .unwrap_or_else(|_| panic!("{}: the event didn't come in time", self.mri))
                .expect("the client stopped");
            if let CallEvent::Participants { participants, .. } = &event {
                self.participants.clone_from(participants);
            }
            if let Some(picked) = pick(event) {
                return picked;
            }
        }
    }

    /// Every event within `quiet`, for a test that something didn't happen.
    pub async fn events_within(&mut self, quiet: Duration) -> Vec<CallEvent> {
        let mut seen = Vec::new();
        let deadline = tokio::time::Instant::now() + quiet;
        while let Ok(Some(event)) = tokio::time::timeout_at(deadline, self.events.recv()).await {
            seen.push(event);
        }
        seen
    }

    pub async fn state(&mut self, wanted: CallState) -> CallId {
        self.next(|event| match event {
            CallEvent::State { call, state } if state == wanted => Some(call),
            _ => None,
        })
        .await
    }

    pub async fn incoming(&mut self) -> Incoming {
        self.next(|event| match event {
            CallEvent::Incoming(incoming) => Some(incoming),
            _ => None,
        })
        .await
    }

    pub async fn ended(&mut self) -> End {
        self.next(|event| match event {
            CallEvent::Ended { end, .. } => Some(end),
            CallEvent::RingEnded { end, .. } => panic!("a ring ended ({end:?}), not a call"),
            _ => None,
        })
        .await
    }

    pub async fn ring_ended(&mut self) -> RemoteEnd {
        self.next(|event| match event {
            CallEvent::RingEnded { end, .. } => Some(end),
            CallEvent::Ended { end, .. } => panic!("the call ended ({end:?}) instead of its ring"),
            _ => None,
        })
        .await
    }

    /// Who else is in the call, as told until `wanted` takes it.
    pub async fn participants_until(&mut self, wanted: impl Fn(&[Participant]) -> bool) -> Vec<Participant> {
        if wanted(&self.participants) {
            return self.participants.clone();
        }
        self.next(|event| match event {
            CallEvent::Participants { participants, .. } if wanted(&participants) => Some(participants),
            _ => None,
        })
        .await
    }

    /// The far end's media as the machine read it (an acceptance's answer).
    pub async fn remote_media(&mut self) -> MediaContent {
        self.next(|event| match event {
            CallEvent::RemoteMedia { media, .. } => Some(media),
            _ => None,
        })
        .await
    }
}

struct Driver {
    calls: Calls,
    sink: Sink,
    responders: HashMap<DeliveryKey, CallResponder>,
    told: mpsc::UnboundedSender<CallEvent>,
    /// How many more responses are held back.
    held: usize,
    invitations: Arc<AtomicUsize>,
    offer: Option<MediaContent>,
    roots: Arc<Mutex<Vec<Value>>>,
}

impl Driver {
    async fn run(
        mut self,
        mut push: teams_trouter::TrouterHandle,
        mut commanded: mpsc::UnboundedReceiver<Command>,
        ready: tokio::sync::oneshot::Sender<()>,
    ) {
        let mut ready = Some(ready);
        loop {
            tokio::select! {
                event = push.next() => {
                    let Some(event) = event else { return };
                    self.pushed(event, &mut ready);
                }
                command = commanded.recv() => {
                    let Some(command) = command else { return };
                    self.act(command);
                }
            }
            self.carry_out().await;
        }
    }

    fn pushed(&mut self, event: PushEvent, ready: &mut Option<tokio::sync::oneshot::Sender<()>>) {
        match event {
            PushEvent::Connected { push_url, .. } => self.calls.set_push_url(&push_url).unwrap(),
            PushEvent::Registration(RegistrationReport::Registered) => {
                if let Some(ready) = ready.take() {
                    let _ = ready.send(());
                }
            }
            PushEvent::Call(delivery) => {
                if let Ok(body) = serde_json::from_slice::<Value>(&delivery.delivery.body)
                    && body.get("evt").is_some()
                {
                    self.roots.lock().unwrap().push(body);
                }
                let key = DeliveryKey {
                    connection: delivery.delivery.connection.clone(),
                    id: delivery.delivery.id,
                };
                if !delivery.delivery.path.contains("callAgent") {
                    self.invitations.fetch_add(1, Ordering::SeqCst);
                }
                self.responders.insert(key, delivery.responder());
                self.calls
                    .on_delivery(&delivery.delivery, delivery.observed, Instant::now());
            }
            _ => {}
        }
    }

    fn act(&mut self, command: Command) {
        let now = Instant::now();
        let offer = self.offer.clone().unwrap_or_else(offer);
        match command {
            Command::Offer(offer) => {
                self.offer = Some(offer);
                Ok(())
            }
            Command::Place(placement) => self.calls.place(&placement, offer, now).map(drop),
            Command::Add(call, invitee, chat) => self.calls.add(call, &invitee, &chat),
            Command::Renegotiate(call) => self.calls.renegotiate(call, &offer),
            Command::Join(chat, conversation) => {
                self.calls.posted_running(&chat, conversation);
                self.calls.join(&chat, offer, now).map(drop)
            }
            Command::Answer(call) => self.calls.answer(call, answer()),
            Command::Decline(call) => self.calls.decline(call),
            Command::HangUp(call) => self.calls.hang_up(call, now),
        }
        .unwrap();
    }

    async fn carry_out(&mut self) {
        while let Some(effect) = self.calls.poll() {
            match effect {
                Effect::Send { id, write } => {
                    write.submit_to(id, &mut self.sink);
                    for (id, sending) in std::mem::take(&mut self.sink.queued) {
                        let outcome = sending.await;
                        self.calls.on_outcome(id, &outcome, Instant::now());
                    }
                }
                Effect::Respond { .. } if self.held > 0 => self.held -= 1,
                Effect::Respond { delivery, reply } => {
                    let response = match reply {
                        DeliveryReply::Receipt(receipt) => CallResponse::from(receipt),
                        DeliveryReply::Acknowledgement(acknowledgement) => CallResponse::from(acknowledgement),
                    };
                    let sent = match self.responders.get(&delivery) {
                        Some(responder) => responder.respond(response).await.is_ok(),
                        None => false,
                    };
                    if !sent {
                        self.calls.on_response_failed(&delivery);
                    }
                }
                Effect::Event(event) => {
                    let _ = self.told.send(event);
                }
                Effect::LetGo { attach } => self.sink.tracker.let_go(attach.as_str()),
            }
        }
    }
}
