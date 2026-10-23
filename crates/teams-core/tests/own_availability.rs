//! The account's own availability (M2 Task 19) through a running engine, against the mock chat service, its registrar,
//! its UPS and its Trouter: the endpoint's `PUT` and the manual status (set, cleared) go out as transient writes of the
//! outbox, each under the fence's permit for its own account (logged to `fence.log`, no member read), naming the
//! endpoint the push channel registered in this run; nothing goes out before a registration went through, and a fence
//! that isn't the account's own denies it with nothing sent.
//!
//! Tokens and ids here are the mock's, made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::io::{self, Write};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use serde_json::Value;
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Event, Parts, Push};
use teams_core::{EngineError, Outbox, OwnStatus, Transient, TransientSent};
use teams_fence::{Fence, FenceLog, LiveMembershipSource, MembershipSource, Mri, Roster};
use teams_mock::{
    AvailabilityChange, CHAT_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, OwnAvailability, PRESENCE_BASE, TrouterMock,
};
use teams_proto::{Author, Client, Credentials, EndpointRegistration, HttpTransport, ManualAvailability, register};
use teams_store::Store;
use teams_trouter::{Options, RegistrationReport, Trouter, TrouterCredentials};
use tokio::sync::broadcast;

/// How long a test waits for an event.
const WAIT: Duration = Duration::from_secs(30);

/// One account's mock token, for the transport and the push channel alike.
struct Tokens {
    token: String,
    renewals: broadcast::Sender<Renewal>,
}

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

impl TrouterCredentials for Tokens {
    fn ic3(&self) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(AccessToken {
            token: Secret::new(self.token.clone()),
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
    /// The fence's decisions on writes (not the requests it logged).
    fn decisions(&self) -> Vec<Value> {
        let bytes = self.0.lock().unwrap_or_else(PoisonError::into_inner).clone();
        String::from_utf8(bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|line| line.get("kind").is_some())
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

struct Rig {
    handle: EngineHandle,
    log: SharedLog,
    _dir: tempfile::TempDir,
}

/// Lab account a's engine against the mocks, its fence's own account `own` (none for `None`), its transport given the
/// presence base, with a push channel when `push`.
fn rig(mock: &ChatServiceMock, trouter: &TrouterMock, own: Option<&str>, push: bool) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let log = SharedLog::default();
    let tokens = Arc::new(Tokens {
        token: mock.token(LAB_A).unwrap(),
        renewals: broadcast::channel(4).0,
    });
    let fence = Fence::new(
        Roster::new([LAB_A, LAB_B, LAB_C].map(Mri::new), []),
        FenceLog::from_writer(log.clone()),
    );
    let fence = Arc::new(match own {
        Some(mri) => fence.with_own_account(Mri::new(mri)),
        None => fence,
    });
    let origin = mock.origin();
    let http = || {
        HttpTransport::with_mock_origin(Arc::clone(&tokens), Arc::clone(&fence), CHAT_BASE, &origin)
            .unwrap()
            .with_presence(PRESENCE_BASE)
            .unwrap()
    };
    let transport = Arc::new(http());
    let live = LiveMembershipSource::with_mock_origin(&origin).unwrap();
    live.set_credential_source(transport.clone());
    let membership: Arc<dyn MembershipSource + Send> = Arc::new(live);
    let channel = push.then(|| {
        let trouter = Trouter::with_mock_origin(
            Arc::clone(&transport),
            Arc::clone(&tokens),
            Arc::clone(&fence),
            trouter.origin(),
        )
        .unwrap()
        .with_options(Options {
            backoff_base: Duration::from_millis(50),
            backoff_cap: Duration::from_millis(50),
            backoff_jitter_percent: 0,
            ..Options::default()
        });
        Push::new(trouter)
    });
    let client = Arc::new(Client::new(http()));
    let outbox = Outbox::new(transport, fence, membership, Author::new(Mri::new(LAB_A), "Lab A"));
    let handle = engine::start(Parts {
        store: Store::open(&dir.path().join("store.sqlite")).unwrap(),
        client,
        outbox,
        push: channel,
        options: EngineOptions {
            tick: Duration::from_millis(50),
            ..EngineOptions::default()
        },
        known_names: Vec::new(),
    })
    .unwrap();
    Rig { handle, log, _dir: dir }
}

/// Waits until the push channel's registration went through.
async fn registered(handle: &mut EngineHandle) {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let event = tokio::time::timeout_at(deadline, handle.next_event())
            .await
            .expect("the registration came in time")
            .expect("the engine runs");
        match event {
            Event::Registration(RegistrationReport::Registered) => return,
            Event::Registration(RegistrationReport::Failed(why)) => panic!("the registration failed: {why}"),
            _ => {}
        }
    }
}

#[tokio::test]
async fn the_account_sets_its_own_availability_from_its_registered_endpoint() {
    let mock = ChatServiceMock::start().await.unwrap();
    let trouter = mock.start_trouter().unwrap();
    let mut rig = rig(&mock, &trouter, Some(LAB_A), true);
    registered(&mut rig.handle).await;
    let requests = rig.handle.requests();

    let steps = [
        OwnStatus::Available,
        OwnStatus::Manual(ManualAvailability::Busy),
        OwnStatus::Manual(ManualAvailability::DoNotDisturb),
        OwnStatus::Manual(ManualAvailability::Away),
        OwnStatus::Manual(ManualAvailability::Available),
        OwnStatus::ClearManual,
    ];
    for status in steps {
        assert_eq!(
            requests.own_availability(status).await.unwrap(),
            TransientSent::Sent,
            "{status:?}"
        );
    }

    // UPS took each, naming the endpoint a registered with the registrar in this run, and nothing else
    let endpoint = trouter
        .registrations()
        .into_iter()
        .find(|registration| registration.account == LAB_A)
        .unwrap()
        .registration_id;
    let change = |change| OwnAvailability {
        account: LAB_A.to_owned(),
        endpoint: endpoint.clone(),
        change,
    };
    assert_eq!(
        mock.own_availability(),
        [
            change(AvailabilityChange::Endpoint {
                availability: "Available".to_owned(),
                activity: "Available".to_owned(),
            }),
            change(AvailabilityChange::Manual("Busy".to_owned())),
            change(AvailabilityChange::Manual("DoNotDisturb".to_owned())),
            change(AvailabilityChange::Manual("Away".to_owned())),
            change(AvailabilityChange::Manual("Available".to_owned())),
            change(AvailabilityChange::Reset),
        ]
    );
    // each under the fence's permit for its own account, logged
    let decisions = rig.log.decisions();
    assert_eq!(decisions.len(), steps.len());
    for decision in &decisions {
        assert_eq!(decision["kind"], "own_availability", "{decision}");
        assert_eq!(decision["verdict"], "allow", "{decision}");
        assert_eq!(decision["scope"], serde_json::json!({"account": {"mri": LAB_A}}));
    }
    rig.handle.shutdown();
}

#[tokio::test]
async fn nothing_goes_out_without_an_endpoint_registered_in_this_run() {
    let mock = ChatServiceMock::start().await.unwrap();
    let trouter = mock.start_trouter().unwrap();
    // a one-shot engine: no push channel, so no registration
    let rig = rig(&mock, &trouter, Some(LAB_A), false);
    let requests = rig.handle.requests();
    for status in [
        OwnStatus::Available,
        OwnStatus::Manual(ManualAvailability::Busy),
        OwnStatus::ClearManual,
    ] {
        let error = requests.own_availability(status).await.unwrap_err();
        assert!(matches!(error, EngineError::NoEndpoint), "{status:?}: {error:?}");
        assert_eq!(error.kind(), "no endpoint");
    }
    assert!(mock.own_availability().is_empty());
    assert!(rig.log.decisions().is_empty(), "nothing was even asked of the fence");
    rig.handle.shutdown();
}

#[tokio::test]
async fn a_fence_that_isnt_the_accounts_own_denies_it_and_nothing_goes_out() {
    let mock = ChatServiceMock::start().await.unwrap();
    let trouter = mock.start_trouter().unwrap();
    for own in [None, Some(LAB_B)] {
        let mut rig = rig(&mock, &trouter, own, true);
        registered(&mut rig.handle).await;
        let sent = rig
            .handle
            .requests()
            .own_availability(OwnStatus::Available)
            .await
            .unwrap();
        assert!(
            matches!(&sent, TransientSent::Denied { reason } if reason.contains("not the fence's own account")),
            "{own:?}: {sent:?}"
        );
        let decisions = rig.log.decisions();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0]["reason"], "not_own_account");
        rig.handle.shutdown();
    }
    assert!(mock.own_availability().is_empty(), "UPS was told nothing");
}

