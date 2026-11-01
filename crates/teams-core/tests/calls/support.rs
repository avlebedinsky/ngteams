//! What the call session's tests share: the mock chat service with its calling service, its Trouter and its media
//! (a loopback TURN server and a loopback peer behind each party it plays), and an engine per lab account with its
//! own store, fence and `fence.log`, its messaging socket and its calling socket, its calls' media on the file
//! backend (a tone in, a recording out, nothing processed, each opening of the microphone counted), relayed through
//! the mock's TURN server.
//!
//! Every id, token and MRI here is the mocks' made-up one.

use std::future::{self, Future};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use serde_json::Value;
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Event, Parts, Push};
use teams_core::{
    CallId, CallMedia, CallParts, CallStatus, CallVideo, CallView, End, Identity, IncomingCall, Outbox, Relay,
    VideoOutput,
};
use teams_fence::{Fence, FenceLog, LiveMembershipSource, MembershipSource, Mri, Roster};
use teams_media::devices::file::{AudioInput, FileDevices, FrameCounter, Pacing, PatternCamera};
use teams_media::devices::{AudioSink, AudioSource, DeviceError, Devices, VideoSink, VideoSource, wav};
use teams_media::processing::ProcessingConfig;
use teams_media::relay::RelayServer;
use teams_media::session::SessionConfig;
use teams_media::tone::{Tone, level_at};
use teams_media::video::codec::EncoderSettings;
use teams_media::video::convert::MaxSize;
use teams_mock::{CHAT_BASE, ChatServiceMock, ECHO, LAB_A, LAB_B, LAB_C, TENANT, TrouterMock};
use teams_proto::{Author, Client, ClientInfo, Credentials, HttpTransport};
use teams_store::Store;
use teams_trouter::{CallStack, Options, RegistrationReport, Trouter, TrouterCredentials};
use tokio::sync::broadcast;

/// How long a test waits for an event.
pub const WAIT: Duration = Duration::from_secs(30);
/// How long the media runs once connected, before what was heard is judged.
pub const TALK: Duration = Duration::from_secs(2);

/// Each party's tone: a's, b's, and the mock's peers'.
pub const A_TONE: Tone = Tone {
    hz: 440.0,
    amplitude: 0.5,
};
pub const B_TONE: Tone = Tone {
    hz: 660.0,
    amplitude: 0.5,
};
pub const PEER_TONE: Tone = Tone {
    hz: 880.0,
    amplitude: 0.5,
};

/// One account's tokens: its `ic3` token and its skypetoken, as the mock takes them.
pub struct Tokens {
    ic3: String,
    skypetoken: String,
    renewals: broadcast::Sender<Renewal>,
}

