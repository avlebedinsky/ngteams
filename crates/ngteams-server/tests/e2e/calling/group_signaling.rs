//! A group call's signaling, with the client's call machine at every party and the call's media node a fixed one
//! ([`super::mixer::FixedMixer`]): the start in a group chat ringing its other members with the node's offers, the
//! caller accepted by the node at once, a callee's answer, a decline and a ring nobody answers, leaves and the last
//! one, a member joining the running call it finds in the chat, a second start refused (also when both come at once),
//! adds (a callee added back rings anew), a media node that fails a join, an answer a join of the same callee overtook, and one that comes while such a join
//! is under way.

use std::time::Duration;

use serde_json::{Value, json};
use teams_call::{CallState, End, Participant};
use teams_proto::{CallLink, Cloud, Endpoint, Method, Mri, Request, Service};

use super::client::Client;
use super::mixer::{FixedMixer, answer_on, offer_to};
use crate::support::{Backend, TestServer, backends};
use crate::trouter::{Account, ORIGIN};

/// Members of the chat who aren't in a test's calls: a big group, some odd names among them.
const QUIET_MEMBERS: [&str; 6] = [
    "Ödön Ðorđević-Łukasiewicz",
    "O'Brien & Sons <QA>",
    "李小龍",
    "Zoë \"Zed\" Ångström",
    "  Spaced  Out  ",
    "x",
];

fn mri(id: &str) -> String {
    format!("8:orgid:{id}")
}

/// A member's removal from a group chat, as the chat service takes it.
struct RemoveMember {
    chat: String,
    member: String,
}

impl Request for RemoveMember {
    fn service(&self) -> Service {
        Service::ChatService
    }
    fn method(&self) -> Method {
        Method::Delete
    }
    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["v1", "threads", &self.chat, "members", &self.member])
    }
    fn body(&self) -> Option<Value> {
        None
    }
    fn is_write(&self) -> bool {
        true
    }
}

/// `writer`'s removal of `member` from the group chat `chat`.
async fn remove_member(writer: &teams_proto::Client<crate::trouter::Transport>, chat: &str, member: &Person) {
    let removed = writer
        .write(RemoveMember {
            chat: chat.to_owned(),
            member: member.mri(),
        })
        .await
        .unwrap();
    assert!(removed.is_success(), "{}", removed.status());
}

/// A person of a test: the account and its calling client.
pub struct Person {
    pub id: String,
    pub upn: String,
    pub client: Client,
}

impl Person {
    fn mri(&self) -> String {
        mri(&self.id)
    }
}

/// A server with a fixed media node and a group chat: Ada made it with Bert, Cora, a member with no calling client,
/// Eve (who left it again) and quiet members; Oscar is in no chat with them. Everyone named has a calling client but
/// the member without one.
pub struct Group {
    pub server: TestServer,
    pub mixer: FixedMixer,
    pub chat: String,
    pub a: Person,
    pub b: Person,
    pub c: Person,
    pub e: Person,
    pub o: Person,
    /// Every member but Ada, as her client calls them all.
    pub others: Vec<String>,
}

impl Group {
    pub async fn start(backend: Backend) -> Self {
        Self::start_with(backend, |_| {}).await
    }

    pub async fn start_with(backend: Backend, configure: impl FnOnce(&mut ngteams_server::config::Config)) -> Self {
        let server = crate::trouter::server_with(backend, |config| {
            if backend == Backend::Postgres {
                config.redis_url = crate::support::redis_url();
            }
            configure(config);
        })
        .await;
        let mixer = FixedMixer::default();
        assert!(server.app.mixers.install(std::sync::Arc::new(mixer.clone())));
        // names of this test's own: with Redis, sign-ins are counted per name across every test running now
        let tag = uuid::Uuid::new_v4().simple().to_string();
        let person = async |name: &str| {
            let upn = format!("{}-{tag}@own.test", name.to_lowercase());
            let id = server.add_user(&upn, name).await;
            let client = super::client(&server, &server, &upn, &id, name).await;
            Person { id, upn, client }
        };
        let ada = person("Ada").await;
        let bert = person("Bert").await;
        let cora = person("Cora").await;
        let eve = person("Eve").await;
        let oscar = person("Oscar").await;
        let mut members = vec![bert.mri(), cora.mri(), eve.mri()];
        let silent = server.add_user(&format!("silent-{tag}@own.test"), "Silent Sam").await;
        members.push(mri(&silent));
        for (n, name) in QUIET_MEMBERS.iter().enumerate() {
            members.push(mri(&server.add_user(&format!("quiet{n}-{tag}@own.test"), name).await));
        }
        let account = Account::sign_in(&server, &ada.upn, &ada.id).await;
        let writer = account.client(&server);
        let create = teams_proto::CreateChat::group(Mri::new(ada.mri()), members.iter().map(Mri::new)).unwrap();
        let created = writer.write(create).await.unwrap();
        let body: Value = serde_json::from_slice(created.body()).unwrap();
        let chat = body["threadResource"]["id"].as_str().unwrap().to_owned();
        remove_member(&writer, &chat, &eve).await;
        Self {
            server,
            mixer,
            chat,
            a: ada,
            b: bert,
            c: cora,
            e: eve,
            o: oscar,
            others: members,
        }
    }

