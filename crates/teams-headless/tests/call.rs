//! `call` on the mock (M3 Task 15): the command places a call through the engine, its media on the file backend (the
//! keyed tone written first, the far end recorded), holds it once it connected, hangs it up and finds its tone in the
//! recording. The test call goes to the mock's echo stand-in (a peer that plays back what it hears a set time later), a
//! call from a chat to b's native engine, which b's own action answers; a call nobody answers is hung up once its time
//! to connect is over; a signal before the calling socket registered places nothing, and so does a signal, or the time
//! to connect running out, while the call's media is still being set up.
//!
//! Every id, token and MRI here is the mock's made-up one.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use clap::Parser as _;
use serde_json::Value;
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Event, Parts};
use teams_core::lab::{LAB_LANGUAGE, web_client_info};
use teams_core::{CallAction, CallParts, End, Identity, Outbox, Relay};
use teams_fence::{Fence, FenceLog, LiveMembershipSource, MembershipSource, Mri, Roster};
use teams_headless::call::{self, CallFiles, Plan, Report, Target};
use teams_headless::chat::LabNames;
use teams_headless::cli::{Cli, Command as CliCommand, DEFAULT_TONE_HZ};
use teams_headless::lab::call_media;
use teams_media::relay::RelayServer;
use teams_media::turn::Transport as RelayTransport;
use teams_mock::{AB_CHAT, AC_CHAT, CHAT_BASE, ChatServiceMock, ECHO, LAB_A, LAB_B, LAB_C, TENANT, TrouterMock};
use teams_proto::{Author, Client, Credentials, HttpTransport};
use teams_store::Store;
use teams_trouter::{CallStack, Options, Trouter, TrouterCredentials};
use tokio::sync::{Notify, broadcast};

/// How long the echo stand-in waits before it plays back what it heard.
const ECHO_DELAY: Duration = Duration::from_millis(400);

/// A short call: quick to register and connect on the mock, held 4 s.
const PLAN: Plan = Plan {
    register_within: Duration::from_secs(30),
    connect_within: Duration::from_secs(30),
    hold: Duration::from_secs(4),
    end_within: Duration::from_secs(10),
};

// --- the command line ---

#[test]
fn call_takes_a_target_its_tone_what_to_hear_a_recording_and_its_times() {
    let parsed = Cli::try_parse_from(["teams-headless", "a", "call", "echo"]).unwrap();
    match parsed.command {
        CliCommand::Call {
            target,
            tone,
            hear,
            record,
            seconds,
            wait,
        } => {
            assert_eq!(target, "echo");
            assert!((tone - DEFAULT_TONE_HZ).abs() < f32::EPSILON);
            assert_eq!((hear, record, seconds, wait), (None, None, 45, 60));
        }
        other => panic!("{other:?}"),
    }
    let parsed = Cli::try_parse_from([
        "teams-headless",
        "b",
        "call",
        "abc",
        "--tone",
        "660",
        "--hear",
        "440",
        "--record",
        "/somewhere/b.wav",
        "--seconds",
        "30",
        "--wait",
        "90",
    ])
    .unwrap();
    match parsed.command {
        CliCommand::Call {
            target,
            tone,
            hear,
            record,
            seconds,
            wait,
        } => {
            assert_eq!(target, "abc");
            assert!((tone - 660.0).abs() < f32::EPSILON);
            assert_eq!(hear, Some(440.0));
            assert_eq!(record.as_deref(), Some(Path::new("/somewhere/b.wav")));
            assert_eq!((seconds, wait), (30, 90));
        }
        other => panic!("{other:?}"),
    }
    assert!(
        Cli::try_parse_from(["teams-headless", "a", "call"]).is_err(),
        "a target is needed"
    );
}

#[test]
fn the_keyed_tone_is_written_beside_the_recording_long_enough_for_the_whole_call() {
    let dir = tempfile::tempdir().unwrap();
    let files = CallFiles::write(dir.path().join("a-call-1.wav"), 1_000.0, &PLAN).unwrap();
    assert_eq!(files.sent, dir.path().join("a-call-1-sent.wav"));
    assert_eq!(files.length, Duration::from_secs(30 + 4 + 10 + 10));
    let samples = teams_media::devices::wav::read(&files.sent).unwrap();
    assert_eq!(samples.len(), 48_000 * 54);
    // the files it plays and records
    let devices = files.devices();
    assert_eq!(devices.recording.as_deref(), Some(files.recording.as_path()));
}

// --- the calls on the mock ---