/// `mri`'s tokens on the lab's mock.
pub fn tokens(lab: &Lab, mri: &str) -> Tokens {
    Tokens {
        ic3: lab.mock.token(mri).unwrap(),
        skypetoken: lab.mock.skypetoken(mri).unwrap(),
        renewals: broadcast::channel(4).0,
    }
}

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.ic3.clone())))
    }

    fn bearer_rejected(&self, _audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.ic3.clone())))
    }

    fn skypetoken(&self) -> impl Future<Output = Result<Option<Secret>, TokenError>> + Send {
        future::ready(Ok(Some(Secret::new(self.skypetoken.clone()))))
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
pub struct SharedLog(Arc<Mutex<Vec<u8>>>);

impl SharedLog {
    pub fn lines(&self) -> Vec<Value> {
        let bytes = self.0.lock().unwrap_or_else(PoisonError::into_inner).clone();
        String::from_utf8(bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// Every line it denied.
    pub fn denials(&self) -> Vec<Value> {
        self.lines()
            .into_iter()
            .filter(|line| line["verdict"] == "deny")
            .collect()
    }

    /// The writes it permitted, each line as text (its kind, scope and call operation).
    pub fn permitted(&self) -> Vec<String> {
        self.lines()
            .into_iter()
            .filter(|line| line["verdict"] == "allow" && line["kind"].is_string() && line["event"].is_null())
            .map(|line| line.to_string())
            .collect()
    }

    /// Whether it permitted a write whose line names `what` (a kind, a call operation).
    pub fn permitted_any(&self, what: &str) -> bool {
        self.permitted().iter().any(|line| line.contains(what))
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
pub struct Lab {
    pub mock: ChatServiceMock,
    pub trouter: TrouterMock,
    pub relay: std::net::SocketAddr,
    pub dir: tempfile::TempDir,
}

impl Lab {
    pub async fn start() -> Self {
        super::diagnostics::install();
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

    /// The mock with its calls' media on, a group call's parties each with a mixer of its own (group video).
    pub async fn start_group_video() -> Self {
        super::diagnostics::install();
        let mock = ChatServiceMock::start().await.unwrap();
        let trouter = mock.start_trouter().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let relay = mock.start_group_video(dir.path(), PEER_TONE).unwrap();
        Self {
            mock,
            trouter,
            relay,
            dir,
        }
    }

    /// `mri`'s engine, its calls' media playing `tone`, ready: both its sockets registered.
    pub async fn account(&self, mri: &'static str, tone: Tone) -> Account {
        let mut account = Account::start(self, mri, tone, (TENANT, true));
        account.ready().await;
        account
    }

    /// A synthetic device backend that may deliberately fail after signaling was admitted.
    pub async fn account_with_devices(&self, devices: Arc<dyn Devices + Send + Sync>) -> Account {
        let mut account = Account::start_devices(self, LAB_A, A_TONE, (TENANT, true), Some(devices));
        account.ready().await;
        account
    }

    /// As [`Self::account`], its engine not knowing the account's tenant (as a lab binary's doesn't): a test call
    /// reads it from the account's token first.
    pub async fn account_without_tenant(&self, mri: &'static str, tone: Tone) -> Account {
        let mut account = Account::start(self, mri, tone, ("", true));
        account.ready().await;
        account
    }

    /// As [`Self::account`], its calls' media without video (none given to its engine).
    pub async fn account_without_video(&self, mri: &'static str, tone: Tone) -> Account {
        let mut account = Account::start(self, mri, tone, (TENANT, false));
        account.ready().await;
        account
    }

    /// The mock's newest call's id.
    pub fn last_call(&self) -> String {
        self.mock.calls().last().expect("a call").id.clone()
    }

    /// The operations `mri` sent on the mock's newest call, in order.
    pub fn sent_by(&self, mri: &str) -> Vec<String> {
        let calls = self.mock.calls();
        let call = calls.last().expect("a call");
        call.requests
            .iter()
            .filter(|(_, account)| account == mri)
            .map(|(op, _)| op.clone())
            .collect()
    }

    /// Every delivery response (`3:::`) on `mri`'s sockets.
    pub fn responses_of(&self, mri: &str) -> usize {
        self.trouter
            .sockets()
            .iter()
            .filter(|socket| socket.account.as_deref() == Some(mri))
            .flat_map(|socket| socket.frames.clone())
            .filter(|frame| frame.starts_with("3:::"))
            .count()
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

/// One lab account's engine on the mock.
pub struct Account {
    pub mri: &'static str,
    pub handle: EngineHandle,
    pub log: SharedLog,
    pub recording: PathBuf,
    /// How often its calls opened the microphone.
    microphones: Arc<AtomicUsize>,
    /// What its calls showed of the far end's video, and of its own camera.
    pub video: Shown,
    /// The newest view of each call told so far (whichever wait it came during).
    views: std::collections::HashMap<CallId, CallView>,
    _store: tempfile::TempDir,
}

/// The frames an account's calls showed: the far end's, decoded (a 1:1 call's on line 0, a group call's participants'
/// on the lines after it), and its own camera's as sent.
#[derive(Clone, Default)]
pub struct Shown {
    pub remote: FrameCounter,
    pub lines: Arc<std::sync::Mutex<std::collections::BTreeMap<usize, FrameCounter>>>,
    pub preview: FrameCounter,
}

impl Shown {
    /// The frames shown on group line `line` so far.
    pub fn line(&self, line: usize) -> u64 {
        self.lines
            .lock()
            .unwrap()
            .get(&line)
            .map_or(0, |counter| counter.counts().frames)
    }
}

impl VideoOutput for Shown {
    fn remote(&self, line: usize) -> Box<dyn VideoSink> {
        if line == 0 {
            return Box::new(self.remote.clone());
        }
        Box::new(self.lines.lock().unwrap().entry(line).or_default().clone())
    }

    fn preview(&self) -> Option<Box<dyn VideoSink>> {
        Some(Box::new(self.preview.clone()))
    }
}

impl Account {
    /// `mri`'s engine on `lab`, knowing `tenant` (none: empty), its calls' media with video when `video`.
    fn start(lab: &Lab, mri: &'static str, tone: Tone, (tenant, video_on): (&str, bool)) -> Self {
        Self::start_devices(lab, mri, tone, (tenant, video_on), None)
    }

    fn start_devices(
        lab: &Lab,
        mri: &'static str,
        tone: Tone,
        (tenant, video_on): (&str, bool),
        devices: Option<Arc<dyn Devices + Send + Sync>>,
    ) -> Self {
        let log = SharedLog::default();
        let fence = Arc::new(
            Fence::new(
                Roster::new([LAB_A, LAB_B, LAB_C].map(Mri::new), [Mri::new(ECHO)]),
                FenceLog::from_writer(log.clone()),
            )
            .with_own_account(Mri::new(mri)),
        );
        let tokens = Arc::new(tokens(lab, mri));
        let origin = lab.mock.origin();
        let http =
            || HttpTransport::with_mock_origin(Arc::clone(&tokens), Arc::clone(&fence), CHAT_BASE, &origin).unwrap();
        let transport = Arc::new(http());
        let live = LiveMembershipSource::with_mock_origin(&origin).unwrap();
        live.set_credential_source(transport.clone());
        let members: Arc<dyn MembershipSource + Send> = Arc::new(live);
        let trouter = || {
            Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), lab.trouter.origin())
                .unwrap()
                .with_options(options())
        };
        let calling = trouter().for_calls(CallStack::new(Arc::clone(&fence), Arc::clone(&members)));
        let store = tempfile::tempdir().unwrap();
        let recording = lab.dir.path().join(format!("{}.wav", mri.rsplit(':').next().unwrap()));
        let microphones = Arc::new(AtomicUsize::new(0));
        let video = Shown::default();
        let mut media = media(lab.relay, tone, &recording, Arc::clone(&microphones));
        if let Some(devices) = devices {
            media.devices = devices;
        }
        media.video = video_on.then(|| CallVideo {
            encoder: EncoderSettings::default(),
            max_size: MaxSize::HD,
            output: Arc::new(video.clone()),
        });
        let calls = CallParts::new(calling, identity(mri, tenant), media);
        let description = format!("{calls:?}");
        assert!(description.contains(mri));
        assert!(!description.contains("Lab"));
        assert!(!description.contains(&tokens.ic3));
        assert!(!description.contains(&tokens.skypetoken));
        let push = Push::new(trouter());
        let client = Arc::new(Client::new(http()));
        let outbox = Outbox::new(transport, fence, members, Author::new(Mri::new(mri), "Lab"));
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
            microphones,
            video,
            views: std::collections::HashMap::new(),
            _store: store,
        }
    }

    /// How often its calls opened the microphone so far.
    pub fn microphones(&self) -> usize {
        self.microphones.load(Ordering::SeqCst)
    }

    /// Stops its engine (which hangs its call up first), off the test's runtime: its fence's log, once it stopped.
    pub async fn shut_down(self) -> SharedLog {
        let Self { handle, log, .. } = self;
        tokio::task::spawn_blocking(move || handle.shutdown()).await.unwrap();
        log
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
    pub async fn next<T>(&mut self, mut pick: impl FnMut(&Event) -> Option<T>) -> T {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let event = tokio::time::timeout_at(deadline, self.handle.next_event())
                .await
                .unwrap_or_else(|_| panic!("{}: the event didn't come in time", self.mri))
                .expect("the engine runs");
            if let Event::CallParticipants { call, view } = &event {
                self.views.insert(*call, view.clone());
            }
            if let Some(picked) = pick(&event) {
                return picked;
            }
        }
    }

    /// Waits for `call` to be `wanted`; fails if it ends first.
    pub async fn status(&mut self, call: CallId, wanted: &CallStatus) {
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
    pub async fn ended(&mut self, call: CallId) -> End {
        self.next(|event| match event {
            Event::Call {
                call: of,
                status: CallStatus::Ended(end),
            } if *of == call => Some(end.clone()),
            _ => None,
        })
        .await
    }

    /// Waits until the view of `call` (who is in it, their lines, who speaks) is one `wanted` takes: that view.
    pub async fn view(&mut self, call: CallId, wanted: impl Fn(&CallView) -> bool) -> CallView {
        if let Some(view) = self.views.get(&call).filter(|view| wanted(view)) {
            return view.clone();
        }
        self.next(|event| match event {
            Event::CallParticipants { call: of, view } if *of == call && wanted(view) => Some(view.clone()),
            _ => None,
        })
        .await
    }

    /// Waits for an invitation to ring here.
    pub async fn incoming(&mut self) -> IncomingCall {
        self.next(|event| match event {
            Event::IncomingCall(incoming) => Some(incoming.clone()),
            _ => None,
        })
        .await
    }

    /// Every event within `quiet`, for a test that something didn't happen.
    pub async fn events_within(&mut self, quiet: Duration) -> Vec<Event> {
        let mut seen = Vec::new();
        let deadline = tokio::time::Instant::now() + quiet;
        while let Ok(Some(event)) = tokio::time::timeout_at(deadline, self.handle.next_event()).await {
            seen.push(event);
        }
        seen
    }
}

/// The account in its calls.
fn identity(mri: &str, tenant: &str) -> Identity {
    Identity {
        mri: Mri::new(mri),
        display_name: Some("Lab".into()),
        endpoint_id: String::new(),
        language_id: Some("en-us".into()),
        tenant_id: tenant.into(),
        client: ClientInfo {
            endpoint_capabilities: Some(73_463),
            client_endpoint_capabilities: Some(114_259_690),
            platform_name: Some("SyntheticNative/1".into()),
        },
        region: Some("amer".into()),
        partition: Some("amer01".into()),
    }
}

/// File devices that count the microphone's openings.
struct Counted {
    devices: FileDevices,
    microphones: Arc<AtomicUsize>,
}

impl Devices for Counted {
    fn microphone(&self) -> Result<Box<dyn AudioSource>, DeviceError> {
        self.microphones.fetch_add(1, Ordering::SeqCst);
        self.devices.microphone()
    }

    fn speaker(&self) -> Result<Box<dyn AudioSink>, DeviceError> {
        self.devices.speaker()
    }

    fn camera(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
        self.devices.camera()
    }
}

/// The calls' media: `tone` in, `recording` out, through the loopback TURN server at `relay`, nothing processed (a
/// steady tone is judged), each opening of the microphone counted in `microphones`.
fn media(relay: std::net::SocketAddr, tone: Tone, recording: &Path, microphones: Arc<AtomicUsize>) -> CallMedia {
    CallMedia {
        devices: Arc::new(Counted {
            devices: FileDevices {
                input: AudioInput::Tone(tone),
                recording: Some(recording.to_path_buf()),
                camera: PatternCamera::default(),
                pacing: Pacing::RealTime,
            },
            microphones,
        }),
        relay: Relay::Fixed(vec![RelayServer::udp(relay)]),
        session: SessionConfig {
            processing: ProcessingConfig::OFF,
        },
        video: None,
    }
}

/// The level of `tone` in the last `stretch` of the recording at `path` (once its media stopped), leaving out its last
/// 200 ms.
pub fn heard(path: &Path, tone: Tone, stretch: Duration) -> f32 {
    heard_before(path, tone, stretch, Duration::from_millis(200))
}

/// The level of `tone` in the `stretch` of the recording at `path` that ends `before` its end.
pub fn heard_before(path: &Path, tone: Tone, stretch: Duration, before: Duration) -> f32 {
    let samples = wav::read(path).unwrap();
    let at = |span: Duration| usize::try_from(span.as_millis()).unwrap() * 48;
    let end = samples.len().saturating_sub(at(before));
    let start = end.saturating_sub(at(stretch));
    assert!(
        end - start >= 24_000,
        "{} holds {} samples",
        path.display(),
        samples.len()
    );
    level_at(&samples[start..end], tone.hz)
}

/// Waits until `done` holds (a request the mock got is recorded a moment after its answer went).
pub async fn wait_until(done: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while !done() {
        assert!(tokio::time::Instant::now() < deadline, "it didn't happen in time");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
