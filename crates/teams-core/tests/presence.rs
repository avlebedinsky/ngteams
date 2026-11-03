//! Others' presence (M2 Task 17) through a running engine, against wiremock as UPS (the transport's `test-support`
//! mock origin): a person a view asked about is read at once and then every poll, in one read of everyone watched
//! (whoever they are: a read reaches nobody; logged to `fence.log` as `allowed` before it is sent); each change told
//! once as `Event::Presence`; a failed read making everyone it asked about unknown; a read the transport's one retry
//! left throttled waiting its `Retry-After`, whoever is asked about meanwhile; the watch's cap dropping the one asked
//! about longest ago, told as unknown; and nothing read without the presence base.
//!
//! Tokens and ids here are made up; the answers follow the synthetic fixture `tests/fixtures/ups/getpresence-answer.json`.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Event, Parts};
use teams_core::{Availability, Outbox};
use teams_fence::Mri;
use teams_proto::{Author, Client, Credentials, HttpTransport};
use teams_store::Store;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const GUID_A: &str = "0000000a-0000-4000-8000-00000000000a";
const GUID_B: &str = "0000000b-0000-4000-8000-00000000000b";
const GUID_C: &str = "0000000c-0000-4000-8000-00000000000c";
const GUID_OUT: &str = "0000000f-0000-4000-8000-00000000000f";
const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const PRESENCE_BASE: &str = "https://teams.microsoft.com/ups/noam";
const GET_PRESENCE: &str = "/ups/noam/v1/presence/getpresence/";
/// How long a test waits for an event before it fails.
const WAIT: Duration = Duration::from_secs(30);
/// The longest the transport waits before its one retry of a throttled read.
const RETRY_WAIT: Duration = Duration::from_millis(50);

fn mri(guid: &str) -> String {
    format!("8:orgid:{guid}")
}

/// An item of UPS's answer, in the fixture's shape, for `mri` as `availability`.
fn item(mri: &str, availability: &str) -> Value {
    json!({
        "mri": mri,
        "presence": {
            "sourceNetwork": "SameEnterprise",
            "calendarData": {"isOutOfOffice": false},
            "capabilities": [],
            "availability": availability,
            "activity": availability,
            "lastActiveTime": "0001-01-01T00:00:00Z",
        },
        "etagMatch": false,
        "etag": "made-up-tag",
        "status": 20000,
    })
}

/// UPS's answer: each asked MRI of the request as `availability`.
fn answer_all(request: &Request, availability: &str) -> ResponseTemplate {
    let asked: Vec<Value> = serde_json::from_slice(&request.body).unwrap();
    let items: Vec<Value> = asked
        .iter()
        .map(|ask| item(ask["mri"].as_str().unwrap(), availability))
        .collect();
    ResponseTemplate::new(200).set_body_raw(Value::Array(items).to_string(), "application/json")
}

/// The MRIs a received presence read asked about.
fn asked(request: &Request) -> Vec<String> {
    let asked: Vec<Value> = serde_json::from_slice(&request.body).unwrap();
    asked
        .iter()
        .map(|ask| ask["mri"].as_str().unwrap().to_owned())
        .collect()
}

/// Placeholder tokens for every audience.
struct FakeTokens;

impl Credentials for FakeTokens {
    fn bearer(&self, audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(format!("placeholder-token-for-{audience}"))))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

struct Rig {
    server: MockServer,
    handle: EngineHandle,
    _dir: tempfile::TempDir,
}

/// An engine of lab account a (the lab: a, b and c), its reads on `server`, polling presence every `poll`; its reads'
/// transport given the presence base only when `presence`.
fn rig(server: MockServer, poll: Duration, presence: bool) -> Rig {
    rig_of(server, poll, presence, vec![mri(GUID_B), mri(GUID_C)])
}

/// [`rig`], the lab a and `others`.
fn rig_of(server: MockServer, poll: Duration, presence: bool, _others: Vec<String>) -> Rig {
    let dir = tempfile::tempdir().unwrap();

    let tokens = Arc::new(FakeTokens);
    let transport = || {
        HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &server.uri())
            .unwrap()
            .with_max_retry_wait(RETRY_WAIT)
    };
    let reads = if presence {
        transport().with_presence(PRESENCE_BASE).unwrap()
    } else {
        transport()
    };
    let author = Author::new(Mri::new(mri(GUID_A)), "lab-a");
    let outbox = Outbox::new(Arc::new(transport()), author);
    let store = Store::open(&dir.path().join("store").join("a.sqlite")).unwrap();
    let handle = engine::start(Parts {
        calls: None,
        store,
        client: Arc::new(Client::new(reads)),
        outbox,
        push: None,
        options: EngineOptions {
            tick: Duration::from_millis(50),
            presence_poll: poll,
            ..EngineOptions::default()
        },
        known_names: Vec::new(),
    })
    .unwrap();
    Rig {
        server,
        handle,
        _dir: dir,
    }
}

