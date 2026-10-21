//! People's names and pictures (M2 Task 12) through a running engine, against wiremock as Graph and the middle tier
//! (the transport's `test-support` mock origin): a name and a picture from Graph, kept in the store and told as
//! `Event::Profile`; a name Graph doesn't give from the profile lookup, for a lab account only and under the fence's
//! permit (logged to `fence.log` before it is sent); someone outside the lab never looked up by `POST` and named from
//! their messages, else from the names the binary knows; one lookup per person at a time; a failed one not tried
//! again before its wait, and tried again after it without anyone asking; and an old profile read again by age, its
//! read time kept when a part failed.
//!
//! Tokens, ids and names here are made up; the answers follow the synthetic fixtures
//! (`tests/fixtures/graph/profile.json`, `tests/fixtures/mt/short-profile.json`).

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::io::{self, Write};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Event, Parts};
use teams_core::{NameFrom, Outbox, PROFILE_MAX_AGE, PROFILE_RETRY};
use teams_fence::{FailClosed, Fence, FenceLog, Mri, Roster};
use teams_proto::{Author, Client, Credentials, HttpTransport};
use teams_store::{Conversation, ConversationKind, Message, NameSource, PictureState, Profile, Store, StoredPicture};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const GUID_A: &str = "0000000a-0000-4000-8000-00000000000a";
const GUID_B: &str = "0000000b-0000-4000-8000-00000000000b";
const GUID_OUT: &str = "0000000f-0000-4000-8000-00000000000f";
const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const MIDDLE_TIER: &str = "https://teams.microsoft.com/api/mt/part/amer-03";
const SHORT_PROFILE: &str = "/api/mt/part/amer-03/beta/users/fetchShortProfile";
const CHAT: &str = "19:synthetic-chat@thread.v2";
const PICTURE: [u8; 6] = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
/// How long a test waits for an event before it fails.
const WAIT: Duration = Duration::from_secs(30);

fn mri(guid: &str) -> String {
    format!("8:orgid:{guid}")
}

fn graph_user(guid: &str) -> String {
    format!("/v1.0/users/{guid}")
}

fn graph_picture(guid: &str) -> String {
    format!("/v1.0/users/{guid}/photos/96x96/$value")
}

fn user_answer(guid: &str, name: &str) -> ResponseTemplate {
    let body = json!({
        "@odata.context": "https://graph.microsoft.com/v1.0/$metadata#users(id,displayName)/$entity",
        "id": guid,
        "displayName": name,
    });
    ResponseTemplate::new(200).set_body_raw(body.to_string(), "application/json")
}

fn not_found() -> ResponseTemplate {
    let body = json!({"error": {"code": "ImageNotFound", "message": "Made-up text."}});
    ResponseTemplate::new(404).set_body_raw(body.to_string(), "application/json")
}

fn short_profile_answer(guid: &str, name: &str) -> ResponseTemplate {
    let body = json!({
        "type": "Microsoft.SkypeSpaces.MiddleTier.Models.IUserIdentity",
        "value": [{
            "displayName": name,
            "type": "ADUser",
            "isShortProfile": true,
            "mri": mri(guid),
            "objectId": guid,
            "userType": "Member",
        }],
    });
    ResponseTemplate::new(200).set_body_raw(body.to_string(), "application/json; charset=utf-8")
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
    server: MockServer,
    handle: EngineHandle,
    log: SharedLog,
    store_file: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

/// An engine of lab account a (the lab: a and b), its reads and its profile lookup on `server`, with `store` filled
/// in before it starts and `known` as the names the binary knows.
fn rig(server: MockServer, fill: impl FnOnce(&mut Store), known: Vec<(String, String)>) -> Rig {
    rig_with(server, fill, known, true, PROFILE_RETRY)
}

/// [`rig`], its reads' transport given the middle tier's base only when `middle_tier`, a failed lookup tried again
/// after `retry`.
fn rig_with(
    server: MockServer,
    fill: impl FnOnce(&mut Store),
    known: Vec<(String, String)>,
    middle_tier: bool,
    retry: Duration,
) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let log = SharedLog::default();
    let fence = Arc::new(Fence::new(
        Roster::new([mri(GUID_A), mri(GUID_B)].map(Mri::new), []),
        FenceLog::from_writer(log.clone()),
    ));
    let tokens = Arc::new(FakeTokens);
    let transport = |fence: &Arc<Fence>| {
        HttpTransport::with_mock_origin(Arc::clone(&tokens), Arc::clone(fence), CHAT_BASE, &server.uri())
            .unwrap()
            .with_middle_tier(MIDDLE_TIER)
            .unwrap()
    };
    let reads = if middle_tier {
        transport(&fence)
    } else {
        HttpTransport::with_mock_origin(Arc::clone(&tokens), Arc::clone(&fence), CHAT_BASE, &server.uri()).unwrap()
    };
    let client = Arc::new(Client::new(reads));
    let author = Author::new(Mri::new(mri(GUID_A)), "lab-a");
    let outbox = Outbox::new(Arc::new(transport(&fence)), fence, Arc::new(FailClosed), author);
    let store_file = dir.path().join("store").join("a.sqlite");
    let mut store = Store::open(&store_file).unwrap();
    fill(&mut store);
    let handle = engine::start(Parts {
        store,
        client,
        outbox,
        push: None,
        options: EngineOptions {
            tick: Duration::from_millis(50),
            catch_up_retry: Duration::from_millis(500),
            profile_retry: retry,
        },
        known_names: known,
    })
    .unwrap();
    Rig {
        server,
        handle,
        log,
        store_file,
        _dir: dir,
    }
}

