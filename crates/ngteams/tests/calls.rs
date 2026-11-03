//! The personal build's calls on the mock (`teams-mock`'s calling service, its Trouter and its media: a loopback TURN
//! server and a loopback peer behind each party it plays), under the personal fence (`docs/personal-build.md`,
//! "Calls"): each account an engine of its own, as `crates/ngteams` assembles one, with the personal fence of its
//! account (which logs every send and denies none, so it reads no members), its own `fence.log`, its messaging socket
//! and its calling socket, its calls' media on the file backend (a tone in, a recording out, each opening of the
//! microphone and the camera counted). No PipeWire and no portal is opened: the calls use synthetic file devices.
//!
//! - the test call to the web bundle's echo service: every write permitted and logged, its registration an own-state
//!   entry, its frames logged, no plain request, the microphone opened only once the start passed, the camera never;
//! - a call in a 1:1 chat: the callee's engine answers only by its action;
//! - any start, whoever it names and in whatever chat, passes the fence unread and logged;
//! - an outsider's invitation: offered, rung, answered or declined only by the action;
//! - an invitation during a call: not offered, nothing sent for it;
//! - an `attach` refused with a 401 goes again and its invitation is answered; one that failed for good (a refusal, a
//!   skypetoken that couldn't be had) is let go, and the next invitation rings;
//! - a running group call joined;
//! - the plain requests a lab call sends, each with its personal route.
//!
//! Every id, token and MRI here is the mocks' made-up one.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineError, EngineHandle, EngineOptions, Event, Parts, Push};
use teams_core::{CallAction, CallId, CallMedia, CallParts, CallStatus, End, Identity, IncomingCall, Outbox, Relay};
use teams_fence::{FailClosed, Fence, FenceLog, MembershipSource, Mri, Scope, Target, WriteKind};
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_media::devices::{AudioSink, AudioSource, DeviceError, Devices, VideoSource, wav};
use teams_media::processing::ProcessingConfig;
use teams_media::relay::RelayServer;
use teams_media::session::SessionConfig;
use teams_media::tone::{Tone, level_at};
use teams_mock::{
    AB_CHAT, ABC_CHAT, AX_CHAT, CALLING_APP, CHAT_BASE, ChatServiceMock, LAB_A, LAB_B, OUTSIDER, TENANT, TIMED_OUT,
    TrouterMock,
};
use teams_proto::{Author, Client, ClientInfo, Credentials, HttpTransport, RelayTokens, Transport};
use teams_store::Store;
use teams_trouter::{CallStack, Options, RegistrationReport, Trouter, TrouterCredentials};
use tokio::sync::broadcast;

/// How long a test waits for an event.
const WAIT: Duration = Duration::from_secs(30);
/// How long the media runs once connected, before what was heard is judged.
const TALK: Duration = Duration::from_secs(2);
/// The far tone is heard at least this loud in a recording.
const HEARD: f32 = 0.25;

const A_TONE: Tone = Tone {
    hz: 440.0,
    amplitude: 0.5,
};
const B_TONE: Tone = Tone {
    hz: 660.0,
    amplitude: 0.5,
};
const PEER_TONE: Tone = Tone {
    hz: 880.0,
    amplitude: 0.5,
};

/// One account's tokens: its `ic3` token and its skypetoken, as the mock takes them.
struct Tokens {
    ic3: String,
    skypetoken: String,
    renewals: broadcast::Sender<Renewal>,
    /// The next skypetoken asked for can't be had (its renewal failed), once.
    failing: Arc<AtomicBool>,
}

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.ic3.clone())))
    }

    fn bearer_rejected(&self, _audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.ic3.clone())))
    }

    fn skypetoken(&self) -> impl Future<Output = Result<Option<Secret>, TokenError>> + Send {
        future::ready(if self.failing.swap(false, Ordering::SeqCst) {
            Err(TokenError::NotSignedIn {
                account: "the account".into(),
            })
        } else {
            Ok(Some(Secret::new(self.skypetoken.clone())))
        })
    }
}

impl TrouterCredentials for Tokens {
    fn ic3(&self) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(AccessToken {
            token: Secret::new(self.ic3.clone()),
            expires_at: SystemTime::now() + Duration::from_secs(7_200),
            scopes: Vec::new(),
        }))
    }

    fn ic3_after(&self, _old: &Secret) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        self.ic3()
    }

    fn renewals(&self) -> broadcast::Receiver<Renewal> {
        self.renewals.subscribe()
    }
}