    /// The one call that isn't over.
    async fn open_call(&self) -> ngteams_server::calling::OpenCall {
        let calls = ngteams_server::calling::open_calls(&self.server.app.db).await.unwrap();
        let [call] = calls.as_slice() else {
            panic!("one call: {calls:?}");
        };
        call.clone()
    }

    /// The chat's `Event/Call` messages, the newest first, as `person` reads them: their types and contents.
    async fn call_events(&self, person: &Person) -> Vec<(String, String)> {
        let account = Account::sign_in(&self.server, &person.upn, &person.id).await;
        let reader = account.client(&self.server);
        let page = teams_proto::chat::ChatReads::new(&reader)
            .messages(&self.chat, 50)
            .await
            .unwrap();
        page.messages
            .iter()
            .filter(|message| message.message_type.as_str() == "Event/Call")
            .map(|message| (message.message_type.as_str().to_owned(), message.content.clone()))
            .collect()
    }

    /// The running call the chat's events name, as `person`'s client finds it.
    async fn running_call(&self, person: &Person) -> Option<CallLink> {
        let events = self.call_events(person).await;
        teams_proto::running_call_in(
            &Cloud::own(ORIGIN).unwrap(),
            events.iter().map(|(kind, content)| (kind.as_str(), content.as_str())),
        )
    }
}

/// Whether `participants` are exactly `mris`, in any order.
fn are(participants: &[Participant], mris: &[String]) -> bool {
    let mut seen: Vec<&str> = participants
        .iter()
        .map(|participant| participant.mri.as_str())
        .collect();
    let mut wanted: Vec<&str> = mris.iter().map(String::as_str).collect();
    seen.sort_unstable();
    wanted.sort_unstable();
    seen == wanted
}

