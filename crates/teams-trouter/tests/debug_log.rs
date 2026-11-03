//! Both directions of the real socket session reach the diagnostic log; all peers and credentials are synthetic.

#![cfg(test)]

use std::fs;
use std::future::{self, Future};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::json;
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_mock::{CHAT_BASE, ChatServiceMock, LAB_B};
use teams_proto::{Credentials, HttpTransport};
use teams_trouter::{Options, PushEvent, RegistrationReport, Trouter, TrouterCredentials, TrouterHandle};
use tokio::sync::broadcast;

const INBOUND_SECRET: &str = "syntheticInboundSkypeCredential";

struct Tokens {
    token: String,
    renewals: broadcast::Sender<Renewal>,
}

impl Tokens {
    fn access(&self) -> AccessToken {
        AccessToken {
            token: Secret::new(self.token.clone()),
            expires_at: SystemTime::now() + Duration::from_secs(7200),
            scopes: Vec::new(),
        }
    }
}

impl Credentials for Tokens {
    fn bearer(&self, _: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }

    fn bearer_rejected(&self, _: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }
}

impl TrouterCredentials for Tokens {
    fn ic3(&self) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(self.access()))
    }
    fn ic3_after(&self, _: &Secret) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(self.access()))
    }
    fn renewals(&self) -> broadcast::Receiver<Renewal> {
        self.renewals.subscribe()
    }
}

async fn until(push: &mut TrouterHandle, predicate: impl Fn(&PushEvent) -> bool) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let event = push.next().await.expect("session ended before its event");
            if predicate(&event) {
                break;
            }
        }
    })
    .await
    .expect("mock socket event timed out");
}

async fn exchange() -> (String, Vec<String>) {
    let mock = ChatServiceMock::start().await.unwrap();
    let trouter = mock.start_trouter().unwrap();
    let token = mock.token(LAB_B).unwrap();
    let credentials = Arc::new(Tokens {
        token: token.clone(),
        renewals: broadcast::channel(4).0,
    });
    let transport = HttpTransport::with_mock_origin(Arc::clone(&credentials), CHAT_BASE, &mock.origin()).unwrap();
    let mut push = Trouter::with_mock_origin(Arc::new(transport), credentials, trouter.origin())
        .unwrap()
        .with_options(Options {
            connect_timeout: Duration::from_secs(5),
            hello_timeout: Duration::from_secs(5),
            ..Options::default()
        })
        .spawn();
    until(&mut push, |event| {
        matches!(event, PushEvent::Registration(RegistrationReport::Registered))
    })
    .await;
    trouter.deliver_presence(
        LAB_B,
        &json!({"wire-note": "wire-visible-inbound", "skypetoken": INBOUND_SECRET}),
        false,
    );
    trouter.disconnect(LAB_B, "test-complete");
    until(&mut push, |event| matches!(event, PushEvent::Disconnected { .. })).await;
    push.stop().await;
    let frames = trouter.sockets().into_iter().flat_map(|socket| socket.frames).collect();
    (token, frames)
}

#[tokio::test]
async fn both_frame_directions_are_complete_and_redacted_at_debug_and_absent_at_info() {
    let dir = tempfile::tempdir().unwrap();
    let log = teams_log::install(&teams_log::Config {
        dir: dir.path().into(),
        setting: teams_log::Level::Debug,
        env: None,
        rotation: teams_log::Rotation::default(),
    })
    .unwrap();
    let (token, frames) = exchange().await;
    assert!(log.flush_written());
    let debug = fs::read_to_string(log.path()).unwrap();
    assert!(!frames.is_empty());
    for frame in frames {
        let expected = format!("frame sent {}", teams_log::redact::frame(&frame));
        assert!(debug.contains(&expected), "missing sent frame: {expected}\n{debug}");
    }
    for kept in [
        "DEBUG teams_wire::trouter: frame received 1::",
        "trouter.connected",
        "user.authenticate",
        "\"args\":\"[redacted]\"",
        "wire-visible-inbound",
        "[redacted]",
    ] {
        assert!(
            debug.contains(kept),
            "missing received/authentication data {kept:?}:\n{debug}"
        );
    }
    for secret in [token.as_str(), INBOUND_SECRET] {
        assert!(!debug.contains(secret), "credential leaked: {secret}");
    }
    log.apply_setting(teams_log::Level::Info);
    let (second_token, _) = exchange().await;
    assert!(log.flush_written());
    let all = fs::read_to_string(log.path()).unwrap();
    let info = &all[debug.len()..];
    for absent in [
        "teams_wire::trouter",
        "frame sent",
        "frame received",
        "user.authenticate",
        "wire-visible-inbound",
        token.as_str(),
        second_token.as_str(),
        INBOUND_SECRET,
    ] {
        assert!(!info.contains(absent), "frame payload at info: {absent:?}:\n{info}");
    }
}