/// A `fence.log` the test reads back.
#[derive(Clone, Default)]
struct SharedLog(Arc<Mutex<Vec<u8>>>);

impl SharedLog {
    fn lines(&self) -> Vec<Value> {
        let bytes = self.0.lock().unwrap_or_else(PoisonError::into_inner).clone();
        String::from_utf8(bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// Every write decision it denied.
    fn denials(&self) -> Vec<Value> {
        self.lines()
            .into_iter()
            .filter(|line| line["verdict"] == "deny")
            .collect()
    }

    /// The writes it permitted, each line as text (its kind, scope and call operation).
    fn permitted(&self) -> Vec<String> {
        self.lines()
            .into_iter()
            .filter(|line| line["verdict"] == "allow" && line["kind"].is_string() && line["event"].is_null())
            .map(|line| line.to_string())
            .collect()
    }

    fn permitted_any(&self, what: &str) -> bool {
        self.permitted().iter().any(|line| line.contains(what))
    }

    /// The `event` values of its lines that have one (`frame`, `own_state`, `delivery_response`, `allowed`, …).
    fn events(&self) -> Vec<String> {
        self.lines()
            .into_iter()
            .filter_map(|line| line["event"].as_str().map(str::to_owned))
            .collect()
    }
}

impl Write for SharedLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The mock with its Trouter and its calls' media on.
struct Lab {
    mock: ChatServiceMock,
    trouter: TrouterMock,
    relay: std::net::SocketAddr,
    dir: tempfile::TempDir,
}

impl Lab {
    async fn start() -> Self {
        let mock = ChatServiceMock::start().await.unwrap();
        let trouter = mock.start_trouter().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let relay = mock.start_media(dir.path(), PEER_TONE).unwrap();
        Self {
            mock,
            trouter,
            relay,
            dir,
        }
    }

    /// `mri`'s personal engine, its calls' media playing `tone`, ready: both its sockets registered.
    async fn account(&self, mri: &'static str, tone: Tone) -> Account {
        let mut account = Account::start(self, mri, tone);
        account.ready().await;
        account
    }

    fn last_call(&self) -> String {
        self.mock.calls().last().expect("a call").id.clone()
    }

    /// The operations `mri` sent on the mock's newest call, in order.
    fn sent_by(&self, mri: &str) -> Vec<String> {
        let calls = self.mock.calls();
        let call = calls.last().expect("a call");
        call.requests
            .iter()
            .filter(|(_, account)| account == mri)
            .map(|(op, _)| op.clone())
            .collect()
    }
}

/// Short waits, so the tests run in seconds.
fn options() -> Options {
    Options {
        ping_interval: Duration::from_millis(300),
        // the production wait: the mock's media (libwebrtc offers and answers of its peers and mixers) holds the
        // mock up for most of a second at times, and no test waits for a pong that never comes
        pong_timeout: Duration::from_secs(5),
        backoff_base: Duration::from_millis(50),
        backoff_cap: Duration::from_millis(200),
        backoff_jitter_percent: 0,
        ..Options::default()
    }
}

/// How often the calls opened the microphone and the camera.
#[derive(Clone, Default)]
struct Opened {
    microphones: Arc<AtomicUsize>,
    cameras: Arc<AtomicUsize>,
}

impl Opened {
    fn microphones(&self) -> usize {
        self.microphones.load(Ordering::SeqCst)
    }

    fn cameras(&self) -> usize {
        self.cameras.load(Ordering::SeqCst)
    }
}

/// File devices that count the microphone's and the camera's openings: the seam's stand-in for the desktop's
/// PipeWire and camera portal.
struct Counted {
    devices: FileDevices,
    opened: Opened,
}

impl Devices for Counted {
    fn microphone(&self) -> Result<Box<dyn AudioSource>, DeviceError> {
        self.opened.microphones.fetch_add(1, Ordering::SeqCst);
        self.devices.microphone()
    }

    fn speaker(&self) -> Result<Box<dyn AudioSink>, DeviceError> {
        self.devices.speaker()
    }

    fn camera(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
        self.opened.cameras.fetch_add(1, Ordering::SeqCst);
        self.devices.camera()
    }
}

/// One account's personal engine on the mock.
struct Account {
    mri: &'static str,
    handle: EngineHandle,
    log: SharedLog,
    recording: PathBuf,
    opened: Opened,
    /// Fails its next skypetoken, once.
    failing: Arc<AtomicBool>,
    _store: tempfile::TempDir,
}

impl Account {
    fn start(lab: &Lab, mri: &'static str, tone: Tone) -> Self {
        let log = SharedLog::default();
        let fence = Arc::new(Fence::personal(Mri::new(mri), FenceLog::from_writer(log.clone())));
        let failing = Arc::new(AtomicBool::new(false));
        let tokens = Arc::new(Tokens {
            ic3: lab.mock.token(mri).unwrap(),
            skypetoken: lab.mock.skypetoken(mri).unwrap(),
            renewals: broadcast::channel(4).0,
            failing: Arc::clone(&failing),
        });
        let origin = lab.mock.origin();
        let http =
            || HttpTransport::with_mock_origin(Arc::clone(&tokens), Arc::clone(&fence), CHAT_BASE, &origin).unwrap();
        let transport = Arc::new(http());
        // the personal fence reads no members
        let members: Arc<dyn MembershipSource + Send> = Arc::new(FailClosed);
        let trouter = || {
            Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), lab.trouter.origin())
                .unwrap()
                .with_options(options())
        };
        let calling = trouter().for_calls(CallStack::new(Arc::clone(&fence), Arc::clone(&members)));
        let store = tempfile::tempdir().unwrap();
        let recording = lab.dir.path().join(format!("{}.wav", mri.rsplit(':').next().unwrap()));
        let opened = Opened::default();
        let media = media(lab.relay, tone, &recording, opened.clone());
        let calls = CallParts::new(calling, identity(mri), media);
        let push = Push::new(trouter());
        let client = Arc::new(Client::new(http()));
        let outbox = Outbox::new(transport, fence, members, Author::new(Mri::new(mri), "Someone"));
        let handle = engine::start(Parts {
            calls: Some(calls),
            store: Store::open(&store.path().join("store.sqlite")).unwrap(),
            client,
            outbox,
            push: Some(push),
            options: EngineOptions {
                tick: Duration::from_millis(50),
                ..EngineOptions::default()
            },
            known_names: Vec::new(),
        })
        .unwrap();
        Self {
            mri,
            handle,
            log,
            recording,
            opened,
            failing,
            _store: store,
        }
    }