impl Rig {
    /// Waits for `Event::Profile` of `who`, failing on any `Event::Failed`.
    async fn profile_of(&mut self, who: &str) {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let event = tokio::time::timeout_at(deadline, self.handle.next_event())
                .await
                .expect("no profile event in time")
                .expect("the engine stopped");
            match event {
                Event::Profile { mri } if mri == who => return,
                Event::Failed { during, error } => panic!("failed {during}: {error}"),
                _ => {}
            }
        }
    }

    /// Stops the engine and reads `who`'s stored profile.
    fn stored_after_shutdown(self, who: &str) -> Option<Profile> {
        self.handle.shutdown();
        Store::open(&self.store_file).unwrap().profile(who).unwrap()
    }

    async fn requests_to(&self, wanted: &str) -> Vec<Request> {
        self.server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|request| request.url.path() == wanted)
            .collect()
    }
}

fn now_ms() -> i64 {
    i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis()).unwrap()
}

#[tokio::test]
async fn a_name_and_a_picture_come_from_graph_and_are_kept_in_the_store() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(graph_user(GUID_B)))
        .respond_with(user_answer(GUID_B, "Lab Account B"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(graph_picture(GUID_B)))
        .respond_with(ResponseTemplate::new(200).set_body_raw(PICTURE.to_vec(), "image/jpeg"))
        .mount(&server)
        .await;
    let mut rig = rig(server, |_| {}, Vec::new());

    // nothing stored yet: no name, and a lookup starts
    let first = rig.handle.person(mri(GUID_B)).await.unwrap();
    assert_eq!((first.name, first.from, first.picture), (None, None, None));
    rig.profile_of(&mri(GUID_B)).await;
    let person = rig.handle.person(mri(GUID_B)).await.unwrap();
    assert_eq!(person.name.as_deref(), Some("Lab Account B"));
    assert_eq!(person.from, Some(NameFrom::Graph));
    let picture = person.picture.expect("a picture");
    assert_eq!(
        (picture.media_type.as_str(), picture.bytes.as_slice()),
        ("image/jpeg", &PICTURE[..])
    );
    assert!(!format!("{:?}", rig.handle.person(mri(GUID_B)).await.unwrap()).contains("Lab Account B"));

    // fresh in the store: asked again, nothing more is read; Graph named them, so nothing went by POST
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(rig.requests_to(&graph_user(GUID_B)).await.len(), 1);
    assert_eq!(rig.requests_to(&graph_picture(GUID_B)).await.len(), 1);
    assert!(rig.requests_to(SHORT_PROFILE).await.is_empty());
    for request in rig.server.received_requests().await.unwrap() {
        assert_eq!(request.method.as_str(), "GET");
        let bearer = request.headers.get("authorization").unwrap().to_str().unwrap();
        assert_eq!(bearer, "Bearer placeholder-token-for-https://graph.microsoft.com");
    }
    rig.handle.shutdown();
}

