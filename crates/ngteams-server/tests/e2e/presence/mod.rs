//! Presence (UPS): the real own-cloud engine with its push channel, reading and watching a colleague's presence and
//! reporting its own, against the in-process server; and the service's wire, request by request.

mod nodes;
mod wire;

use std::sync::Arc;
use std::time::Duration;

use teams_auth::secrets::MemorySecretStore;
use teams_auth::session::Session;
use teams_core::account::{MIDDLE_TIER_GTM, PRESENCE_GTM};
use teams_core::engine::{self, EngineHandle, EngineOptions, Event, OwnStatus, Parts};
use teams_core::{Availability, Outbox};
use teams_proto::{Author, Client, Cloud, HttpTransport, ManualAvailability, Mri};
use teams_store::Store;
use teams_trouter::Trouter;
use tempfile::TempDir;

use crate::support::{Backend, TestServer, backends};
use crate::trouter::{ORIGIN, options, server_with};

pub(crate) type Transport = HttpTransport<Session<MemorySecretStore>>;

const WAIT: Duration = Duration::from_secs(40);
/// Longer than any test: what a watcher learns in a test, it learns from a push or from its first read.
const NEVER: Duration = Duration::from_hours(1);

/// The transport as the account bring-up builds it: every base from authz's GTMs.
pub(crate) async fn transport(session: &Arc<Session<MemorySecretStore>>, server: &TestServer, id: &str) -> Transport {
    session.skypetoken().await.unwrap();
    let base = |gtm: &str| {
        session
            .service_base(gtm)
            .unwrap_or_else(|| panic!("authz names no {gtm}"))
    };
    HttpTransport::for_cloud_with_mock_origin(
        Arc::clone(session),
        &base(teams_auth::authz::CHAT_BASE_GTM),
        Cloud::own(ORIGIN).unwrap(),
        &server.origin,
    )
    .unwrap()
    .for_account(Mri::new(format!("8:orgid:{id}")))
    .with_middle_tier(&base(MIDDLE_TIER_GTM))
    .unwrap()
    .with_presence(&base(PRESENCE_GTM))
    .unwrap()
}

/// One running client of an account: its engine with its push channel.
pub(crate) struct Seat {
    pub mri: String,
    pub engine: EngineHandle,
    pub client: Arc<Client<Transport>>,
    _dir: TempDir,
}

impl Seat {
    /// `upn`'s client, its requests through `http` and its push socket on `socket`.
    pub(crate) async fn start(http: &TestServer, socket: &TestServer, upn: &str, id: &str) -> Self {
        let session = Arc::new(http.session(upn).await);
        // the reads carry Loki, as the app's account does
        let client = Arc::new(Client::new(transport(&session, http, id).await.with_loki()));
        let writes = Arc::new(transport(&session, http, id).await);
        let trouter = Trouter::with_mock_origin(
            Arc::clone(&writes),
            Arc::clone(&session),
            &socket.origin.replace("http://", "ws://"),
        )
        .unwrap()
        .with_options(options());
        let dir = tempfile::tempdir().unwrap();
        let mri = format!("8:orgid:{id}");
        let engine = engine::start(Parts {
            store: Store::open(&dir.path().join("account.sqlite")).unwrap(),
            client: Arc::clone(&client),
            outbox: Outbox::new(writes, Author::new(Mri::new(mri.clone()), upn)),
            calls: None,
            push: Some(engine::Push::new(trouter)),
            options: EngineOptions {
                tick: Duration::from_millis(20),
                presence_poll: NEVER,
                presence_poll_subscribed: NEVER,
                ..EngineOptions::default()
            },
            known_names: Vec::new(),
        })
        .unwrap();
        Self {
            mri,
            engine,
            client,
            _dir: dir,
        }
    }