    /// Waits until both its sockets are registered.
    async fn ready(&mut self) {
        let (mut messaging, mut calling) = (false, false);
        while !(messaging && calling) {
            let which = self
                .next(|event| match event {
                    Event::Registration(RegistrationReport::Registered) => Some(true),
                    Event::CallingRegistration(RegistrationReport::Registered) => Some(false),
                    _ => None,
                })
                .await;
            if which {
                messaging = true;
            } else {
                calling = true;
            }
        }
    }

    /// The next event `pick` takes, skipping others; fails after [`WAIT`].
    async fn next<T>(&mut self, mut pick: impl FnMut(&Event) -> Option<T>) -> T {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let event = tokio::time::timeout_at(deadline, self.handle.next_event())
                .await
                .unwrap_or_else(|_| panic!("{}: the event didn't come in time", self.mri))
                .expect("the engine runs");
            if let Some(picked) = pick(&event) {
                return picked;
            }
        }
    }

    /// Waits for `call` to be `wanted`; fails if it ends first.
    async fn status(&mut self, call: CallId, wanted: &CallStatus) {
        self.next(|event| match event {
            Event::Call { call: of, status } if *of == call && status == wanted => Some(()),
            Event::Call {
                call: of,
                status: CallStatus::Ended(end),
            } if *of == call => panic!("call {} ended ({end:?}) before {wanted:?}", call.get()),
            _ => None,
        })
        .await;
    }

    /// Waits for `call` to end: how.
    async fn ended(&mut self, call: CallId) -> End {
        self.next(|event| match event {
            Event::Call {
                call: of,
                status: CallStatus::Ended(end),
            } if *of == call => Some(end.clone()),
            _ => None,
        })
        .await
    }

    /// Waits for an invitation to ring here.
    async fn incoming(&mut self) -> IncomingCall {
        self.next(|event| match event {
            Event::IncomingCall(incoming) => Some(incoming.clone()),
            _ => None,
        })
        .await
    }

    /// Every event within `quiet`, for a test that something didn't happen.
    async fn events_within(&mut self, quiet: Duration) -> Vec<Event> {
        let mut seen = Vec::new();
        let deadline = tokio::time::Instant::now() + quiet;
        while let Ok(Some(event)) = tokio::time::timeout_at(deadline, self.handle.next_event()).await {
            seen.push(event);
        }
        seen
    }

    async fn act(&self, action: CallAction) -> Result<CallId, EngineError> {
        self.handle.requests().call(action).await
    }

    /// Stops its engine (which hangs its call up first), off the test's runtime.
    async fn shut_down(self) {
        let Self { handle, .. } = self;
        tokio::task::spawn_blocking(move || handle.shutdown()).await.unwrap();
    }
}