#[tokio::test]
async fn call_echo_places_the_test_call_holds_it_hangs_it_up_and_finds_its_tone_played_back() {
    let lab = Lab::start_echo().await;
    let mut a = lab.account(LAB_A, 1_000.0);
    let (report, printed) = a.call(Target::Echo(Mri::new(ECHO)), &PLAN).await;
    assert!(report.completed(), "{report:?}\n{printed}");
    let setup = report.setup.unwrap();
    assert!(setup < Duration::from_secs(20), "{setup:?}");
    for line in [
        "calling_registration registered=true",
        "call_placing",
        "status=\"ringing\"",
        "status=\"connected\"",
        "status=\"leaving\"",
        "end=\"hung_up\"",
    ] {
        assert!(printed.contains(line), "{line}:\n{printed}");
    }
    assert!(
        printed.lines().any(|line| line.contains("call_summary")
            && line.contains("held=true")
            && line.contains("end=\"hung_up\"")),
        "{printed}"
    );
    assert_eq!(lab.sent_by(LAB_A), ["start", "updateEndpointMetadata", "add", "leave"]);
    let (log, files) = a.shut_down().await;
    assert!(log.denials().is_empty(), "{:#?}", log.denials());
    for what in ["\"call\"", "\"add\"", "\"leave\""] {
        assert!(log.permitted_any(what), "{what}: {:#?}", log.lines());
    }

    // the echo stand-in played the tone back ECHO_DELAY later, plus the way there and back
    let echo = files.heard(1_000.0).unwrap().expect("the tone came back");
    eprintln!("{}", call::echo_line(Some(&echo)));
    assert!(!echo.blocks.is_empty() && echo.delays.len() >= 3, "{echo:?}");
    let p50 = echo.delay_percentile(50).unwrap();
    assert!(
        p50 >= ECHO_DELAY && p50 < ECHO_DELAY + Duration::from_millis(800),
        "{p50:?}"
    );
    assert!(echo.level() > 0.1, "{echo:?}");
    // another tone isn't in it
    assert_eq!(files.heard(660.0).unwrap(), None);
}

#[tokio::test]
async fn call_b_places_a_call_from_the_chat_that_b_s_own_action_answers_and_b_hears_the_tone() {
    let lab = Lab::start_echo().await;
    let mut a = lab.account(LAB_A, 1_000.0);
    let mut b = lab.account(LAB_B, 660.0);
    b.ready().await;
    let answering = b.answer_the_next_call();
    let (report, printed) = a.call(Target::Chat(AB_CHAT.to_owned()), &PLAN).await;
    let b = answering.await.unwrap();
    assert!(report.completed(), "{report:?}\n{printed}");
    assert_eq!(lab.sent_by(LAB_A), ["start", "updateEndpointMetadata", "leave"]);
    let ((a_log, a_files), (b_log, b_files)) = (a.shut_down().await, b.shut_down().await);
    assert!(a_log.denials().is_empty() && b_log.denials().is_empty());
    // a's keyed tone is in b's recording, b's in a's
    let heard_by_b = call::heard(&b_files.recording, 1_000.0, a_files.length, a_files.length).unwrap();
    let heard_by_a = a_files.heard(660.0).unwrap();
    eprintln!("b: {}", call::echo_line(heard_by_b.as_ref()));
    eprintln!("a: {}", call::echo_line(heard_by_a.as_ref()));
    assert!(heard_by_b.is_some() && heard_by_a.is_some());
}

#[tokio::test]
async fn a_call_nobody_answers_is_hung_up_once_its_time_to_connect_is_over() {
    let lab = Lab::start_echo().await;
    let mut a = lab.account(LAB_A, 1_000.0);
    // c's engine isn't running: the call rings at nobody
    let plan = Plan {
        connect_within: Duration::from_secs(3),
        ..PLAN
    };
    let (report, printed) = a.call(Target::Chat(AC_CHAT.to_owned()), &plan).await;
    assert!(!report.completed(), "{report:?}");
    assert!(!report.held && report.setup.is_none(), "{report:?}");
    assert_eq!(report.end, Some(End::HungUp), "{printed}");
    assert!(printed.contains("call_not_connected_in_time"), "{printed}");
    let (log, _) = a.shut_down().await;
    assert!(log.denials().is_empty(), "{:#?}", log.denials());
}

#[tokio::test]
async fn a_signal_before_the_calling_socket_registered_places_nothing() {
    let lab = Lab::start_echo().await;
    let mut a = lab.account(LAB_A, 1_000.0);
    let mut out = Vec::new();
    let names = LabNames::new([Some(LAB_A), Some(LAB_B), Some(LAB_C)]);
    let report = call::run(
        &mut a.handle,
        Target::Echo(Mri::new(ECHO)),
        &PLAN,
        &names,
        &mut out,
        future::ready(()),
    )
    .await
    .unwrap();
    assert_eq!(
        report,
        Report {
            interrupted: true,
            ..Report::default()
        }
    );
    assert!(lab.mock.calls().is_empty());
    let (log, _) = a.shut_down().await;
    assert!(log.permitted().is_empty(), "{:#?}", log.permitted());
}

