//! Others' presence pushed (the subscription of the account's endpoint, UPS's deliveries) through a running engine,
//! against the mock chat service, its registrar, its UPS and its Trouter: once the push channel registered, the
//! watched people are subscribed in full (the earlier subscriptions purged), anyone newly watched added, never a bot;
//! a delivery changes a person's presence as a read would and is told once, a stored replay is left alone; a reconnect
//! subscribes again in full; while the subscription covers everyone the reads slow to the subscribed poll, and someone
//! UPS only lets be polled keeps them at the minute.
//!
//! Tokens and ids here are the mock's, made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Event, Parts, Push};
use teams_core::{Availability, Outbox};
use teams_fence::{Fence, FenceLog, Mri, Roster};
use teams_mock::{CHAT_BASE, ChatServiceMock, ECHO, LAB_A, LAB_B, LAB_C, OUTSIDER, PRESENCE_BASE, TrouterMock};
use teams_proto::{Author, Client, Credentials, HttpTransport};
use teams_store::Store;
use teams_trouter::{Options, RegistrationReport, Trouter, TrouterCredentials};
use tokio::sync::broadcast;
use wiremock::Request;

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

struct Rig {
    mock: ChatServiceMock,
    trouter: TrouterMock,
    handle: EngineHandle,
    _dir: tempfile::TempDir,
}

/// Lab account a's engine against the mocks, with the presence base and a push channel, reading presence every `poll`
/// without a subscription covering everyone and every `subscribed` with one. The mock's UPS answers every read
/// `Offline` and takes every subscription but of those in `polling_only`.
async fn rig(poll: Duration, subscribed: Duration, polling_only: &[&str]) -> Rig {
    let mock = ChatServiceMock::start().await.unwrap();
    mock.serve_presence(&[]).await;
    mock.serve_presence_subscriptions(polling_only).await;
    let trouter = mock.start_trouter().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let tokens = Arc::new(Tokens {
        token: mock.token(LAB_A).unwrap(),
        renewals: broadcast::channel(4).0,
    });
    let _fence = Arc::new(Fence::new(
        Roster::new([LAB_A, LAB_B, LAB_C].map(Mri::new), [Mri::new(ECHO)]),
        FenceLog::from_writer(std::io::sink()),
    ));
    let origin = mock.origin();
    let http = || {
        HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin)
            .unwrap()
            .with_presence(PRESENCE_BASE)
            .unwrap()
    };
    let transport = Arc::new(http());
    let push = Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), trouter.origin())
        .unwrap()
        .with_options(Options {
            backoff_base: Duration::from_millis(50),
            backoff_cap: Duration::from_millis(50),
            backoff_jitter_percent: 0,
            ..Options::default()
        });
    let outbox = Outbox::new(transport, Author::new(Mri::new(LAB_A), "Lab A"));
    let handle = engine::start(Parts {
        calls: None,
        store: Store::open(&dir.path().join("store.sqlite")).unwrap(),
        client: Arc::new(Client::new(http())),
        outbox,
        push: Some(Push::new(push)),
        options: EngineOptions {
            tick: Duration::from_millis(50),
            presence_poll: poll,
            presence_poll_subscribed: subscribed,
            ..EngineOptions::default()
        },
        known_names: Vec::new(),
    })
    .unwrap();
    Rig {
        mock,
        trouter,
        handle,
        _dir: dir,
    }
}

/// The presence of `mri` as UPS delivers it (`tests/fixtures/trouter/presence-deliveries.json`'s shape).
fn delivery(mri: &str, availability: &str, activity: &str) -> Value {
    json!({"presence": [{
        "mri": mri,
        "etag": "A0000000110",
        "presence": {
            "sourceNetwork": "SameEnterprise",
            "calendarData": {"isOutOfOffice": false},
            "capabilities": [],
            "availability": availability,
            "activity": activity,
            "lastActiveTime": "2023-11-14T22:13:20.000Z",
            "deviceType": "Web",
        },
    }]})
}