#[tokio::test]
async fn a_name_graph_doesnt_give_comes_from_the_profile_lookup_for_a_lab_account_under_the_fence() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(graph_user(GUID_B)))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(graph_picture(GUID_B)))
        .respond_with(not_found())
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(SHORT_PROFILE))
        .respond_with(short_profile_answer(GUID_B, "Lab Account B"))
        .mount(&server)
        .await;
    let mut rig = rig(server, |_| {}, Vec::new());
    let _ = rig.handle.person(mri(GUID_B)).await.unwrap();
    rig.profile_of(&mri(GUID_B)).await;
    let person = rig.handle.person(mri(GUID_B)).await.unwrap();
    assert_eq!(person.name.as_deref(), Some("Lab Account B"));
    assert_eq!(person.from, Some(NameFrom::ShortProfile));
    assert_eq!(person.picture, None, "no picture: initials");

    let lookups = rig.requests_to(SHORT_PROFILE).await;
    assert_eq!(lookups.len(), 1);
    let body: Value = serde_json::from_slice(&lookups[0].body).unwrap();
    assert_eq!(body, json!([mri(GUID_B)]));
    // the fence's decision, then the request, each logged before it went
    let lines = rig.log.lines();
    let kinds: Vec<(&str, &str)> = lines
        .iter()
        .map(|line| {
            (
                line["event"].as_str().or(line["channel"].as_str()).unwrap_or(""),
                line["verdict"].as_str().or(line["outcome"].as_str()).unwrap_or(""),
            )
        })
        .collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(kinds[0], ("post_read_decision", "allow"), "{lines:?}");
    assert_eq!(lines[1]["post_read"], "short_profile", "{lines:?}");
    rig.handle.shutdown();
}

#[tokio::test]
async fn someone_outside_the_lab_is_never_looked_up_by_post_and_is_named_from_their_messages() {
    let server = MockServer::start().await;
    for guid in [GUID_OUT, GUID_B] {
        Mock::given(method("GET"))
            .and(path(graph_user(guid)))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(graph_picture(guid)))
            .respond_with(not_found())
            .mount(&server)
            .await;
    }
    Mock::given(method("POST"))
        .and(path(SHORT_PROFILE))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let fill = |store: &mut Store| {
        store
            .upsert_conversation(&Conversation {
                id: CHAT.to_owned(),
                kind: ConversationKind::Group,
                topic: None,
                version: 1,
                last_activity: None,
            })
            .unwrap();
        let message = Message {
            conversation_id: CHAT.to_owned(),
            id: "1000".to_owned(),
            version: 1000,
            client_message_id: None,
            sender: Some(mri(GUID_OUT)),
            sender_name: Some("Someone From Their Message".to_owned()),
            message_type: "RichText/Html".to_owned(),
            content: "<p>made up</p>".to_owned(),
            properties: None,
            arrival_time: 1000,
            compose_time: Some(1000),
            edit_time: None,
            delete_time: None,
        };
        store.upsert_message(&message, None).unwrap();
    };
    let known = vec![(mri(GUID_B), "lab-b".to_owned())];
    let mut rig = rig(server, fill, known);

    let outsider = rig.handle.person(mri(GUID_OUT)).await.unwrap();
    assert_eq!(outsider.name.as_deref(), Some("Someone From Their Message"));
    assert_eq!(outsider.from, Some(NameFrom::Message));
    rig.profile_of(&mri(GUID_OUT)).await;
    let outsider = rig.handle.person(mri(GUID_OUT)).await.unwrap();
    assert_eq!(outsider.from, Some(NameFrom::Message), "Graph and the lookup gave none");
    assert!(
        rig.requests_to(SHORT_PROFILE).await.is_empty(),
        "denied before anything went"
    );
    let decisions: Vec<Value> = rig.log.lines();
    assert_eq!(decisions.len(), 1, "{decisions:?}");
    assert_eq!(decisions[0]["event"], "post_read_decision");
    assert_eq!(decisions[0]["verdict"], "deny");
    assert!(!decisions[0].to_string().contains(GUID_OUT), "the log names no MRI");

    // a lab account the lookup fails for (500) is named by the name the binary knows, and tried again later
    let _ = rig.handle.person(mri(GUID_B)).await.unwrap();
    rig.profile_of(&mri(GUID_B)).await;
    let b = rig.handle.person(mri(GUID_B)).await.unwrap();
    assert_eq!((b.name.as_deref(), b.from), (Some("lab-b"), Some(NameFrom::Known)));
    rig.handle.shutdown();
}

#[tokio::test]
async fn without_the_middle_tier_s_base_the_fence_isn_t_asked_for_a_lookup() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(graph_user(GUID_B)))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(graph_picture(GUID_B)))
        .respond_with(not_found())
        .mount(&server)
        .await;
    let known = vec![(mri(GUID_B), "lab-b".to_owned())];
    let mut rig = rig_with(server, |_| {}, known, false, PROFILE_RETRY);
    let _ = rig.handle.person(mri(GUID_B)).await.unwrap();
    rig.profile_of(&mri(GUID_B)).await;
    let b = rig.handle.person(mri(GUID_B)).await.unwrap();
    assert_eq!((b.name.as_deref(), b.from), (Some("lab-b"), Some(NameFrom::Known)));
    assert!(rig.log.lines().is_empty(), "no decision for a lookup that can't go out");
    assert!(rig.requests_to(SHORT_PROFILE).await.is_empty());
    rig.handle.shutdown();
}