impl Rig {
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

    async fn reads(&self) -> Vec<Request> {
        self.server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|request| request.url.path() == GET_PRESENCE)
            .collect()
    }
}

#[tokio::test]
async fn a_persons_presence_is_read_at_once_and_told_logged_as_allowed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(GET_PRESENCE))
        .respond_with(|request: &Request| answer_all(request, "Offline"))
        .mount(&server)
        .await;
    let mut rig = rig(server, Duration::from_hours(1), true);

    // nothing read yet: unknown, and a read starts on the next tick
    assert_eq!(rig.handle.presence(mri(GUID_B)).await.unwrap(), Availability::Unknown);
    assert_eq!(rig.next_presence().await, (mri(GUID_B), Availability::Offline));
    assert_eq!(rig.handle.presence(mri(GUID_B)).await.unwrap(), Availability::Offline);

    let reads = rig.reads().await;
    assert_eq!(reads.len(), 1, "read once: the poll is an hour");
    assert_eq!(asked(&reads[0]), [mri(GUID_B)]);

    rig.handle.shutdown();
}

#[tokio::test]
async fn a_bot_is_never_watched_nor_named_by_a_read() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(GET_PRESENCE))
        .respond_with(|request: &Request| answer_all(request, "Available"))
        .mount(&server)
        .await;
    let mut rig = rig(server, Duration::from_hours(1), true);
    // the echo service of the test call: a `28:` id, which has no presence and is never read
    let bot = "28:00000000-0000-4000-8000-0000000000ec".to_owned();

    assert_eq!(rig.handle.presence(bot.clone()).await.unwrap(), Availability::Unknown);
    assert_eq!(rig.handle.presence(mri(GUID_B)).await.unwrap(), Availability::Unknown);
    assert_eq!(rig.next_presence().await, (mri(GUID_B), Availability::Available));
    let reads = rig.reads().await;
    assert_eq!(reads.len(), 1);
    assert_eq!(asked(&reads[0]), [mri(GUID_B)], "the bot left out");
    assert_eq!(rig.handle.presence(bot).await.unwrap(), Availability::Unknown);
    rig.handle.shutdown();
}

#[tokio::test]
async fn everyone_watched_is_read_in_one_read_each_poll_and_only_a_change_is_told() {
    let server = MockServer::start().await;
    let answered = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&answered);
    Mock::given(method("POST"))
        .and(path(GET_PRESENCE))
        .respond_with(move |request: &Request| {
            // Offline twice, then Available from the third read on
            let n = count.fetch_add(1, Ordering::SeqCst);
            answer_all(request, if n < 2 { "Offline" } else { "Available" })
        })
        .mount(&server)
        .await;
    let mut rig = rig(server, Duration::from_millis(300), true);

    rig.handle.presence(mri(GUID_B)).await.unwrap();
    assert_eq!(rig.next_presence().await, (mri(GUID_B), Availability::Offline));
    rig.handle.presence(mri(GUID_C)).await.unwrap();
    assert_eq!(rig.next_presence().await, (mri(GUID_C), Availability::Offline));
    // the next change: both Available, each told once
    let mut changed = vec![rig.next_presence().await, rig.next_presence().await];
    changed.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(
        changed,
        [
            (mri(GUID_B), Availability::Available),
            (mri(GUID_C), Availability::Available)
        ]
    );
    // later reads give the same: nothing more is told
    assert_eq!(rig.presences_within(Duration::from_millis(900)).await, Vec::new());

    let reads = rig.reads().await;
    assert!(reads.len() >= 3, "{}", reads.len());
    assert_eq!(asked(&reads[0]), [mri(GUID_B)]);
    for read in &reads[1..] {
        assert_eq!(asked(read), [mri(GUID_B), mri(GUID_C)], "one read of everyone watched");
    }
    rig.handle.shutdown();
}

