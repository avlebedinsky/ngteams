//! The relay: coturn takes the credential the server hands a client and no other; without a relay a call isn't
//! placed.

use std::time::Duration;

use serde_json::Value;
use teams_core::engine::Event;
use teams_core::{CallAction, CallStatus};
use teams_proto::RelayTokens;

use super::pair;
use super::support::{Allocation, Rig, Turn, allocate};
use crate::support::backends;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn coturn_takes_the_servers_credential_and_refuses_an_expired_or_made_up_one() {
    for backend in backends() {
        let Some(rig) = Rig::start(backend, "relay", 30, Turn::Coturn).await else {
            return;
        };
        let (mut a, mut b, _chat) = pair(&rig).await;
        let answer = a.client().read(&RelayTokens).await.unwrap();
        let tokens = RelayTokens::read_answer(answer.body()).unwrap();
        let [credential] = tokens.credentials.as_slice() else {
            panic!("one credential: {tokens:?}");
        };
        let raw: Value = serde_json::from_slice(answer.body()).unwrap();
        assert_eq!(raw["urls"][0], format!("turn:{}?transport=udp", rig.relay));
        let (expires, account) = credential.username.split_once(':').unwrap();
        assert_eq!(account, a.oid());
        let relay = rig.relay;
        let ask = |user: &str, password: &str| {
            let (user, password) = (user.to_owned(), password.to_owned());
            tokio::task::spawn_blocking(move || allocate(relay, &user, &password))
        };

        let made = ask(&credential.username, credential.password.expose()).await.unwrap();
        assert_eq!(made, Allocation::Made, "the server's own credential");
        let refused = Allocation::Refused(401);
        let made_up = ask(&credential.username, "bWFkZS11cCBwYXNzd29yZA==").await.unwrap();
        assert_eq!(made_up, refused, "a made-up password");
        // another account's name with this one's password
        let other = format!("{expires}:{}", b.oid());
        let borrowed = ask(&other, credential.password.expose()).await.unwrap();
        assert_eq!(borrowed, refused, "a credential under another name");
        // a credential the secret really made, an hour after it ran out
        let past = ngteams_server::db::now() - 3600;
        let (user, password) = ngteams_server::calling::rest_credential(&rig.secret, a.oid(), past);
        assert_eq!(ask(&user, &password).await.unwrap(), refused, "an expired credential");
        // one of a secret that isn't coturn's
        let (user, password) = ngteams_server::calling::rest_credential("not the relay's", a.oid(), past + 7200);
        assert_eq!(ask(&user, &password).await.unwrap(), refused, "another secret");
        a.close().await;
        b.close().await;
        rig.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_a_relay_a_call_is_not_placed_and_nothing_is_left_behind() {
    for backend in backends() {
        let Some(rig) = Rig::start(backend, "no-relay", 30, Turn::Missing).await else {
            return;
        };
        let (mut a, mut b, chat) = pair(&rig).await;
        let refused = a.client().read(&RelayTokens).await;
        let error = refused.map(|_| ()).unwrap_err().to_string();
        assert!(error.contains("503"), "{error}");
        for attempt in 0..2 {
            let (from, to) = (a.mark(), b.mark());
            match a.press(CallAction::Call { chat: chat.clone() }).await {
                Err(error) => eprintln!("attempt {attempt}: the client refused the call: {error}"),
                Ok(call) => {
                    let end = a.ended(from, call).await;
                    eprintln!("attempt {attempt}: the call ended: {end:?}");
                    assert!(matches!(end, teams_core::End::Failed(_)), "{end:?}");
                }
            }
            let rang = |event: &Event| {
                matches!(
                    event,
                    Event::IncomingCall(_)
                        | Event::Call {
                            status: CallStatus::Ringing | CallStatus::Connected,
                            ..
                        }
                )
            };
            assert!(b.quiet(to, Duration::from_secs(2), rang).await, "b rang");
            assert!(!a.log[from..].iter().any(rang), "a's call rang");
            assert!(rig.open_calls().await.is_empty());
        }
        a.close().await;
        b.close().await;
        rig.stop().await;
    }
}