#[tokio::test(flavor = "multi_thread")]
async fn a_group_call_rings_every_other_member_with_the_mixers_offer() {
    for backend in backends() {
        let mut group = Group::start(backend).await;
        let mut callees = group.others.clone();
        callees.push(group.o.mri());
        group.a.client.call_group(&group.chat, &callees);
        let b = group.b.client.incoming().await;
        let c = group.c.client.incoming().await;
        for (rings, person) in [(&b, &group.b), (&c, &group.c)] {
            assert_eq!(rings.caller.as_str(), group.a.mri(), "{}", person.upn);
            assert_eq!(rings.caller_name.as_deref(), Some("Ada"));
            assert_eq!(rings.group_chat.as_deref(), Some(group.chat.as_str()));
        }
        let call = group.open_call().await;
        let legs = group.mixer.legs(&call.id);
        // a leg for Ada, Bert, Cora, the member with no calling client and every quiet member; none for Eve, who
        // left the chat, nor for Oscar, who was never in it
        assert_eq!(legs.len(), 3 + 1 + QUIET_MEMBERS.len(), "{legs:?}");
        let offers = [b.offer.unwrap().blob, c.offer.unwrap().blob];
        for offer in &offers {
            assert!(
                legs.keys().any(|leg| *offer == offer_to(leg)),
                "the node's offer to the callee's own leg: {offer}"
            );
        }
        assert_ne!(offers[0], offers[1]);
        let quiet = group.e.client.events_within(Duration::from_millis(500)).await;
        assert!(quiet.is_empty(), "Eve left the chat: {quiet:#?}");
        let quiet = group.o.client.events_within(Duration::from_millis(100)).await;
        assert!(quiet.is_empty(), "Oscar is no member: {quiet:#?}");
        group.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_caller_is_accepted_at_once_and_a_callees_answer_puts_it_in_the_roster() {
    for backend in backends() {
        let mut group = Group::start(backend).await;
        group.a.client.call_group(&group.chat, &group.others);
        // up at once with the node's answer, while everyone still rings
        let answer = group.a.client.remote_media().await;
        group.a.client.state(CallState::Connected).await;
        let call = group.open_call().await;
        assert!(
            group
                .mixer
                .legs(&call.id)
                .keys()
                .any(|leg| answer.blob == answer_on(leg, 0)),
            "{answer:?}"
        );
        let rings = group.b.client.incoming().await;
        group.c.client.incoming().await;
        group.b.client.answer(rings.call);
        group.b.client.state(CallState::Connected).await;
        let (a, b) = (group.a.mri(), group.b.mri());
        let told = group
            .a
            .client
            .participants_until(|now| are(now, std::slice::from_ref(&b)))
            .await;
        let bert = &told[0];
        assert_eq!(bert.name.as_deref(), Some("Bert"));
        let leg = group.mixer.leg_of(&call.id, bert.audio_source.unwrap()).unwrap();
        assert!(
            group.mixer.legs(&call.id)[&leg].answered,
            "Bert's answer reached his leg"
        );
        group
            .b
            .client
            .participants_until(|now| are(now, std::slice::from_ref(&a)))
            .await;
        // Cora still rings, told the roster at her ring's subscription
        let quiet = group.c.client.events_within(Duration::from_millis(300)).await;
        assert!(
            quiet
                .iter()
                .all(|event| matches!(event, teams_call::CallEvent::Participants { .. })),
            "{quiet:#?}"
        );
        group.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_decline_or_an_unanswered_ring_leaves_the_call_running_for_the_others() {
    for backend in backends() {
        let mut group = Group::start_with(backend, |config| config.calling.ring_timeout = 3).await;
        group.a.client.call_group(&group.chat, &group.others);
        group.a.client.state(CallState::Connected).await;
        let call = group.open_call().await;
        let rings = group.b.client.incoming().await;
        group.c.client.incoming().await;
        group.b.client.decline(rings.call);
        assert_eq!(group.b.client.ended().await, End::Declined);
        let end = group.c.client.ring_ended().await;
        assert_eq!(end.code, Some(487), "{end:?}");
        // the ring ended for everyone who didn't answer: only Ada's leg is left, and her call goes on
        let legs = group.mixer.legs(&call.id);
        assert_eq!(legs.len(), 1, "{legs:?}");
        let quiet = group.a.client.events_within(Duration::from_millis(500)).await;
        assert!(
            !quiet
                .iter()
                .any(|event| matches!(event, teams_call::CallEvent::Ended { .. })),
            "{quiet:#?}"
        );
        assert_eq!(group.open_call().await.phase, "connected");
        group.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_leave_takes_one_party_out_and_the_last_leave_ends_the_call_in_the_chat() {
    for backend in backends() {
        let mut group = Group::start(backend).await;
        let (a, b, c) = (group.a.mri(), group.b.mri(), group.c.mri());
        group.a.client.call_group(&group.chat, &group.others);
        let placed = group.a.client.state(CallState::Connected).await;
        let call = group.open_call().await;
        let b_rings = group.b.client.incoming().await;
        let c_rings = group.c.client.incoming().await;
        group.b.client.answer(b_rings.call);
        group.c.client.answer(c_rings.call);
        let told = group
            .a
            .client
            .participants_until(|now| are(now, &[b.clone(), c.clone()]))
            .await;
        let bert = told.iter().find(|participant| participant.mri.as_str() == b).unwrap();
        let bert = group.mixer.leg_of(&call.id, bert.audio_source.unwrap()).unwrap();
        // Bert leaves: the others go on without him, and his leg goes
        group.b.client.hang_up(b_rings.call);
        assert_eq!(group.b.client.ended().await, End::HungUp);
        group
            .a
            .client
            .participants_until(|now| are(now, std::slice::from_ref(&c)))
            .await;
        group
            .c
            .client
            .participants_until(|now| are(now, std::slice::from_ref(&a)))
            .await;
        assert!(!group.mixer.legs(&call.id).contains_key(&bert));
        // the caller leaves: Cora is still in it
        group.a.client.hang_up(placed);
        assert_eq!(group.a.client.ended().await, End::HungUp);
        group.c.client.participants_until(<[Participant]>::is_empty).await;
        assert!(!group.mixer.dropped(&call.id));
        // the last one leaves: the call is over, its room dropped, its end in the chat
        group.c.client.hang_up(c_rings.call);
        assert_eq!(group.c.client.ended().await, End::HungUp);
        tokio::time::timeout(super::client::WAIT, async {
            while !group.mixer.dropped(&call.id) {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
        let events = group.call_events(&group.b).await;
        assert_eq!(events.len(), 2, "{events:?}");
        let (ended, started) = (&events[0].1, &events[1].1);
        assert!(
            started.contains("<callEventType>callStarted</callEventType>"),
            "{started}"
        );
        assert!(started.contains(&format!("<callId>{}</callId>", call.id)), "{started}");
        assert!(ended.contains("<callEventType>callEnded</callEventType>"), "{ended}");
        for party in [&a, &b, &c] {
            assert!(ended.contains(&format!("<name>{party}</name>")), "{ended}");
        }
        assert!(
            group.running_call(&group.b).await.is_none(),
            "nothing runs in the chat any more"
        );
        group.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_member_joins_the_running_call_it_finds_in_the_chats_events() {
    for backend in backends() {
        let mut group = Group::start(backend).await;
        let (a, b, c) = (group.a.mri(), group.b.mri(), group.c.mri());
        // Ada rings Bert only: Cora isn't rung, and finds the call in the chat
        group.a.client.call_group(&group.chat, std::slice::from_ref(&b));
        group.a.client.state(CallState::Connected).await;
        let rings = group.b.client.incoming().await;
        group.b.client.answer(rings.call);
        group.b.client.state(CallState::Connected).await;
        let running = group
            .running_call(&group.c)
            .await
            .expect("the chat names its running call");
        let call = group.open_call().await;
        assert!(running.as_str().contains(&call.conversation), "{running:?}");
        group.c.client.join(&group.chat, running.clone());
        let answer = group.c.client.remote_media().await;
        group.c.client.state(CallState::Connected).await;
        assert!(
            group
                .mixer
                .legs(&call.id)
                .keys()
                .any(|leg| answer.blob == answer_on(leg, 0)),
            "{answer:?}"
        );
        // the joiner is told everyone, and everyone is told the joiner
        group
            .c
            .client
            .participants_until(|now| are(now, &[a.clone(), b.clone()]))
            .await;
        group
            .a
            .client
            .participants_until(|now| are(now, &[b.clone(), c.clone()]))
            .await;
        group
            .b
            .client
            .participants_until(|now| are(now, &[a.clone(), c.clone()]))
            .await;
        // someone in no chat with them joins nothing
        group.o.client.join(&group.chat, running);
        let end = group.o.client.ended().await;
        assert_eq!(end, End::Failed("join"), "{end:?}");
        assert_eq!(group.mixer.legs(&call.id).len(), 3);
        group.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_start_in_a_chat_with_a_running_call_is_refused_naming_it() {
    for backend in backends() {
        let mut group = Group::start(backend).await;
        let b = group.b.mri();
        group.a.client.call_group(&group.chat, std::slice::from_ref(&b));
        group.a.client.state(CallState::Connected).await;
        group.b.client.incoming().await;
        let call = group.open_call().await;
        let mut callees = group.others.clone();
        callees.retain(|callee| *callee != group.c.mri());
        callees.push(group.a.mri());
        group.c.client.call_group(&group.chat, &callees);
        let End::Remote(end) = group.c.client.ended().await else {
            panic!("the start ends as the service's");
        };
        assert!(end.chat_has_running_call(), "{end:?}");
        let named = end.conversation.unwrap_or_default();
        assert!(named.ends_with(&format!("/conv/{}", call.conversation)), "{named}");
        // the running call is untouched
        assert_eq!(group.open_call().await.id, call.id);
        let quiet = group.b.client.events_within(Duration::from_millis(300)).await;
        assert!(quiet.is_empty(), "{quiet:#?}");
        group.server.stop().await;
    }
}

/// A raw calling request with `person`'s skypetoken.
async fn raw(server: &TestServer, person: &Person, method: reqwest::Method, path: &str, body: &Value) -> (u16, Value) {
    raw_as(server, &person.upn, method, path, body).await
}

/// A raw calling request with the skypetoken of the account `upn`.
async fn raw_as(server: &TestServer, upn: &str, method: reqwest::Method, path: &str, body: &Value) -> (u16, Value) {
    let session = server.session(upn).await;
    let token = session.skypetoken().await.unwrap();
    let response = reqwest::Client::new()
        .request(method, server.url(path))
        .header("x-skypetoken", token.token.expose())
        .json(body)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

/// The calling endpoint of the account `id` and a callback base on it.
pub(super) async fn own_callbacks(server: &TestServer, id: &str) -> (String, String) {
    let stack = ngteams_server::trouter::Stack::Calling;
    let registered = ngteams_server::registrar::registered(&server.app.db, id, stack.app_id())
        .await
        .unwrap();
    let endpoint = ngteams_server::trouter::endpoint_id(id, &registered[0], stack);
    let base = format!("{ORIGIN}/trouter/v4/f/{endpoint}/callAgent/raw/");
    (endpoint, base)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_group_start_is_not_served_without_a_media_node_nor_for_someone_outside_the_chat() {
    let server = crate::trouter::server(Backend::Sqlite).await;
    let a_id = server.add_user("a@own.test", "Ada").await;
    let b_id = server.add_user("b@own.test", "Bert").await;
    let c_id = server.add_user("c@own.test", "Cora").await;
    let mut a = super::client(&server, &server, "a@own.test", &a_id, "Ada").await;
    let account = Account::sign_in(&server, "a@own.test", &a_id).await;
    let others = [Mri::new(mri(&b_id)), Mri::new(mri(&c_id))];
    let create = teams_proto::CreateChat::group(Mri::new(mri(&a_id)), others).unwrap();
    let created = account.client(&server).write(create).await.unwrap();
    let body: Value = serde_json::from_slice(created.body()).unwrap();
    let chat = body["threadResource"]["id"].as_str().unwrap().to_owned();
    a.call_group(&chat, &[mri(&b_id)]);
    assert_eq!(a.ended().await, End::Failed("start"), "no media node: 501");
    server.stop().await;
    // with one, someone outside the chat starts nothing in it, and a chat that doesn't exist has no call
    let mut group = Group::start(Backend::Sqlite).await;
    let chat = group.chat.clone();
    group.o.client.call_group(&chat, std::slice::from_ref(&group.others[0]));
    assert_eq!(group.o.client.ended().await, End::Failed("start"));
    group
        .o
        .client
        .call_group("19:nothing@thread.v2", std::slice::from_ref(&group.others[0]));
    assert_eq!(group.o.client.ended().await, End::Failed("start"));
    assert!(
        ngteams_server::calling::open_calls(&group.server.app.db)
            .await
            .unwrap()
            .is_empty()
    );
    group.server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_party_adds_a_member_who_rings_and_one_with_nothing_to_ring_isnt_brought_in() {
    for backend in backends() {
        let mut group = Group::start(backend).await;
        let (a, b, c) = (group.a.mri(), group.b.mri(), group.c.mri());
        group.a.client.call_group(&group.chat, std::slice::from_ref(&b));
        let placed = group.a.client.state(CallState::Connected).await;
        let rings = group.b.client.incoming().await;
        group.b.client.answer(rings.call);
        group.b.client.state(CallState::Connected).await;
        group.a.client.add(placed, &c, &group.chat);
        let added = group.c.client.incoming().await;
        assert_eq!(added.group_chat.as_deref(), Some(group.chat.as_str()));
        let call = group.open_call().await;
        let offer = added.offer.unwrap().blob;
        assert!(
            group.mixer.legs(&call.id).keys().any(|leg| offer == offer_to(leg)),
            "{offer}"
        );
        group.c.client.answer(added.call);
        group.c.client.state(CallState::Connected).await;
        group
            .a
            .client
            .participants_until(|now| are(now, &[b.clone(), c.clone()]))
            .await;
        group
            .c
            .client
            .participants_until(|now| are(now, &[a.clone(), b.clone()]))
            .await;
        // the member with no calling client has nothing to ring
        let silent = group.others[3].clone();
        group.a.client.add(placed, &silent, &group.chat);
        let who = group
            .a
            .client
            .next(|event| match event {
                teams_call::CallEvent::AddFailed { who, .. } => Some(who),
                _ => None,
            })
            .await;
        assert_eq!(who.as_str(), silent);
        // someone in no chat with them isn't added: the add isn't taken
        group.a.client.add(placed, &group.o.mri(), &group.chat);
        let who = group
            .a
            .client
            .next(|event| match event {
                teams_call::CallEvent::AddFailed { who, .. } => Some(who),
                _ => None,
            })
            .await;
        assert_eq!(who.as_str(), group.o.mri());
        assert_eq!(group.mixer.legs(&call.id).len(), 3, "Ada's, Bert's and Cora's");
        group.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_partys_renegotiation_is_answered_by_the_media_node_and_a_hold_is_refused() {
    let mut group = Group::start(Backend::Sqlite).await;
    let b = group.b.mri();
    group.a.client.call_group(&group.chat, std::slice::from_ref(&b));
    group.a.client.remote_media().await;
    let placed = group.a.client.state(CallState::Connected).await;
    group.b.client.incoming().await;
    let call = group.open_call().await;
    group.a.client.renegotiate(placed);
    let answer = group.a.client.remote_media().await;
    assert!(
        group
            .mixer
            .legs(&call.id)
            .keys()
            .any(|leg| answer.blob == answer_on(leg, 1)),
        "the node's answer to the renegotiation: {answer:?}"
    );
    // acknowledged, the next one is answered too
    tokio::time::sleep(Duration::from_millis(300)).await;
    group.a.client.renegotiate(placed);
    let answer = group.a.client.remote_media().await;
    assert!(
        group
            .mixer
            .legs(&call.id)
            .keys()
            .any(|leg| answer.blob == answer_on(leg, 2)),
        "{answer:?}"
    );
    // Bert, still ringing, hears nothing of it
    let quiet = group.b.client.events_within(Duration::from_millis(300)).await;
    assert!(
        quiet
            .iter()
            .all(|event| matches!(event, teams_call::CallEvent::Participants { .. })),
        "{quiet:#?}"
    );
    // a group call has no hold
    let (_, base) = own_callbacks(&group.server, &group.a.id).await;
    let hold = json!({"hold": {"links": {"holdCompletion": format!("{base}holdCompletion/")}}});
    let path = format!("/flightproxy/api/v2/ep/cc.own/cc/v1/active/{}/0/a1/1/hold", call.id);
    let (status, _) = raw(&group.server, &group.a, reqwest::Method::POST, &path, &hold).await;
    assert_eq!(status, 403);
    group.server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_member_follows_the_roster_without_joining_and_nobody_acts_on_anothers_seat() {
    let mut group = Group::start(Backend::Sqlite).await;
    let (a, b) = (group.a.mri(), group.b.mri());
    group.a.client.call_group(&group.chat, std::slice::from_ref(&b));
    group.a.client.state(CallState::Connected).await;
    let rings = group.b.client.incoming().await;
    let call = group.open_call().await;
    let conv = format!("/flightproxy/api/v2/ep/conv.own/conv/{}", call.conversation);
    // Cora, a member, subscribes without media: the whole roster in the answer and at her callback
    let (endpoint, base) = own_callbacks(&group.server, &group.c.id).await;
    let mut told = group
        .server
        .app
        .fanout
        .subscribe(&ngteams_server::calling::topic(&endpoint))
        .await
        .unwrap();
    let watch =
        json!({"conversationRequest": {"roster": {"type": "Delta", "rosterUpdate": format!("{base}rosterUpdate/")}}});
    let (status, answer) = raw(&group.server, &group.c, reqwest::Method::POST, &conv, &watch).await;
    assert_eq!(status, 200);
    assert!(answer["roster"]["participants"].get(&a).is_some(), "{answer}");
    let first = tokio::time::timeout(super::client::WAIT, told.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&first).contains(&a));
    // Bert answers: the watcher is told
    group.b.client.answer(rings.call);
    group.b.client.state(CallState::Connected).await;
    let next = tokio::time::timeout(super::client::WAIT, told.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&next).contains(&b));
    // someone in no chat with them learns nothing of the call
    let (status, _) = raw(&group.server, &group.o, reqwest::Method::POST, &conv, &watch).await;
    assert_eq!(status, 404);
    // a party's links name its own seat: another's is none of its business
    let cc = format!("/flightproxy/api/v2/ep/cc.own/cc/v1/active/{}", call.id);
    let (status, _) = raw(
        &group.server,
        &group.a,
        reqwest::Method::POST,
        &format!("{cc}/1/a1/1"),
        &Value::Null,
    )
    .await;
    assert_eq!(status, 404, "Ada's keep-alive on Bert's seat");
    let (status, _) = raw(
        &group.server,
        &group.a,
        reqwest::Method::POST,
        &format!("{cc}/0/a1/1"),
        &Value::Null,
    )
    .await;
    assert_eq!(status, 202, "her own");
    let (status, _) = raw(
        &group.server,
        &group.b,
        reqwest::Method::POST,
        &format!("{cc}/0/a1/1/updateMediaDescriptions"),
        &json!({}),
    )
    .await;
    assert_eq!(status, 404);
    group.server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_callee_who_declined_joins_later_and_its_own_leave_takes_it_out() {
    for backend in backends() {
        let mut group = Group::start(backend).await;
        let b = group.b.mri();
        group.a.client.call_group(&group.chat, std::slice::from_ref(&b));
        group.a.client.state(CallState::Connected).await;
        let rings = group.b.client.incoming().await;
        group.b.client.decline(rings.call);
        assert_eq!(group.b.client.ended().await, End::Declined);
        let running = group
            .running_call(&group.b)
            .await
            .expect("the chat names its running call");
        group.b.client.join(&group.chat, running);
        group.b.client.remote_media().await;
        let joined = group.b.client.state(CallState::Connected).await;
        group
            .a
            .client
            .participants_until(|now| are(now, std::slice::from_ref(&b)))
            .await;
        group.b.client.hang_up(joined);
        assert_eq!(group.b.client.ended().await, End::HungUp);
        group.a.client.participants_until(<[Participant]>::is_empty).await;
        assert_eq!(group.open_call().await.phase, "connected", "Ada is still in it");
        group.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_caller_who_left_rings_and_answers_when_added_back() {
    for backend in backends() {
        let mut group = Group::start(backend).await;
        let (a, b) = (group.a.mri(), group.b.mri());
        group.a.client.call_group(&group.chat, std::slice::from_ref(&b));
        let placed = group.a.client.state(CallState::Connected).await;
        let rings = group.b.client.incoming().await;
        group.b.client.answer(rings.call);
        let answered = group.b.client.state(CallState::Connected).await;
        group
            .a
            .client
            .participants_until(|now| are(now, std::slice::from_ref(&b)))
            .await;
        group.a.client.hang_up(placed);
        assert_eq!(group.a.client.ended().await, End::HungUp);
        group.b.client.participants_until(<[Participant]>::is_empty).await;
        // Bert brings Ada back: hers is the caller's seat, and she rings and answers as anyone added does
        group.b.client.add(answered, &a, &group.chat);
        let back = group.a.client.incoming().await;
        assert_eq!(back.caller.as_str(), b, "invited by whoever added her");
        group.a.client.answer(back.call);
        group.a.client.state(CallState::Connected).await;
        group
            .b
            .client
            .participants_until(|now| are(now, std::slice::from_ref(&a)))
            .await;
        let call = group.open_call().await;
        assert_eq!(group.mixer.legs(&call.id).len(), 2, "Ada's new leg and Bert's");
        group.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_callee_the_same_member_adds_back_rings_anew_after_a_decline_and_after_a_leave() {
    for backend in backends() {
        let mut group = Group::start(backend).await;
        let b = group.b.mri();
        group.a.client.call_group(&group.chat, std::slice::from_ref(&b));
        let placed = group.a.client.state(CallState::Connected).await;
        let rings = group.b.client.incoming().await;
        group.b.client.decline(rings.call);
        assert_eq!(group.b.client.ended().await, End::Declined);
        // Ada adds Bert back: a ring of its own, not the one he declined
        group.a.client.add(placed, &b, &group.chat);
        let again = group.b.client.incoming().await;
        group.b.client.answer(again.call);
        let answered = group.b.client.state(CallState::Connected).await;
        group
            .a
            .client
            .participants_until(|now| are(now, std::slice::from_ref(&b)))
            .await;
        group.b.client.hang_up(answered);
        assert_eq!(group.b.client.ended().await, End::HungUp);
        group.a.client.participants_until(<[Participant]>::is_empty).await;
        // and again once he left
        group.a.client.add(placed, &b, &group.chat);
        let third = group.b.client.incoming().await;
        // the links of an earlier ring of his are over
        let call = group.open_call().await;
        let forked = format!("/flightproxy/api/v2/ep/cc.own/cc/v1/forked/{}/1/i1", call.id);
        let progress = |ring: u64| format!("{forked}/{ring}/progress");
        let (status, _) = raw(&group.server, &group.b, reqwest::Method::POST, &progress(1), &json!({})).await;
        assert_eq!(status, 410);
        let (status, _) = raw(&group.server, &group.b, reqwest::Method::POST, &progress(3), &json!({})).await;
        assert_eq!(status, 200);
        group.b.client.answer(third.call);
        group.b.client.state(CallState::Connected).await;
        group
            .a
            .client
            .participants_until(|now| are(now, std::slice::from_ref(&b)))
            .await;
        group.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn adding_a_member_who_still_rings_leaves_its_ring_as_it_was() {
    for backend in backends() {
        let mut group = Group::start(backend).await;
        let b = group.b.mri();
        group.a.client.call_group(&group.chat, std::slice::from_ref(&b));
        let placed = group.a.client.state(CallState::Connected).await;
        let rings = group.b.client.incoming().await;
        let call = group.open_call().await;
        let offer = rings.offer.clone().unwrap().blob;
        let rung = group
            .mixer
            .legs(&call.id)
            .into_keys()
            .find(|leg| offer == offer_to(leg));
        let rung = rung.expect("Bert rings with the node's offer to his leg");
        group.a.client.add(placed, &b, &group.chat);
        let quiet = group.b.client.events_within(Duration::from_millis(500)).await;
        assert!(
            !quiet
                .iter()
                .any(|event| matches!(event, teams_call::CallEvent::Incoming(_))),
            "{quiet:#?}"
        );
        group.b.client.answer(rings.call);
        group.b.client.state(CallState::Connected).await;
        let legs = group.mixer.legs(&call.id);
        assert_eq!(legs.len(), 2, "{legs:?}");
        assert!(
            legs[&rung].answered,
            "his answer reached the leg he rang with: {legs:?}"
        );
        group.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_join_the_media_node_fails_keeps_the_ring_and_a_later_join_takes_the_same_seat() {
    for backend in backends() {
        let mut group = Group::start(backend).await;
        let (a, b, c) = (group.a.mri(), group.b.mri(), group.c.mri());
        group.a.client.call_group(&group.chat, std::slice::from_ref(&b));
        group.a.client.state(CallState::Connected).await;
        let rings = group.b.client.incoming().await;
        let call = group.open_call().await;
        let offer = rings.offer.clone().unwrap().blob;
        let rung = group
            .mixer
            .legs(&call.id)
            .into_keys()
            .find(|leg| offer == offer_to(leg));
        let rung = rung.expect("Bert rings with the node's offer to his leg");
        let running = group
            .running_call(&group.c)
            .await
            .expect("the chat names its running call");
        group.mixer.fail_legs(true);
        // Bert, still rung, joins instead of answering (from another device of his), and Cora, never rung, joins:
        // the node fails both
        let conv = format!("/flightproxy/api/v2/ep/conv.own/conv/{}", call.conversation);
        let (_, base) = own_callbacks(&group.server, &group.b.id).await;
        let join = json!({
            "callInvitation": {
                "mediaContent": {"blob": "v=0\r\ns=bert joins\r\n", "contentType": "application/sdp-ngc-1.0"},
                "links": {"acceptance": format!("{base}acceptance/"), "end": format!("{base}end/")},
            },
            "participants": {"from": {"id": b, "displayName": "Bert"}},
        });
        let (status, _) = raw(&group.server, &group.b, reqwest::Method::POST, &conv, &join).await;
        assert_eq!(status, 503);
        group.c.client.join(&group.chat, running.clone());
        assert_eq!(group.c.client.ended().await, End::Failed("join"));
        group.mixer.fail_legs(false);
        // Bert's ring is still his to answer, on the leg it rang with
        group.b.client.answer(rings.call);
        group.b.client.state(CallState::Connected).await;
        assert!(group.mixer.legs(&call.id)[&rung].answered);
        // Cora's next join takes her in
        group.c.client.join(&group.chat, running);
        group.c.client.state(CallState::Connected).await;
        group
            .a
            .client
            .participants_until(|now| are(now, &[b.clone(), c.clone()]))
            .await;
        group
            .c
            .client
            .participants_until(|now| are(now, &[a.clone(), b.clone()]))
            .await;
        let legs = group.mixer.legs(&call.id);
        assert_eq!(legs.len(), 3, "Ada's, Bert's and Cora's: {legs:?}");
        group.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_answer_a_join_of_the_same_callee_overtook_is_refused_and_the_join_takes_him_in() {
    for backend in backends() {
        let mut group = Group::start(backend).await;
        let b = group.b.mri();
        group.a.client.call_group(&group.chat, std::slice::from_ref(&b));
        group.a.client.state(CallState::Connected).await;
        let rings = group.b.client.incoming().await;
        let call = group.open_call().await;
        let offer = rings.offer.clone().unwrap().blob;
        let rung = group
            .mixer
            .legs(&call.id)
            .into_keys()
            .find(|leg| offer == offer_to(leg));
        let rung = rung.expect("Bert rings with the node's offer to his leg");
        // Bert answers on one device and, while the node takes that answer, joins from another
        let answer = group.mixer.hold_answer();
        group.b.client.answer(rings.call);
        answer.reached().await;
        let new_leg = group.mixer.hold_new_leg();
        let conv = format!("/flightproxy/api/v2/ep/conv.own/conv/{}", call.conversation);
        let (_, base) = own_callbacks(&group.server, &group.b.id).await;
        let join = json!({
            "callInvitation": {
                "mediaContent": {"blob": "v=0\r\ns=bert joins\r\n", "contentType": "application/sdp-ngc-1.0"},
                "links": {"acceptance": format!("{base}acceptance/"), "end": format!("{base}end/")},
            },
            "participants": {"from": {"id": b, "displayName": "Bert"}},
        });
        let Person { upn, client, .. } = &mut group.b;
        let joining = raw_as(&group.server, upn, reqwest::Method::POST, &conv, &join);
        let answered = async {
            new_leg.reached().await;
            // the answer comes back once the join reserved his seat over the leg the answer was for
            answer.go();
            let ended = client.ended().await;
            new_leg.go();
            ended
        };
        let ((status, _), ended) = tokio::join!(joining, answered);
        assert!(
            matches!(ended, End::Failed(_)),
            "the answer was for a leg the join replaced: {ended:?}"
        );
        assert_eq!(status, 200, "the join takes him in");
        group
            .a
            .client
            .participants_until(|now| are(now, std::slice::from_ref(&b)))
            .await;
        let legs = group.mixer.legs(&call.id);
        assert_eq!(legs.len(), 2, "Ada's and the one Bert joined on: {legs:?}");
        assert!(!legs.contains_key(&rung), "the leg he rang with is gone: {legs:?}");
        group.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_answer_that_comes_while_a_join_of_the_same_callee_is_under_way_is_refused_and_the_join_takes_him_in() {
    for backend in backends() {
        let mut group = Group::start(backend).await;
        let b = group.b.mri();
        group.a.client.call_group(&group.chat, std::slice::from_ref(&b));
        group.a.client.state(CallState::Connected).await;
        let rings = group.b.client.incoming().await;
        let call = group.open_call().await;
        let offer = rings.offer.clone().unwrap().blob;
        let rung = group
            .mixer
            .legs(&call.id)
            .into_keys()
            .find(|leg| offer == offer_to(leg));
        let rung = rung.expect("Bert rings with the node's offer to his leg");
        // Bert joins from another device and, once the node made the join's leg, answers on the ringing one
        let made = group.mixer.hold_made_leg();
        let conv = format!("/flightproxy/api/v2/ep/conv.own/conv/{}", call.conversation);
        let (_, base) = own_callbacks(&group.server, &group.b.id).await;
        let join = json!({
            "callInvitation": {
                "mediaContent": {"blob": "v=0\r\ns=bert joins\r\n", "contentType": "application/sdp-ngc-1.0"},
                "links": {"acceptance": format!("{base}acceptance/"), "end": format!("{base}end/")},
            },
            "participants": {"from": {"id": b, "displayName": "Bert"}},
        });
        let Person { upn, client, .. } = &mut group.b;
        let joining = raw_as(&group.server, upn, reqwest::Method::POST, &conv, &join);
        let mixer = &group.mixer;
        let answered = async {
            made.reached().await;
            client.answer(rings.call);
            let ended = client.ended().await;
            let legs = mixer.legs(&call.id);
            made.go();
            (ended, legs)
        };
        let ((status, _), (ended, during)) = tokio::join!(joining, answered);
        assert!(
            matches!(ended, End::Failed(_)),
            "the answer was for the leg the join is replacing: {ended:?}"
        );
        assert!(
            during.values().all(|leg| !leg.answered),
            "the answer reached no leg: {during:?}"
        );
        assert_eq!(status, 200, "the join takes him in");
        group
            .a
            .client
            .participants_until(|now| are(now, std::slice::from_ref(&b)))
            .await;
        let legs = group.mixer.legs(&call.id);
        assert_eq!(legs.len(), 2, "Ada's and the one Bert joined on: {legs:?}");
        assert!(!legs.contains_key(&rung), "the leg he rang with is gone: {legs:?}");
        group.server.stop().await;
    }
}

/// How `client`'s start comes out: up (`None`), or ended.
async fn start_outcome(client: &mut Client) -> Option<End> {
    client
        .next(|event| match event {
            teams_call::CallEvent::State {
                state: CallState::Connected,
                ..
            } => Some(None),
            teams_call::CallEvent::Ended { end, .. } => Some(Some(end)),
            _ => None,
        })
        .await
}

#[tokio::test(flavor = "multi_thread")]
async fn of_two_starts_at_once_in_a_chat_one_runs_and_the_others_room_is_dropped() {
    for backend in backends() {
        let mut group = Group::start(backend).await;
        let b = group.b.mri();
        // both starts are past the chat's check for a running call before either is stored
        group.mixer.hold_rooms(2);
        group.a.client.call_group(&group.chat, std::slice::from_ref(&b));
        group.c.client.call_group(&group.chat, std::slice::from_ref(&b));
        let (a, c) = tokio::join!(start_outcome(&mut group.a.client), start_outcome(&mut group.c.client));
        let (winner, lost) = match (a, c) {
            (None, Some(lost)) => (&group.a, lost),
            (Some(lost), None) => (&group.c, lost),
            outcomes => panic!("one start runs: {outcomes:?}"),
        };
        let End::Remote(end) = lost else {
            panic!("the other ends as the service's: {lost:?}");
        };
        assert!(end.chat_has_running_call(), "{end:?}");
        let call = group.open_call().await;
        assert_eq!(call.caller, winner.id);
        let named = end.conversation.unwrap_or_default();
        assert!(named.ends_with(&format!("/conv/{}", call.conversation)), "{named}");
        assert_eq!(group.mixer.rooms(), std::slice::from_ref(&call.id));
        assert_eq!(group.mixer.dropped_rooms(), 1, "the other start's room");
        group.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_former_party_no_longer_in_the_chat_follows_nothing() {
    let mut group = Group::start(Backend::Sqlite).await;
    let b = group.b.mri();
    group.a.client.call_group(&group.chat, std::slice::from_ref(&b));
    group.a.client.state(CallState::Connected).await;
    let rings = group.b.client.incoming().await;
    group.b.client.answer(rings.call);
    let answered = group.b.client.state(CallState::Connected).await;
    group.b.client.hang_up(answered);
    assert_eq!(group.b.client.ended().await, End::HungUp);
    let account = Account::sign_in(&group.server, &group.a.upn, &group.a.id).await;
    remove_member(&account.client(&group.server), &group.chat, &group.b).await;
    let call = group.open_call().await;
    let conv = format!("/flightproxy/api/v2/ep/conv.own/conv/{}", call.conversation);
    let (_, base) = own_callbacks(&group.server, &group.b.id).await;
    let watch =
        json!({"conversationRequest": {"roster": {"type": "Delta", "rosterUpdate": format!("{base}rosterUpdate/")}}});
    let (status, _) = raw(&group.server, &group.b, reqwest::Method::POST, &conv, &watch).await;
    assert_eq!(status, 404, "Bert left the call and the chat");
    group.server.stop().await;
}