/// The account in its calls, as the personal build describes it (the web client's numbers and language).
fn identity(mri: &str) -> Identity {
    Identity {
        mri: Mri::new(mri),
        display_name: Some("Someone".into()),
        endpoint_id: String::new(),
        language_id: Some(teams_core::account::CALL_LANGUAGE.into()),
        tenant_id: TENANT.into(),
        client: ClientInfo {
            endpoint_capabilities: Some(teams_core::account::WEB_ENDPOINT_CAPABILITIES),
            client_endpoint_capabilities: Some(teams_core::account::WEB_CLIENT_ENDPOINT_CAPABILITIES),
            platform_name: Some("SyntheticNative/1".into()),
        },
        region: Some("amer".into()),
        partition: Some("amer01".into()),
    }
}

/// The calls' media: `tone` in, `recording` out, through the loopback TURN server at `relay`, nothing processed.
fn media(relay: std::net::SocketAddr, tone: Tone, recording: &Path, opened: Opened) -> CallMedia {
    CallMedia {
        devices: Arc::new(Counted {
            devices: FileDevices {
                input: AudioInput::Tone(tone),
                recording: Some(recording.to_path_buf()),
                camera: PatternCamera::default(),
                pacing: Pacing::RealTime,
            },
            opened,
        }),
        relay: Relay::Fixed(vec![RelayServer::udp(relay)]),
        session: SessionConfig {
            processing: ProcessingConfig::OFF,
        },
        video: None,
    }
}

/// The level of `tone` in the last `stretch` of the recording at `path`, leaving out its last 200 ms.
fn heard(path: &Path, tone: Tone, stretch: Duration) -> f32 {
    let samples = wav::read(path).unwrap();
    let at = |span: Duration| usize::try_from(span.as_millis()).unwrap() * 48;
    let end = samples.len().saturating_sub(at(Duration::from_millis(200)));
    let start = end.saturating_sub(at(stretch));
    assert!(
        end - start >= 24_000,
        "{} holds {} samples",
        path.display(),
        samples.len()
    );
    level_at(&samples[start..end], tone.hz)
}