#[tokio::test]
async fn a_signal_while_the_call_s_media_is_set_up_places_nothing() {
    let lab = Lab::start_echo().await;
    // a relay that never answers: the media's setup waits for its allocation
    let silent = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut a = lab.account_relayed(LAB_A, 1_000.0, silent.local_addr().unwrap());
    let placing = Arc::new(Notify::new());
    let mut out = Output::new(Arc::clone(&placing));
    let names = LabNames::new([Some(LAB_A), Some(LAB_B), Some(LAB_C)]);
    let stop = async move { placing.notified().await };
    let report = tokio::time::timeout(
        Duration::from_secs(30),
        call::run(
            &mut a.handle,
            Target::Echo(Mri::new(ECHO)),
            &PLAN,
            &names,
            &mut out,
            stop,
        ),
    )
    .await
    .expect("the signal ends the wait")
    .unwrap();
    assert_eq!(
        report,
        Report {
            interrupted: true,
            ..Report::default()
        }
    );
    assert!(out.text().contains("call_placing"), "{}", out.text());
    let (log, _) = a.shut_down().await;
    assert!(lab.mock.calls().is_empty());
    assert!(log.permitted().is_empty(), "{:#?}", log.permitted());
}

#[tokio::test]
async fn a_call_whose_media_isn_t_set_up_within_its_time_to_connect_isn_t_placed() {
    let lab = Lab::start_echo().await;
    let silent = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut a = lab.account_relayed(LAB_A, 1_000.0, silent.local_addr().unwrap());
    let plan = Plan {
        connect_within: Duration::from_secs(1),
        ..PLAN
    };
    let (report, printed) = a.call(Target::Echo(Mri::new(ECHO)), &plan).await;
    assert_eq!(report, Report::default(), "{printed}");
    assert!(printed.contains("call_not_connected_in_time"), "{printed}");
    assert!(!printed.contains("call_summary"), "{printed}");
    let (log, _) = a.shut_down().await;
    assert!(lab.mock.calls().is_empty());
    assert!(log.permitted().is_empty(), "{:#?}", log.permitted());
}

/// What `call` prints, kept; `placing` is told once it printed `call_placing`.
struct Output {
    text: Vec<u8>,
    placing: Arc<Notify>,
}

impl Output {
    fn new(placing: Arc<Notify>) -> Self {
        Self {
            text: Vec::new(),
            placing,
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.text).into_owned()
    }
}

