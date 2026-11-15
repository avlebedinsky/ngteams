//! A group call's media through the server's own media node (`ngteams_server::mixer`): the client's engines
//! (`teams-core`, `teams-media` on the file device backend) call in a group chat, relayed through the loopback TURN
//! server to the node, and each hears the others' tones mixed, never its own.

use std::time::Duration;

use ngteams_server::mixer::MediaNode;
use teams_core::engine::Action;
use teams_core::{CallAction, CallId, CallStatus, CallView, End};
use teams_media::test_turn::TurnServer;
use teams_media::tone::Tone;
use teams_proto::Mri;
use teams_store::OutboxState;

use super::engines::{A_TONE, B_TONE, Engine, HEARD, TALK, WAIT, heard, relayed};
use crate::support::{Backend, TestServer, backends, redis_url};
use crate::trouter::{Account, ORIGIN};

const C_TONE: Tone = Tone {
    hz: 880.0,
    amplitude: 0.5,
};
const SILENT: Tone = Tone {
    hz: 440.0,
    amplitude: 0.0,
};
/// Below this, a tone isn't in a recording.
const ABSENT: f32 = 0.05;

/// The media node's configuration on loopback: its sockets on free ports, its candidate the loopback address.
fn on_loopback(config: &mut ngteams_server::config::Config) {
    config.mixer.udp_listen = ([127, 0, 0, 1], 0).into();
    config.mixer.internal_listen = ([127, 0, 0, 1], 0).into();
    config.mixer.public_addrs = vec![[127, 0, 0, 1].into()];
}

/// A server with its media node, the loopback TURN relay it names, and a directory for the recordings.
struct Mixed {
    server: TestServer,
    node: Option<MediaNode>,
    turn: TurnServer,
    dir: tempfile::TempDir,
}

impl Mixed {
    async fn start(backend: Backend) -> Self {
        Self::start_with(backend, |_| {}).await
    }