#[tokio::test]
async fn an_availability_writes_to_no_chat_and_only_the_newer_of_its_kind_replaces_it() {
    let mock = ChatServiceMock::start().await.unwrap();
    let tokens = Arc::new(Tokens {
        token: mock.token(LAB_A).unwrap(),
        renewals: broadcast::channel(4).0,
    });
    let fence = Arc::new(Fence::new(
        Roster::new([LAB_A, LAB_B, LAB_C].map(Mri::new), []),
        FenceLog::from_writer(io::sink()),
    ));
    let transport = HttpTransport::with_mock_origin(tokens, fence, CHAT_BASE, &mock.origin()).unwrap();
    let registration = EndpointRegistration::messaging(
        "00000000-0000-4000-8000-0000000000e1",
        "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint00000A/",
    )
    .unwrap();
    let (_, endpoint) = register(&transport, &registration).await.unwrap();
    let endpoint = endpoint.expect("the mock's registrar took it");

    let available = Transient::EndpointAvailable {
        endpoint: endpoint.clone(),
    };
    let busy = Transient::ManualStatus {
        endpoint: endpoint.clone(),
        status: Some(ManualAvailability::Busy),
    };
    let reset = Transient::ManualStatus { endpoint, status: None };
    let typing = Transient::Typing {
        conversation: teams_mock::AB_CHAT.to_owned(),
    };
    for own in [&available, &busy, &reset] {
        assert_eq!(own.conversation(), None, "{own:?}");
        assert!(!own.replaces(&typing) && !typing.replaces(own), "{own:?}");
    }
    assert!(
        reset.replaces(&busy) && busy.replaces(&reset),
        "the newer manual status wins"
    );
    assert!(available.replaces(&available));
    assert!(!available.replaces(&busy) && !busy.replaces(&available));
}