impl Rig {
    /// Waits until the push channel's registration went through.
    async fn registered(&mut self) {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let event = tokio::time::timeout_at(deadline, self.handle.next_event())
                .await
                .expect("no registration in time")
                .expect("the engine stopped");
            match event {
                Event::Registration(RegistrationReport::Registered) => return,
                Event::Registration(RegistrationReport::Failed(why)) => panic!("the registration failed: {why}"),
                _ => {}
            }
        }
    }

    /// Waits for the next `Event::Presence`, failing on any `Event::Failed`.
    async fn next_presence(&mut self) -> (String, Availability) {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let event = tokio::time::timeout_at(deadline, self.handle.next_event())
                .await
                .expect("no presence event in time")
                .expect("the engine stopped");
            match event {
                Event::Presence { mri, availability } => return (mri, availability),
                Event::Failed { during, error } => panic!("failed {during}: {error}"),
                _ => {}
            }
        }
    }

    /// The `Event::Presence`s told within `within`.
    async fn presences_within(&mut self, within: Duration) -> Vec<(String, Availability)> {
        let deadline = tokio::time::Instant::now() + within;
        let mut told = Vec::new();
        while let Ok(Some(event)) = tokio::time::timeout_at(deadline, self.handle.next_event()).await {
            if let Event::Presence { mri, availability } = event {
                told.push((mri, availability));
            }
        }
        told
    }

    async fn requests(&self, part: &str) -> Vec<Request> {
        self.mock
            .received_requests()
            .await
            .into_iter()
            .filter(|request| request.url.path().contains(part))
            .collect()
    }

    /// The subscriptions UPS got, as their bodies.
    async fn subscriptions(&self) -> Vec<Value> {
        self.requests("/pubsub/subscriptions/")
            .await
            .iter()
            .map(|request| serde_json::from_slice(&request.body).unwrap())
            .collect()
    }

    /// Waits until UPS got at least `count` subscriptions; their bodies.
    async fn subscriptions_at_least(&self, count: usize) -> Vec<Value> {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let subscriptions = self.subscriptions().await;
            if subscriptions.len() >= count {
                return subscriptions;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{} subscription(s) in time, not {count}",
                subscriptions.len()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn reads(&self) -> usize {
        self.requests("/presence/getpresence/").await.len()
    }
}

/// The MRIs a subscription's body adds.
fn added(subscription: &Value) -> Vec<String> {
    subscription["subscriptionsToAdd"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["mri"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn a_delivery_changes_a_badge_and_a_stored_replay_is_left_alone() {
    let mut rig = rig(Duration::from_hours(1), Duration::from_hours(1), &[]).await;
    rig.registered().await;
    rig.handle.presence(LAB_B).await.unwrap();
    assert_eq!(rig.next_presence().await, (LAB_B.to_owned(), Availability::Offline));
    let subscriptions = rig.subscriptions_at_least(1).await;
    assert_eq!(subscriptions[0]["shouldPurgePreviousSubscriptions"], true);
    assert_eq!(added(&subscriptions[0]), [LAB_B]);
    let trouter_uri = subscriptions[0]["trouterUri"].as_str().unwrap();
    assert!(
        trouter_uri.ends_with("/unifiedPresenceService") && trouter_uri.contains("/v4/f/"),
        "{trouter_uri}"
    );

    // b goes into a call: pushed, told at once
    rig.trouter
        .deliver_presence(LAB_A, &delivery(LAB_B, "Busy", "InACall"), false);
    assert_eq!(rig.next_presence().await, (LAB_B.to_owned(), Availability::Busy));
    assert_eq!(rig.handle.presence(LAB_B).await.unwrap(), Availability::Busy);
    // the same again changes nothing; a replay of an older change on a connect is left alone; someone not watched too
    rig.trouter
        .deliver_presence(LAB_A, &delivery(LAB_B, "Busy", "InACall"), false);
    rig.trouter
        .deliver_presence(LAB_A, &delivery(LAB_B, "Offline", "Offline"), true);
    rig.trouter
        .deliver_presence(LAB_A, &delivery(LAB_C, "Available", "Available"), false);
    assert_eq!(rig.presences_within(Duration::from_millis(500)).await, Vec::new());
    assert_eq!(rig.handle.presence(LAB_B).await.unwrap(), Availability::Busy);

    // the next change comes too
    rig.trouter
        .deliver_presence(LAB_A, &delivery(LAB_B, "Available", "Available"), false);
    assert_eq!(rig.next_presence().await, (LAB_B.to_owned(), Availability::Available));
    assert_eq!(rig.reads().await, 1, "no read beside the first: the poll is an hour");
    rig.handle.shutdown();
}

#[tokio::test]
async fn the_subscription_names_whom_a_read_may_never_a_bot_and_adds_the_newly_watched() {
    let mut rig = rig(Duration::from_hours(1), Duration::from_hours(1), &[]).await;
    rig.registered().await;
    // a bot (the echo service of the test call) and b at once: one subscription in full, of b alone
    assert_eq!(rig.handle.presence(ECHO).await.unwrap(), Availability::Unknown);
    rig.handle.presence(LAB_B).await.unwrap();
    let subscriptions = rig.subscriptions_at_least(1).await;
    assert_eq!(added(&subscriptions[0]), [LAB_B]);
    assert_eq!(subscriptions[0]["shouldPurgePreviousSubscriptions"], true);

    // someone newly watched (outside the lab: a subscription reaches nobody) is added, without a purge
    rig.handle.presence(OUTSIDER).await.unwrap();
    let subscriptions = rig.subscriptions_at_least(2).await;
    assert_eq!(added(&subscriptions[1]), [OUTSIDER]);
    assert_eq!(subscriptions[1]["shouldPurgePreviousSubscriptions"], false);
    assert_eq!(subscriptions[1]["subscriptionsToRemove"], json!([]));

    // asked about again, nobody is added again
    rig.handle.presence(LAB_B).await.unwrap();
    rig.handle.presence(ECHO).await.unwrap();
    let _ = rig.presences_within(Duration::from_millis(700)).await;
    let subscriptions = rig.subscriptions().await;
    assert_eq!(subscriptions.len(), 2, "{subscriptions:?}");
    for subscription in &subscriptions {
        assert!(!subscription.to_string().contains(ECHO), "a bot named: {subscription}");
    }
    rig.handle.shutdown();
}

#[tokio::test]
async fn more_than_eight_watched_are_read_and_subscribed_eight_at_a_time() {
    let mut rig = rig(Duration::from_hours(1), Duration::from_hours(1), &[]).await;
    rig.registered().await;
    let people: Vec<String> = (1..=10_u32)
        .map(|n| format!("8:orgid:{n:08x}-0000-4000-8000-000000000001"))
        .collect();
    for person in &people {
        rig.handle.presence(person.clone()).await.unwrap();
    }
    let mut told = Vec::new();
    while told.len() < people.len() {
        told.push(rig.next_presence().await);
    }
    let subscriptions = rig.subscriptions_at_least(2).await;
    let reads = rig.requests("/presence/getpresence/").await;
    for read in &reads {
        let named: Vec<Value> = serde_json::from_slice(&read.body).unwrap();
        assert!(named.len() <= 8, "a read naming {}", named.len());
    }
    for subscription in &subscriptions {
        assert!(added(subscription).len() <= 8, "{subscription}");
    }
    let subscribed: Vec<String> = subscriptions.iter().flat_map(added).collect();
    assert_eq!(
        subscribed.len(),
        people.len(),
        "everyone subscribed once: {subscribed:?}"
    );
    assert_eq!(
        subscriptions
            .iter()
            .filter(|subscription| subscription["shouldPurgePreviousSubscriptions"] == true)
            .count(),
        1,
        "only the first request of a subscription in full purges"
    );
    rig.handle.shutdown();
}

#[tokio::test]
async fn a_reconnect_subscribes_again_in_full() {
    let mut rig = rig(Duration::from_hours(1), Duration::from_hours(1), &[]).await;
    rig.registered().await;
    rig.handle.presence(LAB_B).await.unwrap();
    rig.handle.presence(LAB_C).await.unwrap();
    let first = rig.subscriptions_at_least(1).await;
    assert_eq!(first[0]["shouldPurgePreviousSubscriptions"], true);

    // the network fails: the push channel reconnects and registers again, and the subscription is made in full anew
    let before = rig.subscriptions().await.len();
    rig.trouter.drop_sockets(LAB_A);
    rig.registered().await;
    let subscriptions = rig.subscriptions_at_least(before + 1).await;
    let again = subscriptions.last().unwrap();
    assert_eq!(again["shouldPurgePreviousSubscriptions"], true);
    let mut named = added(again);
    named.sort();
    assert_eq!(named, [LAB_B, LAB_C]);
    // and a change pushed on the new socket is told
    rig.trouter
        .deliver_presence(LAB_A, &delivery(LAB_C, "Away", "Away"), false);
    let mut told = rig.next_presence().await;
    while told.0 != LAB_C || told.1 == Availability::Offline {
        told = rig.next_presence().await;
    }
    assert_eq!(told, (LAB_C.to_owned(), Availability::Away));
    rig.handle.shutdown();
}

#[tokio::test]
async fn the_reads_slow_while_the_subscription_covers_everyone_and_fall_back_to_the_minute_otherwise() {
    // everyone taken: after the first reads, none within a second of 150 ms polls
    let mut covered = rig(Duration::from_millis(150), Duration::from_hours(1), &[]).await;
    covered.registered().await;
    covered.handle.presence(LAB_B).await.unwrap();
    covered.next_presence().await;
    covered.subscriptions_at_least(1).await;
    // the subscription's answer is in once a poll's time passed without a read
    tokio::time::sleep(Duration::from_millis(400)).await;
    let settled = covered.reads().await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(
        covered.reads().await,
        settled,
        "no read while the subscription covers b"
    );

    // the push channel goes down and stays down (every new socket closed before it is connected): nothing is live,
    // and the reads are back at the minute's pace
    covered.trouter.withhold_connected(usize::MAX);
    covered.trouter.drop_sockets(LAB_A);
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        covered.reads().await >= settled + 3,
        "read at the minute's pace once the socket is down"
    );
    covered.handle.shutdown();

    // b only polled (UPS's 30010): the reads keep the minute's pace though the socket is up
    let mut polled = rig(Duration::from_millis(150), Duration::from_hours(1), &[LAB_B]).await;
    polled.registered().await;
    polled.handle.presence(LAB_B).await.unwrap();
    polled.next_presence().await;
    polled.subscriptions_at_least(1).await;
    let before = polled.reads().await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(polled.reads().await >= before + 3, "read on at the minute's pace");
    polled.handle.shutdown();
}