impl Write for Output {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.text.extend_from_slice(buf);
        if String::from_utf8_lossy(buf).contains("call_placing") {
            self.placing.notify_one();
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// --- the mock lab ---

/// One account's tokens: its `ic3` token and its skypetoken, as the mock takes them.
struct Tokens {
    ic3: String,
    skypetoken: String,
    renewals: broadcast::Sender<Renewal>,
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

    fn denials(&self) -> Vec<Value> {
        self.lines()
            .into_iter()
            .filter(|line| line["verdict"] == "deny")
            .collect()
    }

    /// The writes it permitted, each line as text.
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

/// The mock with its Trouter and its calls' media on, each peer the echo's stand-in.
struct Lab {
    mock: ChatServiceMock,
    trouter: TrouterMock,
    relay: std::net::SocketAddr,
    dir: tempfile::TempDir,
}

impl Lab {
    async fn start_echo() -> Self {
        let mock = ChatServiceMock::start().await.unwrap();
        let trouter = mock.start_trouter().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let relay = mock.start_echo_media(dir.path(), ECHO_DELAY).unwrap();
        Self {
            mock,
            trouter,
            relay,
            dir,
        }
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

    /// `mri`'s engine with its calling (no messaging push channel, as `call` runs it), its media on the files of a
    /// keyed tone at `hz`, relayed through the mock's TURN server.
    fn account(&self, mri: &'static str, hz: f32) -> Account {
        self.account_relayed(mri, hz, self.relay)
    }

    /// [`Self::account`] with its media relayed through the TURN server at `relay`.
    fn account_relayed(&self, mri: &'static str, hz: f32, relay: std::net::SocketAddr) -> Account {
        let log = SharedLog::default();
        let fence = Arc::new(
            Fence::new(
                Roster::new([LAB_A, LAB_B, LAB_C].map(Mri::new), [Mri::new(ECHO)]),
                FenceLog::from_writer(log.clone()),
            )
            .with_own_account(Mri::new(mri)),
        );
        let tokens = Arc::new(Tokens {
            ic3: self.mock.token(mri).unwrap(),
            skypetoken: self.mock.skypetoken(mri).unwrap(),
            renewals: broadcast::channel(4).0,
        });
        let origin = self.mock.origin();
        let http =
            || HttpTransport::with_mock_origin(Arc::clone(&tokens), Arc::clone(&fence), CHAT_BASE, &origin).unwrap();
        let transport = Arc::new(http());
        let live = LiveMembershipSource::with_mock_origin(&origin).unwrap();
        live.set_credential_source(transport.clone());
        let members: Arc<dyn MembershipSource + Send> = Arc::new(live);
        let calling = Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), self.trouter.origin())
            .unwrap()
            .with_options(Options {
                ping_interval: Duration::from_millis(300),
                pong_timeout: Duration::from_millis(1_500),
                backoff_base: Duration::from_millis(50),
                backoff_cap: Duration::from_millis(200),
                backoff_jitter_percent: 0,
                ..Options::default()
            })
            .for_calls(CallStack::new(Arc::clone(&fence), Arc::clone(&members)))
            .unwrap();
        let name = mri.rsplit(':').next().unwrap();
        let files = CallFiles::write(self.dir.path().join(format!("{name}-call.wav")), hz, &PLAN).unwrap();
        let relay = Relay::Fixed(vec![RelayServer {
            addr: relay,
            transport: RelayTransport::Udp,
        }]);
        let identity = Identity {
            mri: Mri::new(mri),
            display_name: Some("Lab".into()),
            endpoint_id: String::new(),
            language_id: Some(LAB_LANGUAGE.into()),
            tenant_id: TENANT.into(),
            client: web_client_info(),
            region: Some("amer".into()),
            partition: Some("amer01".into()),
        };
        let calls = CallParts::new(calling, identity, call_media(files.devices(), relay));
        let store = tempfile::tempdir().unwrap();
        let handle = engine::start(Parts {
            calls: Some(calls),
            store: Store::open(&store.path().join("store.sqlite")).unwrap(),
            client: Arc::new(Client::new(http())),
            outbox: Outbox::new(transport, fence, members, Author::new(Mri::new(mri), "Lab")),
            push: None,
            options: EngineOptions {
                tick: Duration::from_millis(50),
                ..EngineOptions::default()
            },
            known_names: Vec::new(),
        })
        .unwrap();
        Account {
            handle,
            log,
            files,
            _store: store,
        }
    }
}

/// One lab account's engine on the mock, with its call's files.
struct Account {
    handle: EngineHandle,
    log: SharedLog,
    files: CallFiles,
    _store: tempfile::TempDir,
}

impl Account {
    /// `call` to `target` under `plan`, with what it printed.
    async fn call(&mut self, target: Target, plan: &Plan) -> (Report, String) {
        let mut out = Vec::new();
        let names = LabNames::new([Some(LAB_A), Some(LAB_B), Some(LAB_C)]);
        let report = call::run(&mut self.handle, target, plan, &names, &mut out, future::pending())
            .await
            .unwrap();
        (report, String::from_utf8(out).unwrap())
    }

    /// Waits until its calling socket is registered.
    async fn ready(&mut self) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let event = tokio::time::timeout_at(deadline, self.handle.next_event())
                .await
                .expect("registered in time")
                .expect("the engine runs");
            if matches!(
                event,
                Event::CallingRegistration(teams_core::RegistrationReport::Registered)
            ) {
                return;
            }
        }
    }

    /// Answers the next call that rings here, by its own action (as the user would), and waits until it ended.
    fn answer_the_next_call(mut self) -> tokio::task::JoinHandle<Self> {
        tokio::spawn(async move {
            let requests = self.handle.requests();
            loop {
                let event = tokio::time::timeout(Duration::from_secs(60), self.handle.next_event())
                    .await
                    .expect("the call came and ended in time")
                    .expect("the engine runs");
                match event {
                    Event::IncomingCall(incoming) => {
                        requests.call(CallAction::Answer { call: incoming.call }).await.unwrap();
                    }
                    Event::Call {
                        status: teams_core::CallStatus::Ended(_),
                        ..
                    } => return self,
                    _ => {}
                }
            }
        })
    }

    /// Stops its engine (which closes its call's media and recording), off the test's runtime: its fence's log and
    /// its call's files.
    async fn shut_down(self) -> (SharedLog, CallFiles) {
        let Self { handle, log, files, .. } = self;
        tokio::task::spawn_blocking(move || handle.shutdown()).await.unwrap();
        (log, files)
    }
}