    /// Reports the account's own availability, once its push channel registered its endpoint.
    pub(crate) async fn report(&self, status: OwnStatus) {
        let until = tokio::time::Instant::now() + WAIT;
        loop {
            match self.engine.own_availability(status).await {
                Ok(_) => return,
                Err(error) => assert!(tokio::time::Instant::now() < until, "{status:?} wasn't sent: {error:?}"),
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Waits until the engine tells that `mri` shows as `wanted`.
    pub(crate) async fn sees(&mut self, mri: &str, wanted: &Availability) {
        let told = tokio::time::timeout(WAIT, async {
            loop {
                match self.engine.next_event().await {
                    Some(Event::Presence { mri: of, availability }) if of == mri && availability == *wanted => {
                        return;
                    }
                    Some(_) => {}
                    None => panic!("the engine stopped"),
                }
            }
        })
        .await;
        assert!(told.is_ok(), "{mri} never showed as {wanted:?}");
    }
}

/// Waits until an endpoint watches `mri` on `server`.
pub(crate) async fn watched(server: &TestServer, mri: &str) {
    let until = tokio::time::Instant::now() + WAIT;
    while server.app.presence.watchers(mri).await.unwrap() == 0 {
        assert!(tokio::time::Instant::now() < until, "nobody watches {mri}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_engine_reads_a_colleague_who_never_signed_in_as_offline() {
    for backend in backends() {
        let server = server_with(backend, |_| {}).await;
        let a = server.add_user("pa@own.test", "Ada").await;
        let b = server.add_user("pb@own.test", "Bert").await;
        let mut ada = Seat::start(&server, &server, "pa@own.test", &a).await;
        let bert = format!("8:orgid:{b}");
        assert_eq!(ada.engine.presence(bert.clone()).await.unwrap(), Availability::Unknown);
        ada.sees(&bert, &Availability::Offline).await;
        ada.engine.shutdown();
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_colleagues_availability_and_manual_status_are_pushed_to_an_engine_that_watches_them() {
    for backend in backends() {
        let server = server_with(backend, |_| {}).await;
        let a = server.add_user("pa@own.test", "Ada").await;
        let b = server.add_user("pb@own.test", "Bert").await;
        let mut ada = Seat::start(&server, &server, "pa@own.test", &a).await;
        let bert = Seat::start(&server, &server, "pb@own.test", &b).await;
        ada.engine.presence(bert.mri.clone()).await.unwrap();
        ada.sees(&bert.mri, &Availability::Offline).await;
        watched(&server, &bert.mri).await;

        // a manual status shows nothing while the account has no live endpoint
        bert.report(OwnStatus::Manual(ManualAvailability::Busy)).await;
        bert.report(OwnStatus::Available).await;
        ada.sees(&bert.mri, &Availability::Busy).await;
        for (status, shown) in [
            (ManualAvailability::DoNotDisturb, Availability::DoNotDisturb),
            (ManualAvailability::Away, Availability::Away),
            (ManualAvailability::Available, Availability::Available),
            (ManualAvailability::Busy, Availability::Busy),
        ] {
            bert.report(OwnStatus::Manual(status)).await;
            ada.sees(&bert.mri, &shown).await;
        }
        bert.report(OwnStatus::ClearManual).await;
        ada.sees(&bert.mri, &Availability::Available).await;
        ada.engine.shutdown();
        bert.engine.shutdown();
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_endpoint_that_is_not_reported_again_lapses_and_its_watchers_are_told() {
    let server = server_with(Backend::Sqlite, |config| config.presence.endpoint_ttl = 2).await;
    let a = server.add_user("pa@own.test", "Ada").await;
    let b = server.add_user("pb@own.test", "Bert").await;
    let mut ada = Seat::start(&server, &server, "pa@own.test", &a).await;
    let bert = Seat::start(&server, &server, "pb@own.test", &b).await;
    ada.engine.presence(bert.mri.clone()).await.unwrap();
    ada.sees(&bert.mri, &Availability::Offline).await;
    watched(&server, &bert.mri).await;
    bert.report(OwnStatus::Available).await;
    ada.sees(&bert.mri, &Availability::Available).await;
    ada.sees(&bert.mri, &Availability::Offline).await;
    ada.engine.shutdown();
    bert.engine.shutdown();
    server.stop().await;
}