#[tokio::test]
async fn someone_outside_the_lab_is_read_like_anyone() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(GET_PRESENCE))
        .respond_with(|request: &Request| answer_all(request, "Busy"))
        .mount(&server)
        .await;
    let mut rig = rig(server, Duration::from_hours(1), true);

    assert_eq!(rig.handle.presence(mri(GUID_OUT)).await.unwrap(), Availability::Unknown);
    assert_eq!(rig.next_presence().await, (mri(GUID_OUT), Availability::Busy));
    assert_eq!(rig.handle.presence(mri(GUID_OUT)).await.unwrap(), Availability::Busy);

    let reads = rig.reads().await;
    assert_eq!(reads.len(), 1);
    assert_eq!(asked(&reads[0]), [mri(GUID_OUT)]);

    rig.handle.shutdown();
}

#[tokio::test]
async fn a_failed_read_or_an_answer_naming_anyone_not_asked_makes_everyone_asked_unknown() {
    let failures = [
        ResponseTemplate::new(500),
        ResponseTemplate::new(200).set_body_raw("not json", "application/json"),
        ResponseTemplate::new(200).set_body_raw(
            json!([item(&mri(GUID_B), "Available"), item(&mri(GUID_OUT), "Available")]).to_string(),
            "application/json",
        ),
    ];
    for failure in failures {
        let server = MockServer::start().await;
        let answered = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&answered);
        Mock::given(method("POST"))
            .and(path(GET_PRESENCE))
            .respond_with(move |request: &Request| {
                if count.fetch_add(1, Ordering::SeqCst) == 0 {
                    answer_all(request, "Available")
                } else {
                    failure.clone()
                }
            })
            .mount(&server)
            .await;
        let mut rig = rig(server, Duration::from_millis(300), true);

        rig.handle.presence(mri(GUID_B)).await.unwrap();
        assert_eq!(rig.next_presence().await, (mri(GUID_B), Availability::Available));
        assert_eq!(rig.next_presence().await, (mri(GUID_B), Availability::Unknown));
        assert_eq!(rig.handle.presence(mri(GUID_B)).await.unwrap(), Availability::Unknown);
        rig.handle.shutdown();
    }
}

#[tokio::test]
async fn an_item_left_out_or_without_presence_is_unknown_and_an_unseen_word_is_kept() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(GET_PRESENCE))
        .respond_with(|_: &Request| {
            let answer = json!([
                item(&mri(GUID_B), "SomethingNew"),
                {"mri": mri(GUID_C), "status": 40400},
            ]);
            ResponseTemplate::new(200).set_body_raw(answer.to_string(), "application/json")
        })
        .mount(&server)
        .await;
    let mut rig = rig(server, Duration::from_hours(1), true);

    rig.handle.presence(mri(GUID_B)).await.unwrap();
    rig.handle.presence(mri(GUID_C)).await.unwrap();
    rig.handle.presence(mri(GUID_A)).await.unwrap();
    let mut told = vec![rig.next_presence().await];
    told.extend(rig.presences_within(Duration::from_millis(500)).await);

    // B's word is kept as it came; C (no presence) and a (left out) stay unknown, which is no change to tell
    assert_eq!(told, [(mri(GUID_B), Availability::Other("SomethingNew".to_owned()))]);
    assert_eq!(rig.handle.presence(mri(GUID_C)).await.unwrap(), Availability::Unknown);
    assert_eq!(rig.handle.presence(mri(GUID_A)).await.unwrap(), Availability::Unknown);
    rig.handle.shutdown();
}

#[tokio::test]
async fn a_throttled_read_waits_its_retry_after() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(GET_PRESENCE))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "5"))
        .mount(&server)
        .await;
    let mut rig = rig(server, Duration::from_millis(100), true);

    rig.handle.presence(mri(GUID_B)).await.unwrap();
    // the first read is sent at once, and not retried inside the transport; within the next second
    // (ten polls) no other goes out
    let deadline = tokio::time::Instant::now() + WAIT;
    while rig.reads().await.is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "no read in time");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(rig.presences_within(Duration::from_secs(1)).await, Vec::new());
    assert_eq!(rig.reads().await.len(), 1);
    rig.handle.shutdown();
}

