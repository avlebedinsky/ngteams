//! The push channel: the client's own Trouter session (`teams-trouter`) against the in-process server, with the
//! registrar it registers its endpoint at and the chat service whose events it is pushed.

mod connect;
mod deliveries;
mod gaps;
mod nodes;
mod refusals;
mod replay;

use std::sync::Arc;
use std::time::Duration;

use teams_auth::secrets::MemorySecretStore;
use teams_auth::session::Session;
use teams_proto::{Client, Cloud, HttpTransport, Mri};
use teams_trouter::{Delivery, Options, PushEvent, RegistrationReport, RegistrationTimes, Trouter, TrouterHandle};

use crate::support::{Backend, TestServer};

pub const ORIGIN: &str = "https://own.example.test";
const WAIT: Duration = Duration::from_secs(30);

type Transport = HttpTransport<Session<MemorySecretStore>>;

/// A server whose public origin is [`ORIGIN`], as the client's own cloud names it.
pub async fn server(backend: Backend) -> TestServer {
    server_with(backend, |_| {}).await
}

pub async fn server_with(backend: Backend, configure: impl FnOnce(&mut ngteams_server::config::Config)) -> TestServer {
    TestServer::start_with(backend, |config| {
        config.public_origin = url::Url::parse(ORIGIN).unwrap();
        configure(config);
    })
    .await
}

/// One signed-in account of the client: its session, and the transports its requests go through.
pub struct Account {
    pub mri: String,
    pub session: Arc<Session<MemorySecretStore>>,
}

impl Account {
    pub async fn sign_in(server: &TestServer, upn: &str, id: &str) -> Self {
        Self {
            mri: format!("8:orgid:{id}"),
            session: Arc::new(server.session(upn).await),
        }
    }

    /// The account's requests through `server`.
    pub fn transport(&self, server: &TestServer) -> Transport {
        HttpTransport::for_cloud_with_mock_origin(
            Arc::clone(&self.session),
            &format!("{ORIGIN}/api/chatsvc/own"),
            Cloud::own(ORIGIN).unwrap(),
            &server.origin,
        )
        .unwrap()
        .for_account(Mri::new(self.mri.clone()))
    }

    pub fn client(&self, server: &TestServer) -> Client<Transport> {
        Client::new(self.transport(server))
    }

    /// The account's messaging push channel, not yet running: its socket on `socket`, its registrations through
    /// `registrar`.
    pub fn trouter_on(
        &self,
        socket: &TestServer,
        registrar: &TestServer,
    ) -> Trouter<Transport, Session<MemorySecretStore>> {
        Trouter::with_mock_origin(
            Arc::new(self.transport(registrar)),
            Arc::clone(&self.session),
            &socket.origin.replace("http://", "ws://"),
        )
        .unwrap()
        .with_options(options())
    }

    pub fn trouter(&self, server: &TestServer) -> Trouter<Transport, Session<MemorySecretStore>> {
        self.trouter_on(server, server)
    }

    pub fn push(&self, server: &TestServer) -> Push {
        Push(self.trouter(server).spawn())
    }
}

/// The account's messaging push channel, running, once it is connected and its endpoint registered: deliveries
/// come from here on.
pub async fn registered_push(account: &Account, server: &TestServer) -> Push {
    let mut push = account.push(server);
    push.until(|event| matches!(event, PushEvent::Registration(RegistrationReport::Registered)).then_some(()))
        .await;
    push
}

/// The next chat delivery.
pub async fn delivery(push: &mut Push) -> Delivery {
    push.until(|event| match event {
        PushEvent::Delivery(delivery) => Some(delivery),
        _ => None,
    })
    .await
}

/// Short waits, so a reconnect or a ping takes a test moments.
pub fn options() -> Options {
    Options {
        backoff_base: Duration::from_millis(300),
        backoff_jitter_percent: 0,
        registration: RegistrationTimes::default(),
        ..Options::default()
    }
}

/// A running push channel and what it told so far.
pub struct Push(pub TrouterHandle);

impl Push {
    pub async fn next(&mut self) -> PushEvent {
        tokio::time::timeout(WAIT, self.0.next())
            .await
            .expect("the push channel told nothing in time")
            .expect("the push channel stopped")
    }

    /// The next event `pick` takes; the ones before it are dropped.
    pub async fn until<T>(&mut self, mut pick: impl FnMut(PushEvent) -> Option<T>) -> T {
        loop {
            if let Some(found) = pick(self.next().await) {
                return found;
            }
        }
    }

    /// Whether nothing `unwanted` comes for `quiet`.
    pub async fn quiet(&mut self, quiet: Duration, mut unwanted: impl FnMut(&PushEvent) -> bool) -> bool {
        let until = tokio::time::Instant::now() + quiet;
        loop {
            match tokio::time::timeout_at(until, self.0.next()).await {
                Err(_) | Ok(None) => return true,
                Ok(Some(event)) if unwanted(&event) => return false,
                Ok(Some(_)) => {}
            }
        }
    }

    pub async fn stop(self) {
        self.0.stop().await;
    }
}