    async fn start_with(backend: Backend, configure: impl FnOnce(&mut ngteams_server::config::Config)) -> Self {
        let turn = TurnServer::start().unwrap();
        let server = TestServer::start_with(backend, |config| {
            relayed(turn.udp)(config);
            on_loopback(config);
            if backend == Backend::Postgres {
                config.redis_url = redis_url();
            }
            configure(config);
        })
        .await;
        let node = MediaNode::start(&server.app).await.unwrap();
        assert!(ngteams_server::mixer::install(&server.app, Some(&node)).unwrap());
        Self {
            server,
            node: Some(node),
            turn,
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn node(&self) -> &MediaNode {
        self.node.as_ref().unwrap()
    }

    /// A new account's engine, its microphone `tone`.
    async fn engine(&self, name: &str, tone: Tone) -> Engine {
        let tag = uuid::Uuid::new_v4().simple().to_string();
        let upn = format!("{}-{tag}@own.test", name.to_lowercase());
        let id = self.server.add_user(&upn, name).await;
        let account = Account::sign_in(&self.server, &upn, &id).await;
        Engine::start(&account, name, (&self.server, &self.server), (tone, self.dir.path())).await
    }

    /// A member with no client.
    async fn member(&self, name: &str) -> String {
        let tag = uuid::Uuid::new_v4().simple().to_string();
        let id = self.server.add_user(&format!("{name}-{tag}@own.test"), name).await;
        format!("8:orgid:{id}")
    }

    /// The call that isn't over, its id.
    async fn open_call(&self) -> String {
        let calls = ngteams_server::calling::open_calls(&self.server.app.db).await.unwrap();
        let [call] = calls.as_slice() else {
            panic!("one call: {calls:?}");
        };
        call.id.clone()
    }

    /// Until the node's room of `call` has `legs` legs, each connected with its control plane up.
    async fn connected(&self, call: &str, legs: usize) {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let room = self.node().room(call).await.unwrap_or_default();
            if room.len() == legs && room.iter().all(|leg| leg.connected && leg.control) {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the room didn't connect: {room:?}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn stop(mut self) {
        if let Some(node) = self.node.take() {
            node.stop().await;
        }
        self.server.stop().await;
        let _ = &self.turn;
    }
}

/// The group chat `first` makes with `others` and the members `quiet`, once every other engine has it.
async fn group_chat(first: &Engine, others: &[&Engine], quiet: &[String]) -> String {
    let mut members: Vec<Mri> = others.iter().map(|engine| Mri::new(engine.mri.clone())).collect();
    members.extend(quiet.iter().map(Mri::new));
    let create = Action::CreateChat {
        others: members,
        topic: Some("Media".to_owned()),
    };
    let row = first.handle.write(create).await.unwrap();
    assert_eq!(row.state, OutboxState::Sent, "{row:?}");
    let chat = row.message_id.unwrap();
    for engine in std::iter::once(first).chain(others.iter().copied()) {
        assert!(engine.handle.sync().await.unwrap().failed.is_empty());
    }
    chat
}

/// The tone `tone`'s level in the last stretch of `engine`'s recording.
fn level(engine: &Engine, tone: Tone) -> f32 {
    heard(&engine.recording, tone, TALK)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_client_hears_its_own_silence_and_gets_the_control_planes_ack() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut a = mixed.engine("Ada", A_TONE).await;
        let (b, c) = (mixed.member("Bert").await, mixed.member("Cora").await);
        let chat = group_chat(&a, &[], &[b, c]).await;
        let call = a.call(CallAction::Call { chat }).await;
        a.status(call, &CallStatus::Connected).await;
        let id = mixed.open_call().await;
        mixed.connected(&id, 3).await;
        tokio::time::sleep(TALK + Duration::from_millis(500)).await;
        a.call(CallAction::HangUp { call }).await;
        a.ended(call).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let own = level(&a, A_TONE);
        eprintln!("a heard its own tone at {own:.3}");
        assert!(own < ABSENT, "a heard itself at {own:.3}");
        a.shut_down().await;
        mixed.stop().await;
    }
}

/// Each engine's call id once all are connected: the first places the call, the others answer their rings.
async fn connect_all(chat: &str, engines: &mut [&mut Engine]) -> Vec<CallId> {
    let (first, rest) = engines.split_first_mut().unwrap();
    let call = first.call(CallAction::Call { chat: chat.to_owned() }).await;
    first.status(call, &CallStatus::Connected).await;
    let mut calls = vec![call];
    for engine in rest.iter_mut() {
        let incoming = engine.incoming().await;
        let answered = engine.call(CallAction::Answer { call: incoming.call }).await;
        engine.status(answered, &CallStatus::Connected).await;
        calls.push(answered);
    }
    calls
}

/// Each engine hangs up its call, and waits for its end.
async fn hang_up_all(engines: &mut [&mut Engine], calls: &[CallId]) {
    for (engine, call) in engines.iter_mut().zip(calls) {
        engine.call(CallAction::HangUp { call: *call }).await;
        engine.ended(*call).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
}

/// `engine` heard each of `heard` and not `not`.
fn hears(engine: &Engine, heard: &[Tone], not: Tone) {
    let levels: Vec<f32> = heard.iter().map(|tone| level(engine, *tone)).collect();
    let own = level(engine, not);
    eprintln!("{} heard {levels:?}, its own {own:.3}", engine.mri);
    assert!(levels.iter().all(|level| *level > HEARD), "{}: {levels:?}", engine.mri);
    assert!(own < ABSENT, "{} heard itself at {own:.3}", engine.mri);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_clients_hear_each_others_tones_mixed() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut a = mixed.engine("Ada", A_TONE).await;
        let mut b = mixed.engine("Bert", B_TONE).await;
        let mut c = mixed.engine("Cora", C_TONE).await;
        let chat = group_chat(&a, &[&b, &c], &[]).await;
        let calls = connect_all(&chat, &mut [&mut a, &mut b, &mut c]).await;
        mixed.connected(&mixed.open_call().await, 3).await;
        tokio::time::sleep(2 * TALK).await;
        hang_up_all(&mut [&mut a, &mut b, &mut c], &calls).await;
        hears(&a, &[B_TONE, C_TONE], A_TONE);
        hears(&b, &[A_TONE, C_TONE], B_TONE);
        hears(&c, &[A_TONE, B_TONE], C_TONE);
        for engine in [a, b, c] {
            engine.shut_down().await;
        }
        mixed.stop().await;
    }
}

/// Whether `view` shows `mri` speaking.
fn speaks(view: &CallView, mri: &str) -> bool {
    view.participants.iter().any(|party| party.mri == mri && party.speaking)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_speaker_is_told_as_the_dominant_one() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut a = mixed.engine("Ada", A_TONE).await;
        let mut b = mixed.engine("Bert", SILENT).await;
        let mut c = mixed.engine("Cora", SILENT).await;
        let chat = group_chat(&a, &[&b, &c], &[]).await;
        let calls = connect_all(&chat, &mut [&mut a, &mut b, &mut c]).await;
        let speaker = a.mri.clone();
        b.view(calls[1], |view| speaks(view, &speaker)).await;
        c.view(calls[2], |view| speaks(view, &speaker)).await;
        hang_up_all(&mut [&mut a, &mut b, &mut c], &calls).await;
        for engine in [a, b, c] {
            engine.shut_down().await;
        }
        mixed.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_late_joiner_hears_the_call_and_is_heard() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut a = mixed.engine("Ada", A_TONE).await;
        let mut b = mixed.engine("Bert", B_TONE).await;
        let mut c = mixed.engine("Cora", C_TONE).await;
        let chat = group_chat(&a, &[&b, &c], &[]).await;
        let mut calls = connect_all(&chat, &mut [&mut a, &mut b]).await;
        // c lets its ring go, then joins the call it finds in the chat
        let rung = c.incoming().await;
        c.call(CallAction::Decline { call: rung.call }).await;
        assert!(c.handle.sync().await.unwrap().failed.is_empty());
        let joined = c.call(CallAction::Join { chat: chat.clone() }).await;
        c.status(joined, &CallStatus::Connected).await;
        calls.push(joined);
        mixed.connected(&mixed.open_call().await, 3).await;
        tokio::time::sleep(2 * TALK).await;
        hang_up_all(&mut [&mut a, &mut b, &mut c], &calls).await;
        hears(&c, &[A_TONE, B_TONE], C_TONE);
        hears(&a, &[B_TONE, C_TONE], A_TONE);
        for engine in [a, b, c] {
            engine.shut_down().await;
        }
        mixed.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_callee_that_leaves_is_no_longer_heard_and_the_call_goes_on() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut a = mixed.engine("Ada", A_TONE).await;
        let mut b = mixed.engine("Bert", B_TONE).await;
        let mut c = mixed.engine("Cora", C_TONE).await;
        let chat = group_chat(&a, &[&b, &c], &[]).await;
        let calls = connect_all(&chat, &mut [&mut a, &mut b, &mut c]).await;
        let id = mixed.open_call().await;
        mixed.connected(&id, 3).await;
        tokio::time::sleep(TALK).await;
        c.call(CallAction::HangUp { call: calls[2] }).await;
        c.ended(calls[2]).await;
        let gone = c.mri.clone();
        a.view(calls[0], |view| view.participants.iter().all(|party| party.mri != gone))
            .await;
        mixed.connected(&id, 2).await;
        tokio::time::sleep(2 * TALK).await;
        assert_eq!(mixed.open_call().await, id, "the call goes on");
        hang_up_all(&mut [&mut a, &mut b], &calls[..2]).await;
        hears(&a, &[B_TONE], A_TONE);
        let left = level(&a, C_TONE);
        assert!(left < ABSENT, "a still heard c at {left:.3}");
        for engine in [a, b, c] {
            engine.shut_down().await;
        }
        mixed.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signaling_and_media_on_different_nodes() {
    let Some(redis) = redis_url() else { return };
    if !backends().contains(&Backend::Postgres) {
        return;
    }
    let turn = TurnServer::start().unwrap();
    let shared = |config: &mut ngteams_server::config::Config| {
        relayed(turn.udp)(config);
        on_loopback(config);
        config.redis_url = Some(redis.clone());
        config.mixer.heartbeat = 1;
        config.mixer.dead_after = 3;
    };
    let api = TestServer::start_with(Backend::Postgres, shared).await;
    let database = api.app.config.database_url.clone();
    let media = TestServer::start_with(Backend::Postgres, |config| {
        shared(config);
        config.database_url = database;
    })
    .await;
    // the signaling node has no media node of its own: a new room goes to the one the registry lists
    assert!(ngteams_server::mixer::install(&api.app, None).unwrap());
    let node = MediaNode::start(&media.app).await.unwrap();
    assert!(ngteams_server::mixer::install(&media.app, Some(&node)).unwrap());
    let mixed = Mixed {
        server: api,
        node: Some(node),
        turn,
        dir: tempfile::tempdir().unwrap(),
    };
    let mut a = mixed.engine("Ada", A_TONE).await;
    let mut b = mixed.engine("Bert", B_TONE).await;
    let quiet = mixed.member("Cora").await;
    let chat = group_chat(&a, &[&b], &[quiet]).await;
    let calls = connect_all(&chat, &mut [&mut a, &mut b]).await;
    mixed.connected(&mixed.open_call().await, 3).await;
    tokio::time::sleep(2 * TALK).await;
    hang_up_all(&mut [&mut a, &mut b], &calls).await;
    hears(&a, &[B_TONE], A_TONE);
    hears(&b, &[A_TONE], B_TONE);
    for engine in [a, b] {
        engine.shut_down().await;
    }
    mixed.stop().await;
    media.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dead_media_node_ends_the_call_for_everyone() {
    for backend in backends() {
        let mut mixed = Mixed::start(backend).await;
        let mut a = mixed.engine("Ada", A_TONE).await;
        let mut b = mixed.engine("Bert", B_TONE).await;
        let quiet = mixed.member("Cora").await;
        let chat = group_chat(&a, &[&b], &[quiet]).await;
        let calls = connect_all(&chat, &mut [&mut a, &mut b]).await;
        mixed.connected(&mixed.open_call().await, 3).await;
        mixed.node.take().unwrap().stop().await;
        assert!(matches!(a.ended(calls[0]).await, End::Remote(_)));
        assert!(matches!(b.ended(calls[1]).await, End::Remote(_)));
        assert!(
            ngteams_server::calling::open_calls(&mixed.server.app.db)
                .await
                .unwrap()
                .is_empty()
        );
        for engine in [a, b] {
            engine.shut_down().await;
        }
        mixed.stop().await;
    }
}

/// An offer of a client whose media never comes up: str0m's audio and data channel, its one candidate a port nobody
/// reads.
fn dead_offer() -> teams_proto::MediaContent {
    let mut rtc = str0m::RtcConfig::new().build(std::time::Instant::now());
    rtc.add_local_candidate(str0m::Candidate::host(([127, 0, 0, 1], 9).into(), "udp").unwrap());
    let mut change = rtc.sdp_api();
    change.add_media(
        str0m::media::MediaKind::Audio,
        str0m::media::Direction::SendRecv,
        None,
        None,
        None,
    );
    change.add_channel("main-channel".to_owned());
    let (offer, _) = change.apply().unwrap();
    let written = teams_sdp::sdp::NgcSession::mixer()
        .write_offer(&teams_sdp::SdpOffer::new(offer.to_sdp_string()))
        .unwrap();
    teams_proto::MediaContent::new(written.as_str(), written.content_type())
}

/// The running call the chat's events name, as the account `upn` reads them.
async fn running_call(server: &TestServer, upn: &str, id: &str, chat: &str) -> teams_proto::CallLink {
    let reader = Account::sign_in(server, upn, id).await.client(server);
    let page = teams_proto::chat::ChatReads::new(&reader)
        .messages(chat, 50)
        .await
        .unwrap();
    let events: Vec<(String, String)> = page
        .messages
        .iter()
        .map(|message| (message.message_type.as_str().to_owned(), message.content.clone()))
        .collect();
    teams_proto::running_call_in(
        &teams_proto::Cloud::own(ORIGIN).unwrap(),
        events.iter().map(|(kind, content)| (kind.as_str(), content.as_str())),
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_leg_without_media_is_taken_out_of_the_roster() {
    for backend in backends() {
        let mixed = Mixed::start_with(backend, |config| config.mixer.connect_timeout = 2).await;
        let mut a = mixed.engine("Ada", A_TONE).await;
        let tag = uuid::Uuid::new_v4().simple().to_string();
        let upn = format!("bert-{tag}@own.test");
        let id = mixed.server.add_user(&upn, "Bert").await;
        let mut b = super::client(&mixed.server, &mixed.server, &upn, &id, "Bert").await;
        let quiet = mixed.member("Cora").await;
        let chat = group_chat(&a, &[], &[b.mri.clone(), quiet]).await;
        let call = a.call(CallAction::Call { chat: chat.clone() }).await;
        a.status(call, &CallStatus::Connected).await;
        let rung = b.incoming().await;
        b.decline(rung.call);
        b.ended().await;
        // b joins with an offer whose media never comes up: it is in the roster, then taken out
        b.offer_with(dead_offer());
        b.join(&chat, running_call(&mixed.server, &upn, &id, &chat).await);
        let joiner = b.mri.clone();
        a.view(call, |view| view.participants.iter().any(|party| party.mri == joiner))
            .await;
        a.view(call, |view| view.participants.iter().all(|party| party.mri != joiner))
            .await;
        let taken_out = b.ended().await;
        eprintln!("b's call ended: {taken_out:?}");
        a.call(CallAction::HangUp { call }).await;
        a.ended(call).await;
        a.shut_down().await;
        mixed.stop().await;
    }
}