/// The test call's echo service: the web bundle's, as the personal build's "Test call" names it.
fn echo() -> Mri {
    ngteams::calling::echo()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_test_call_goes_to_the_echo_service_every_write_logged_and_no_plain_request_sent() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    // the calling registration went as its own-state entry, the calling socket's frames logged
    let registrations = lab.trouter.registrations();
    assert!(
        registrations
            .iter()
            .any(|registration| registration.app_id == CALLING_APP && registration.account == LAB_A),
        "{registrations:?}"
    );
    let own: Vec<Value> = a
        .log
        .lines()
        .into_iter()
        .filter_map(|line| line.get("own").cloned())
        .collect();
    assert!(own.contains(&json!("calling_endpoint_registration")), "{own:?}");
    assert_eq!(a.opened.microphones(), 0, "no microphone before a call");

    let call = a.act(CallAction::TestCall { echo: echo() }).await.unwrap();
    a.status(call, &CallStatus::Connected).await;
    assert_eq!(a.opened.microphones(), 1, "the microphone opened once the start passed");
    tokio::time::sleep(TALK).await;
    a.act(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(a.ended(call).await, End::HungUp);

    assert_eq!(lab.sent_by(LAB_A), ["start", "updateEndpointMetadata", "add", "leave"]);
    assert!(a.log.denials().is_empty(), "{:#?}", a.log.denials());
    for what in ["\"call\"", "update_endpoint_metadata", "\"add\"", "\"leave\""] {
        assert!(a.log.permitted_any(what), "{what}: {:#?}", a.log.permitted());
    }
    let events = a.log.events();
    assert!(
        events.iter().any(|event| event == "frame"),
        "the frames logged: {events:?}"
    );
    assert!(
        events.iter().any(|event| event == "delivery_response"),
        "the acceptance's acknowledgement decided and logged: {events:?}"
    );
    assert!(
        !events.iter().any(|event| event == "allowed"),
        "no plain request went out: {events:?}"
    );
    assert_eq!(
        a.opened.cameras(),
        0,
        "the camera stays off unless the user turns it on"
    );
    let id = lab.last_call();
    let _ = lab.mock.close_peer(&id, LAB_A);
    assert!(
        heard(&a.recording, PEER_TONE, TALK) > HEARD,
        "the echo's media came back"
    );
    a.shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_in_a_chat_listing_the_account_rings_and_the_callee_answers_only_by_its_action() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    let mut b = lab.account(LAB_B, B_TONE).await;
    let call = a.act(CallAction::Call { chat: AB_CHAT.into() }).await.unwrap();
    a.status(call, &CallStatus::Ringing).await;
    let incoming = b.incoming().await;
    assert_eq!(incoming.caller.as_str(), LAB_A);
    // nothing answers it but b's action, and b's microphone stays shut
    let quiet = b.events_within(Duration::from_millis(500)).await;
    assert!(
        !quiet.iter().any(|event| matches!(event, Event::Call { .. })),
        "{quiet:#?}"
    );
    assert!(!lab.sent_by(LAB_B).contains(&"accept".to_owned()));
    assert_eq!(b.opened.microphones(), 0);
    b.act(CallAction::Answer { call: incoming.call }).await.unwrap();
    b.status(incoming.call, &CallStatus::Connected).await;
    a.status(call, &CallStatus::Connected).await;
    assert_eq!(b.opened.microphones(), 1, "opened once the answer passed");
    tokio::time::sleep(TALK).await;
    a.act(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(a.ended(call).await, End::HungUp);
    assert!(matches!(b.ended(incoming.call).await, End::Remote(_)));
    assert!(a.log.denials().is_empty(), "{:#?}", a.log.denials());
    assert!(b.log.denials().is_empty(), "{:#?}", b.log.denials());
    assert!(a.log.permitted_any("\"call\""));
    assert!(b.log.permitted_any("call_answer"), "{:#?}", b.log.permitted());
    assert_eq!(
        a.opened.cameras() + b.opened.cameras(),
        0,
        "no camera at a start or an answer"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(heard(&a.recording, B_TONE, TALK) > HEARD && heard(&b.recording, A_TONE, TALK) > HEARD);
    a.shut_down().await;
    b.shut_down().await;
}

/// A call start from `caller` to `callees` on the chat `id`, as the start's row maps it.
fn call_start(id: &str, caller: &str, callees: &[&str]) -> Target {
    Target::new(WriteKind::Call, Scope::Conversation { id: id.to_owned() })
        .with_added(std::iter::once(caller).chain(callees.iter().copied()).map(Mri::new))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_start_passes_the_personal_fence_unread_and_logged() {
    let mock = ChatServiceMock::start().await.unwrap();
    let decide = |own: &'static str, target: Target| async move {
        let log = SharedLog::default();
        let fence = Fence::personal(Mri::new(own), FenceLog::from_writer(log.clone()));
        let decided = fence.check(target, &FailClosed).await;
        (decided, log)
    };
    let another_echo = Target::new(WriteKind::Call, Scope::New).with_added([
        Mri::new(LAB_A),
        Mri::new(teams_mock::ECHO),
        Mri::new(OUTSIDER),
    ]);
    for (own, target) in [
        // a callee the chat doesn't list, a chat that doesn't list the account, a test call with another bot and more
        (LAB_A, call_start(ABC_CHAT, LAB_A, &[LAB_B, OUTSIDER])),
        (OUTSIDER, call_start(ABC_CHAT, OUTSIDER, &[LAB_B])),
        (LAB_A, call_start(AX_CHAT, LAB_A, &[OUTSIDER])),
        (LAB_A, another_echo),
    ] {
        let (decided, log) = decide(own, target.clone()).await;
        assert!(decided.is_ok(), "{target}");
        assert!(log.denials().is_empty() && log.permitted_any("\"call\""), "{target}");
    }
    assert!(mock.received_requests().await.is_empty(), "no chat read");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_outsider_s_invitation_rings_by_its_name_and_is_answered_or_declined_only_by_the_action() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    // the first: answered by the action
    let id = lab.mock.ring(OUTSIDER, LAB_A);
    let incoming = a.incoming().await;
    assert_eq!(incoming.caller.as_str(), OUTSIDER);
    assert!(incoming.caller_name.is_some(), "shown by the name the invitation gives");
    let quiet = a.events_within(Duration::from_millis(500)).await;
    assert!(
        !quiet.iter().any(|event| matches!(event, Event::Call { .. })),
        "{quiet:#?}"
    );
    let rang = lab.sent_by(LAB_A);
    assert!(
        rang.contains(&"attach".to_owned()),
        "the ring-time sends went: {rang:?}"
    );
    assert!(!rang.contains(&"accept".to_owned()) && !rang.contains(&"reject".to_owned()));
    assert_eq!(a.opened.microphones(), 0);
    a.act(CallAction::Answer { call: incoming.call }).await.unwrap();
    a.status(incoming.call, &CallStatus::Connected).await;
    assert_eq!(a.opened.microphones(), 1);
    tokio::time::sleep(TALK).await;
    a.act(CallAction::HangUp { call: incoming.call }).await.unwrap();
    assert_eq!(a.ended(incoming.call).await, End::HungUp);
    let sent = lab.sent_by(LAB_A);
    assert!(sent.contains(&"accept".to_owned()) && sent.last().map(String::as_str) == Some("leave"));
    let _ = lab.mock.close_peer(&id, LAB_A);

    // the second: declined by the action, the captured rejection
    let _second = lab.mock.ring(OUTSIDER, LAB_A);
    let incoming = a.incoming().await;
    a.act(CallAction::Decline { call: incoming.call }).await.unwrap();
    assert_eq!(a.ended(incoming.call).await, End::Declined);
    let sent = lab.sent_by(LAB_A);
    assert!(!sent.contains(&"accept".to_owned()), "{sent:?}");
    assert!(a.log.permitted_any("call_decline"), "{:#?}", a.log.permitted());
    assert!(a.log.denials().is_empty(), "{:#?}", a.log.denials());
    assert_eq!(a.opened.microphones(), 1, "no microphone for a declined call");
    a.shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_attach_refused_with_a_401_goes_again_and_the_invitation_rings_and_is_answered() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    lab.mock.refuse_next("POST", "/attach", 401).await;
    let id = lab.mock.ring(OUTSIDER, LAB_A);
    let incoming = a.incoming().await;
    a.act(CallAction::Answer { call: incoming.call }).await.unwrap();
    a.status(incoming.call, &CallStatus::Connected).await;
    a.act(CallAction::HangUp { call: incoming.call }).await.unwrap();
    assert_eq!(a.ended(incoming.call).await, End::HungUp);
    assert!(a.log.denials().is_empty(), "{:#?}", a.log.denials());
    let attaches = a.log.permitted().iter().filter(|line| line.contains("/attach")).count();
    assert_eq!(
        attaches,
        2,
        "the refused attach and the one sent again: {:#?}",
        a.log.permitted()
    );
    let _ = lab.mock.close_peer(&id, LAB_A);
    a.shut_down().await;
}

/// An invitation whose `attach` failed for good (refused for a reason no further attempt clears, or never sent for a
/// credential that couldn't be had) is let go: it never rings, and it holds no call open, so the next one rings.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_invitation_whose_attach_failed_for_good_is_let_go_and_the_next_one_rings() {
    for credential in [false, true] {
        let lab = Lab::start().await;
        let mut a = lab.account(LAB_A, A_TONE).await;
        if credential {
            a.failing.store(true, Ordering::SeqCst);
        } else {
            lab.mock.refuse_next("POST", "/attach", 404).await;
        }
        let _first = lab.mock.ring(OUTSIDER, LAB_A);
        let quiet = a.events_within(Duration::from_secs(1)).await;
        assert!(
            !quiet.iter().any(|event| matches!(event, Event::IncomingCall(_))),
            "credential: {credential}: {quiet:#?}"
        );
        assert!(!a.failing.load(Ordering::SeqCst), "the attach asked for its skypetoken");
        let _second = lab.mock.ring(OUTSIDER, LAB_A);
        let incoming = a.incoming().await;
        a.act(CallAction::Decline { call: incoming.call }).await.unwrap();
        assert_eq!(a.ended(incoming.call).await, End::Declined);
        assert!(a.log.denials().is_empty(), "{:#?}", a.log.denials());
        a.shut_down().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_invitation_during_a_call_is_not_offered_and_nothing_is_sent_for_it() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    let call = a.act(CallAction::TestCall { echo: echo() }).await.unwrap();
    a.status(call, &CallStatus::Connected).await;
    let _second = lab.mock.ring(OUTSIDER, LAB_A);
    let quiet = a.events_within(Duration::from_secs(1)).await;
    assert!(
        !quiet.iter().any(|event| matches!(event, Event::IncomingCall(_))),
        "{quiet:#?}"
    );
    assert!(lab.sent_by(LAB_A).is_empty(), "nothing for the second invitation");
    a.act(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(a.ended(call).await, End::HungUp);
    assert!(a.log.denials().is_empty(), "{:#?}", a.log.denials());
    a.shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_running_group_call_is_joined_in_a_chat_listing_the_account() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    let id = lab.mock.ring_group(LAB_B, ABC_CHAT, &[LAB_A]);
    let incoming = a.incoming().await;
    assert_eq!(incoming.group_chat.as_deref(), Some(ABC_CHAT));
    assert!(lab.mock.end_ring(&id, 487, TIMED_OUT));
    a.next(|event| matches!(event, Event::RingEnded { .. }).then_some(()))
        .await;
    assert_eq!(a.opened.microphones(), 0);
    let call = a.act(CallAction::Join { chat: ABC_CHAT.into() }).await.unwrap();
    a.status(call, &CallStatus::Connected).await;
    assert_eq!(a.opened.microphones(), 1, "opened once the join passed");
    tokio::time::sleep(TALK).await;
    a.act(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(a.ended(call).await, End::HungUp);
    assert!(lab.sent_by(LAB_A).contains(&"join".to_owned()));
    assert!(a.log.denials().is_empty(), "{:#?}", a.log.denials());
    let _ = lab.mock.close_peer(&id, LAB_A);
    assert!(heard(&a.recording, PEER_TONE, TALK) > HEARD);
    a.shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_plain_requests_a_lab_call_sends_each_have_a_personal_route() {
    // a lab call's requests besides its sealed call writes and typed frames: the calling registration (an `allowed`
    // line in the lab's fence.log), the TURN credentials (a `GET`). Through the personal fence the first is its
    // own-state entry (`calling_endpoint_registration`, the test above) and the second goes as the read it is.
    let mock = ChatServiceMock::start().await.unwrap();
    let log = SharedLog::default();
    let fence = Arc::new(Fence::personal(Mri::new(LAB_A), FenceLog::from_writer(log.clone())));
    let tokens = Arc::new(Tokens {
        ic3: mock.token(LAB_A).unwrap(),
        skypetoken: String::new(),
        renewals: broadcast::channel(4).0,
        failing: Arc::default(),
    });
    let transport = HttpTransport::with_mock_origin(tokens, Arc::clone(&fence), CHAT_BASE, &mock.origin()).unwrap();
    mock.answer_next(
        "GET",
        "/tokens",
        200,
        &json!({
            "tokens": [{"realm": "\"rtcmedia\"", "username": "placeholder-user", "password": "placeholder-password"}],
            "expires": 604_800,
        }),
    )
    .await;
    let answer = transport.read(&RelayTokens).await.unwrap();
    let relay = RelayTokens::read_answer(answer.body()).unwrap();
    assert_eq!(relay.credentials.len(), 1);
    assert!(
        log.events().iter().all(|event| event != "allowed"),
        "a read, no plain request"
    );
}
