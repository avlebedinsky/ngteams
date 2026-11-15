//! A group call's signaling, with the client's call machine at every party and the call's media node a fixed one
//! ([`super::mixer::FixedMixer`]): the start in a group chat ringing its other members with the node's offers, the
//! caller accepted by the node at once, a callee's answer, a decline and a ring nobody answers, leaves and the last
//! one, a member joining the running call it finds in the chat, a second start refused.

use std::time::Duration;

use serde_json::Value;
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
        let removed = writer
            .write(RemoveMember {
                chat: chat.clone(),
                member: eve.mri(),
            })
            .await
            .unwrap();
        assert!(removed.is_success(), "{}", removed.status());
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
