//! An own-server account's calls on the relay its server names (`calling.turn.urls`, `docs/server.md` "The TURN
//! relay"): the media goes through that coturn and reaches no other host, with the direct path between the clients
//! blocked; a relay configuration the server can't use stops its start; a relay that refuses, isn't there, or is
//! named on a Microsoft host fails the call at its setup, never connected and silent; a call outlasts its
//! credential; Microsoft accounts in the same process keep Teams' relay and their own tokens.
//!
//! The network checks need the QA network ([`QaNet`]), the coturn checks its log ([`COTURN_LOG`]); each prints why it
//! is skipped without them.

use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::Value;
use teams_core::{CallAction, CallStatus, End};

use super::coturn_log::{COTURN_LOG, CoturnLog, Entry};
use super::qa_net::{QaNet, Seen};
use super::support::{A_TONE, B_TONE, COTURN_SECRET, HEARD, Phone, Rig, TALK, Turn, coturn};
use super::{connect, hang_up, heard_each_other, loudness, pair};
use crate::binary::support::Deployment;
use crate::support::{Backend, backends};

/// Nothing outside loopback was tried, no name was asked, no client sent straight to another.
fn stayed_on_the_relay(net: &QaNet, before: &Seen) {
    let after = net.seen().since(before);
    eprintln!("the network since the start: {after:?}");
    assert_eq!(after, Seen::default(), "the run reached past its relay");
}

/// The relay's URLs and credential the server hands `phone`'s account, as JSON.
async fn relay_answer(phone: &Phone) -> Value {
    let answer = phone.client().read(&teams_proto::RelayTokens).await.unwrap();
    serde_json::from_slice(answer.body()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn calls_and_a_redial_carry_media_through_the_named_coturn_and_nothing_else() {
    for backend in backends() {
        let Some(rig) = Rig::start(backend, "own-relay", 30, Turn::Coturn).await else {
            return;
        };
        let net = QaNet::here();
        if let Some(net) = &net {
            net.direct_path_is_blocked();
        }
        let before = net.as_ref().map(QaNet::seen);
        let log = CoturnLog::from_now(COTURN_LOG);
        let (mut a, mut b, chat) = pair(&rig).await;

        let answer = relay_answer(&a).await;
        assert_eq!(
            answer["urls"],
            serde_json::json!([format!("turn:{}?transport=udp", rig.relay)])
        );
        let username = answer["tokens"][0]["username"].as_str().unwrap();
        assert!(username.ends_with(&format!(":{}", a.oid())), "{username}");

        // a calls b, b answers; they hang up; a calls again
        for round in 0..2 {
            let talk = connect(&mut a, &mut b, &chat).await;
            tokio::time::sleep(TALK).await;
            heard_each_other(&a, A_TONE, &b, B_TONE).await;
            if round == 0 {
                hang_up(&mut a, talk.call, &mut b, talk.answered).await;
            } else {
                hang_up(&mut b, talk.answered, &mut a, talk.call).await;
            }
            rig.no_open_calls().await;
        }

        if let Some(log) = &log {
            for phone in [&a, &b] {
                let (allocations, relayed) = (log.allocations(phone.oid()), log.relayed(phone.oid()));
                eprintln!("coturn for {}: {allocations} allocations, {relayed} bytes", phone.upn);
                assert!(allocations >= 2, "{}: {:?}", phone.upn, log.of(phone.oid()));
                assert!(relayed > 10_000, "{}: {:?}", phone.upn, log.of(phone.oid()));
            }
            let users = log.users();
            assert!(
                users
                    .iter()
                    .all(|user| user.ends_with(a.oid()) || user.ends_with(b.oid())),
                "coturn served someone else: {users:?}"
            );
        } else {
            eprintln!("coturn checks skipped: {COTURN_LOG} names no log");
        }
        if let (Some(net), Some(before)) = (&net, &before) {
            stayed_on_the_relay(net, before);
        }
        a.close().await;
        b.close().await;
        rig.stop().await;
    }
}

/// The real binary's `serve` with the nodes' config and `turn` as its `[calling.turn]`: it must exit by itself.
/// Its standard error.
async fn refused_start(turn: &str) -> String {
    let deployment = Deployment::new(Backend::Sqlite, false, None).await;
    let node = deployment.node("a");
    let mut config = std::fs::read_to_string(&node.config).unwrap();
    config.push_str("[calling.turn]\n");
    config.push_str(turn);
    std::fs::write(&node.config, config).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_ngteams-server"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("NGTEAMS_SERVER_") {
            command.env_remove(key);
        }
    }
    let mut child = command
        .args(["--config", node.config.to_str().unwrap(), "serve"])
        .env("NGTEAMS_SERVER_LOG", "warn")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(!status.success(), "{turn}: the server exited as if it ran");
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the server started with [calling.turn] {turn}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let mut error = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut error).unwrap();
    error
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_relay_the_server_cant_hand_out_stops_its_start_and_says_which_key() {
    for (turn, key) in [
        // a secret with no relay named, and an empty list of them
        ("secret = \"qa-secret\"\n", "calling.turn.urls"),
        ("secret = \"qa-secret\"\nurls = []\n", "calling.turn.urls"),
        // a relay named with no credential for it
        ("urls = [\"turn:127.0.0.1:53331?transport=udp\"]\n", "calling.turn.urls"),
        // URLs that aren't TURN URLs, or have no usable host or port
        (
            "secret = \"qa-secret\"\nurls = [\"stun:127.0.0.1:53331\"]\n",
            "calling.turn.urls",
        ),
        (
            "secret = \"qa-secret\"\nurls = [\"https://127.0.0.1:53331\"]\n",
            "calling.turn.urls",
        ),
        ("secret = \"qa-secret\"\nurls = [\"turn:\"]\n", "calling.turn.urls"),
        (
            "secret = \"qa-secret\"\nurls = [\"turn:127.0.0.1:port\"]\n",
            "calling.turn.urls",
        ),
        (
            "secret = \"qa-secret\"\nurls = [\"127.0.0.1:53331\"]\n",
            "calling.turn.urls",
        ),
        (
            "secret = \"qa-secret\"\nurls = [\"turn:127.0.0.1:53331?transport=sctp\"]\n",
            "calling.turn.urls",
        ),
        (
            "secret = \"qa-secret\"\nurls = [\"turns:127.0.0.1:5349?transport=udp\"]\n",
            "calling.turn.urls",
        ),
        // one good URL beside a bad one
        (
            "secret = \"qa-secret\"\nurls = [\"turn:127.0.0.1:53331\", \"turn: 127.0.0.1\"]\n",
            "calling.turn.urls",
        ),
        // a long-term user with an empty password
        (
            "username = \"relay\"\npassword = \"\"\nurls = [\"turn:127.0.0.1:53331\"]\n",
            "calling.turn.password",
        ),
        (
            "secret = \"qa-secret\"\nttl = 0\nurls = [\"turn:127.0.0.1:53331\"]\n",
            "calling.turn.ttl",
        ),
    ] {
        let error = refused_start(turn).await;
        eprintln!("[calling.turn] {turn:?} -> {error}");
        assert!(error.contains(key), "{turn:?}: {error}");
        assert!(!error.contains("qa-secret"), "the secret in the error: {error}");
    }
}