#[tokio::test]
async fn someone_new_asked_about_while_throttled_brings_no_read_before_the_retry_after() {
    let server = MockServer::start().await;
    let answered = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&answered);
    Mock::given(method("POST"))
        .and(path(GET_PRESENCE))
        .respond_with(move |request: &Request| {
            // the first read answered, every later one throttled for five minutes
            if count.fetch_add(1, Ordering::SeqCst) == 0 {
                answer_all(request, "Available")
            } else {
                ResponseTemplate::new(429).insert_header("Retry-After", "300")
            }
        })
        .mount(&server)
        .await;
    let mut rig = rig(server, Duration::from_millis(100), true);

    rig.handle.presence(mri(GUID_B)).await.unwrap();
    assert_eq!(rig.next_presence().await, (mri(GUID_B), Availability::Available));
    // the throttled read is not retried inside the transport: b unknown again, its Retry-After kept
    assert_eq!(rig.next_presence().await, (mri(GUID_B), Availability::Unknown));
    assert_eq!(rig.reads().await.len(), 2);

    // someone new inside the wait (a lab account, and someone outside the lab): no read goes out
    rig.handle.presence(mri(GUID_C)).await.unwrap();
    rig.handle.presence(mri(GUID_OUT)).await.unwrap();
    assert_eq!(rig.presences_within(Duration::from_secs(1)).await, Vec::new());
    assert_eq!(rig.reads().await.len(), 2, "no read inside the Retry-After");
    rig.handle.shutdown();
}

#[tokio::test]
async fn past_the_cap_the_one_asked_about_longest_ago_is_dropped_and_told_unknown() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(GET_PRESENCE))
        .respond_with(|request: &Request| answer_all(request, "Offline"))
        .mount(&server)
        .await;
    // 40 people besides the lab a
    let lab: Vec<String> = (1..=40_u32)
        .map(|n| mri(&format!("{n:08x}-0000-4000-8000-000000000001")))
        .collect();
    let mut rig = rig_of(server, Duration::from_hours(1), true, lab.clone());
    let first = &lab[0];
    let told_first =
        |told: Vec<(String, Availability)>| told.into_iter().filter(|(mri, _)| mri == first).collect::<Vec<_>>();

    rig.handle.presence(first.clone()).await.unwrap();
    assert_eq!(rig.next_presence().await, (first.clone(), Availability::Offline));

    // 19 more fill the watch; asked about again, the first is the latest asked, so the next two drop others
    for other in &lab[1..20] {
        rig.handle.presence(other.clone()).await.unwrap();
    }
    rig.handle.presence(first.clone()).await.unwrap();
    for other in &lab[20..22] {
        rig.handle.presence(other.clone()).await.unwrap();
    }
    // a drop is told as the ask that made it is answered: none of the first
    assert_eq!(
        told_first(rig.presences_within(Duration::from_millis(300)).await),
        Vec::new()
    );

    // 18 more: the first is now the one asked about longest ago, and dropped
    for other in &lab[22..] {
        rig.handle.presence(other.clone()).await.unwrap();
    }
    assert_eq!(
        told_first(rig.presences_within(Duration::from_millis(300)).await),
        [(first.clone(), Availability::Unknown)]
    );
    rig.handle.shutdown();
}

#[tokio::test]
async fn without_the_presence_base_nothing_is_read_or_logged() {
    let server = MockServer::start().await;
    let mut rig = rig(server, Duration::from_millis(100), false);

    assert_eq!(rig.handle.presence(mri(GUID_B)).await.unwrap(), Availability::Unknown);
    assert_eq!(rig.presences_within(Duration::from_millis(500)).await, Vec::new());

    assert!(rig.server.received_requests().await.unwrap().is_empty());

    rig.handle.shutdown();
}

#[test]
fn ups_words_are_read_as_their_availability_and_an_unseen_one_kept() {
    let cases = [
        (Some("Available"), Availability::Available),
        (Some("AvailableIdle"), Availability::AvailableIdle),
        (Some("Away"), Availability::Away),
        (Some("BeRightBack"), Availability::BeRightBack),
        (Some("Busy"), Availability::Busy),
        (Some("BusyIdle"), Availability::BusyIdle),
        (Some("DoNotDisturb"), Availability::DoNotDisturb),
        (Some("Offline"), Availability::Offline),
        (Some("PresenceUnknown"), Availability::Unknown),
        (None, Availability::Unknown),
        (Some("offline"), Availability::Other("offline".to_owned())),
        (Some("InAMeeting"), Availability::Other("InAMeeting".to_owned())),
    ];
    for (word, availability) in cases {
        assert_eq!(Availability::of(word), availability, "{word:?}");
    }
}