#[tokio::test]
async fn a_lookup_isnt_repeated_while_one_is_under_way_nor_soon_after_one_failed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(graph_user(GUID_A)))
        .respond_with(ResponseTemplate::new(500).set_delay(Duration::from_millis(400)))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(graph_picture(GUID_A)))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(SHORT_PROFILE))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let mut rig = rig(server, |_| {}, Vec::new());
    for _ in 0..5 {
        let _ = rig.handle.person(mri(GUID_A)).await.unwrap();
    }
    rig.profile_of(&mri(GUID_A)).await;
    assert_eq!(
        rig.requests_to(&graph_user(GUID_A)).await.len(),
        1,
        "one lookup while it ran"
    );
    assert_eq!(rig.requests_to(SHORT_PROFILE).await.len(), 1);

    // it failed on the way: not read again before its wait, and nothing it had is lost (it had nothing)
    for _ in 0..3 {
        let person = rig.handle.person(mri(GUID_A)).await.unwrap();
        assert_eq!((person.name, person.picture), (None, None));
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(rig.requests_to(&graph_user(GUID_A)).await.len(), 1);
    rig.handle.shutdown();
}

#[tokio::test]
async fn an_old_profile_is_shown_and_read_again_and_a_failed_part_keeps_the_last_one() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(graph_user(GUID_B)))
        .respond_with(user_answer(GUID_B, "Lab Account B Renamed"))
        .mount(&server)
        .await;
    // the picture's read fails on the way: the stored one stays
    Mock::given(method("GET"))
        .and(path(graph_picture(GUID_B)))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let day = i64::try_from(PROFILE_MAX_AGE.as_millis()).unwrap();
    let old = Profile {
        mri: mri(GUID_B),
        name: Some("Lab Account B".to_owned()),
        name_source: Some(NameSource::Graph),
        picture: PictureState::Some(StoredPicture {
            media_type: "image/png".to_owned(),
            bytes: vec![0x89, b'P', b'N', b'G'],
        }),
        read_at: now_ms() - day - 60_000,
    };
    let stored = old.clone();
    let mut rig = rig(server, move |store| store.put_profile(&stored).unwrap(), Vec::new());

    let shown = rig.handle.person(mri(GUID_B)).await.unwrap();
    assert_eq!(
        shown.name.as_deref(),
        Some("Lab Account B"),
        "the old one shown while it is read again"
    );
    rig.profile_of(&mri(GUID_B)).await;
    let person = rig.handle.person(mri(GUID_B)).await.unwrap();
    assert_eq!(person.name.as_deref(), Some("Lab Account B Renamed"));
    assert_eq!(
        person.picture.map(|picture| picture.media_type),
        Some("image/png".to_owned())
    );
    // a part failed: the read time stays the old one, so the person is due again after the retry wait, not a day
    let stored = rig.stored_after_shutdown(&mri(GUID_B)).expect("b's profile");
    assert_eq!(stored.read_at, old.read_at);
    assert_eq!(stored.name.as_deref(), Some("Lab Account B Renamed"));
}

#[tokio::test]
async fn a_failed_lookup_is_tried_again_after_its_wait_without_anyone_asking_again() {
    let server = MockServer::start().await;
    // Graph's read fails on the way once, then answers
    Mock::given(method("GET"))
        .and(path(graph_user(GUID_B)))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(graph_user(GUID_B)))
        .respond_with(user_answer(GUID_B, "Lab Account B"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(graph_picture(GUID_B)))
        .respond_with(not_found())
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(SHORT_PROFILE))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let mut rig = rig_with(server, |_| {}, Vec::new(), true, Duration::from_millis(300));

    // asked once: the lookup fails, and what it left (nothing settled) is told
    let _ = rig.handle.person(mri(GUID_B)).await.unwrap();
    rig.profile_of(&mri(GUID_B)).await;
    assert_eq!(rig.requests_to(&graph_user(GUID_B)).await.len(), 1);
    // nobody asks again: the engine looks them up once the wait is over, and tells the name it found
    rig.profile_of(&mri(GUID_B)).await;
    assert_eq!(rig.requests_to(&graph_user(GUID_B)).await.len(), 2);
    let person = rig.handle.person(mri(GUID_B)).await.unwrap();
    assert_eq!(
        (person.name.as_deref(), person.from),
        (Some("Lab Account B"), Some(NameFrom::Graph))
    );
    // settled now: not looked up again
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(rig.requests_to(&graph_user(GUID_B)).await.len(), 2);
    rig.handle.shutdown();
}