/// a presses Call to b in `chat` on a relay that gives no media: the press is refused at the media's setup (or the
/// call ends failed), the call never connects and b never rings. What the client said.
async fn fails_at_setup(rig: &Rig, a: &mut Phone, b: &mut Phone, chat: &str) -> String {
    let (from, to) = (a.mark(), b.mark());
    let said = match a.press(CallAction::Call { chat: chat.to_owned() }).await {
        Err(error) => error,
        Ok(call) => {
            let end = a.ended(from, call).await;
            assert!(matches!(end, End::Failed(_)), "the call ended {end:?}");
            format!("{end:?}")
        }
    };
    let up = |event: &teams_core::engine::Event| {
        matches!(
            event,
            teams_core::engine::Event::IncomingCall(_)
                | teams_core::engine::Event::Call {
                    status: CallStatus::Ringing | CallStatus::Connected,
                    ..
                }
        )
    };
    assert!(b.quiet(to, Duration::from_secs(2), up).await, "b rang");
    assert!(!a.log[from..].iter().any(up), "a's call rang or connected: {said}");
    rig.no_open_calls().await;
    said
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_relay_that_refuses_isnt_there_or_is_microsofts_fails_the_call_at_setup() {
    let Some((relay, secret)) = coturn() else {
        eprintln!("skipped: no coturn");
        return;
    };
    let closed = {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.local_addr().unwrap()
    };
    let microsoft = teams_media::relay::TEAMS_RELAY_HOST;
    for (case, turn) in [
        (
            "coturn refuses the credential (another secret)",
            format!("secret = \"not {COTURN_SECRET}\"\nurls = [\"turn:{relay}?transport=udp\"]\n"),
        ),
        (
            "nothing listens where the relay is named",
            format!("secret = {secret:?}\nurls = [\"turn:{closed}?transport=udp\"]\n"),
        ),
        (
            "the relay is named on Microsoft's host",
            format!(
                "secret = {secret:?}\nurls = [\"turn:{microsoft}:3478?transport=udp\", \"turns:{microsoft}:443\"]\n"
            ),
        ),
    ] {
        let Some(rig) = Rig::start_with(Backend::Sqlite, "relay-fails", 30, relay, Some(&turn)).await else {
            return;
        };
        let net = QaNet::here();
        let before = net.as_ref().map(QaNet::seen);
        let log = CoturnLog::from_now(COTURN_LOG);
        let (mut a, mut b, chat) = pair(&rig).await;
        for attempt in 0..2 {
            let said = fails_at_setup(&rig, &mut a, &mut b, &chat).await;
            eprintln!("{case}, attempt {attempt}: {said}");
            assert!(said.contains("media couldn't be set up"), "{case}: {said}");
        }
        if let Some(log) = &log {
            let allocated = log.allocations(a.oid());
            eprintln!(
                "{case}: coturn allocated {allocated} for a; a's entries {:?}",
                log.of(a.oid())
            );
            assert_eq!(allocated, 0, "{case}");
        }
        if let (Some(net), Some(before)) = (&net, &before) {
            let after = net.seen().since(before);
            eprintln!("{case}: the network {after:?}");
            // the allocations sent to the named relay where nothing listens are the only UDP off the run's coturn
            let to_named = format!(" . {}", closed.port());
            let direct = after.direct.keys().all(|flow| flow.ends_with(&to_named));
            assert!(after.outside.is_empty() && direct, "{case}: {after:?}");
            assert!(
                after.names.iter().all(|name| !name.contains("microsoft")),
                "{case}: the client looked Microsoft's relay up: {after:?}"
            );
        }
        a.close().await;
        b.close().await;
        rig.stop().await;
    }
}

/// How loud each side heard the other in the last [`TALK`], or in a later one after a loaded host's dip
/// ([`loudness`]).
async fn levels(a: &Phone, b: &Phone) -> (f32, f32) {
    let heard = loudness(&[(a, B_TONE), (b, A_TONE)]).await;
    (heard[0], heard[1])
}

/// The requests coturn took from `oid` (method, refused with), from `log`.
fn requests(log: &CoturnLog, oid: &str) -> Vec<(String, Option<u16>, String)> {
    log.of(oid)
        .into_iter()
        .filter_map(|entry| match entry {
            Entry::Request { user, method, error } => Some((method, error, user)),
            Entry::Usage { .. } => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_longer_than_its_relay_credential_keeps_its_media_and_the_next_call_gets_a_new_one() {
    let Some((relay, secret)) = coturn() else {
        eprintln!("skipped: no coturn");
        return;
    };
    let ttl = 15;
    let turn = format!("secret = {secret:?}\nttl = {ttl}\nurls = [\"turn:{relay}?transport=udp\"]\n");
    let Some(rig) = Rig::start_with(Backend::Sqlite, "credential-expiry", 30, relay, Some(&turn)).await else {
        return;
    };
    let (mut a, mut b, chat) = pair(&rig).await;
    let answer = relay_answer(&a).await;
    assert_eq!(answer["expires"], ttl);
    let talk = connect(&mut a, &mut b, &chat).await;
    let connected = tokio::time::Instant::now();
    // the credential runs out 15 s after it was made; libwebrtc refreshes the relay's permissions every 4 minutes
    tokio::time::sleep_until(connected + Duration::from_secs(ttl + 5)).await;
    let expired = CoturnLog::from_now(COTURN_LOG);
    let mut heard = Vec::new();
    for at in [8_u64, 60, 150, 255, 275] {
        tokio::time::sleep_until(connected + Duration::from_secs(at)).await;
        let level = levels(&a, &b).await;
        eprintln!(
            "{at} s into the call: a heard b at {:.3}, b heard a at {:.3}",
            level.0, level.1
        );
        heard.push((at, level));
    }
    hang_up(&mut a, talk.call, &mut b, talk.answered).await;
    for (at, (a_heard, b_heard)) in &heard {
        assert!(
            *a_heard > HEARD && *b_heard > HEARD,
            "{at} s into the call (credential {ttl} s): a heard {a_heard:.3}, b heard {b_heard:.3}; all {heard:?}"
        );
    }
    if let Some(log) = &expired {
        for phone in [&a, &b] {
            let taken = requests(log, phone.oid());
            eprintln!("coturn took from {} after its credential ran out: {taken:?}", phone.upn);
            assert!(
                taken.iter().all(|(_, error, _)| error.is_none()),
                "{}: {taken:?}",
                phone.upn
            );
            assert!(
                taken.iter().any(|(method, _, _)| method == "CREATE_PERMISSION"),
                "{}: no permission refresh after the expiry: {taken:?}",
                phone.upn
            );
        }
    }

    // the next call takes a new credential and has its media
    let redial = CoturnLog::from_now(COTURN_LOG);
    let first = answer["tokens"][0]["username"].as_str().unwrap().to_owned();
    let talk = connect(&mut a, &mut b, &chat).await;
    tokio::time::sleep(TALK).await;
    heard_each_other(&a, A_TONE, &b, B_TONE).await;
    hang_up(&mut b, talk.answered, &mut a, talk.call).await;
    if let Some(log) = &redial {
        let allocated: Vec<_> = requests(log, a.oid())
            .into_iter()
            .filter(|(method, error, _)| method == "ALLOCATE" && error.is_none())
            .map(|(_, _, user)| user)
            .collect();
        eprintln!("the redial allocated as {allocated:?} (the first call as {first})");
        assert!(!allocated.is_empty() && allocated.iter().all(|user| *user != first));
    }
    rig.no_open_calls().await;
    a.close().await;
    b.close().await;
    rig.stop().await;
}
