//! The signaling module on its synthetic fixtures (`tests/fixtures/synthetic/`, written by hand from the bundle, each
//! marked `_synthetic`; real ones come from the Task 18 capture), the call start built from a permitted call, and the
//! incoming-call policy: an incoming call is ignored and never answered.

use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use echo_call::signaling::{
    Action, CallState, Callback, Endpoint, Inbound, OutgoingCall, RelayDescriptor, RelayToken, SDP_NGC, call_start_body,
};
use echo_call::turn::Transport;
use fixture_guard::Guard;
use serde_json::Value;
use teams_fence::{FailClosed, Fence, FenceLog, Mri, Roster};
use teams_proto::{Authorized, Mutating as _, StartCall};

const LAB_A: &str = "8:orgid:lab-a";
const ECHO: &str = "28:echo-service";
const CALLBACK_BASE: &str = "https://callback.example.invalid/endpoint/";

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/synthetic")
}

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

fn fixture(name: &str) -> TestResult<Value> {
    let text = std::fs::read_to_string(fixtures_dir().join(name))?;
    Ok(serde_json::from_str(&text)?)
}

/// The delivery a fixture holds.
fn inbound(name: &str) -> TestResult<Inbound> {
    let value = fixture(name)?;
    let path = value["path"].as_str().ok_or("the fixture has no path")?;
    Ok(Inbound::parse(path, &value["body"]))
}

#[test]
fn every_fixture_is_marked_synthetic_and_passes_the_fixture_guard() {
    for entry in std::fs::read_dir(fixtures_dir()).unwrap() {
        let path = entry.unwrap().path();
        let value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let marker = value["_synthetic"].as_str().unwrap_or_default();
        assert!(
            marker.starts_with("SYNTHETIC") && marker.contains("Task 18"),
            "{} lacks the synthetic marker",
            path.display()
        );
    }
    let findings = Guard::new().unwrap().scan_paths(&[&fixtures_dir()]).unwrap();
    assert!(findings.is_empty(), "{findings:?}");
}

#[test]
fn callbacks_parse_from_their_paths() {
    assert_eq!(inbound("progress.json").unwrap(), Inbound::Progress);
    assert_eq!(inbound("acceptance.json").unwrap(), Inbound::Accepted);
    assert_eq!(inbound("end.json").unwrap(), Inbound::Ended { code: Some(0) });
    let Inbound::MediaAnswer {
        sdp,
        content_type,
        acknowledgement,
    } = inbound("media-answer.json").unwrap()
    else {
        panic!("expected a media answer");
    };
    assert!(sdp.starts_with("v=0\r\n") && sdp.contains("opus/48000/2"));
    assert_eq!(content_type, SDP_NGC);
    assert!(acknowledgement.is_some_and(|link| link.ends_with("/mediaAcknowledgement")));
}

#[test]
fn malformed_or_unknown_deliveries_are_unknown() {
    assert_eq!(
        Inbound::parse("/call/mediaAnswer/", &serde_json::json!({})),
        Inbound::Unknown
    );
    assert_eq!(
        Inbound::parse("/somewhere/else", &serde_json::json!({})),
        Inbound::Unknown
    );
    assert_eq!(
        Inbound::parse("/call/end/", &serde_json::json!({})),
        Inbound::Ended { code: None }
    );
}

#[test]
fn an_incoming_call_is_recognized_whatever_its_path() {
    let value = fixture("incoming-call.json").unwrap();
    assert_eq!(inbound("incoming-call.json").unwrap(), Inbound::IncomingCall);
    for path in ["/call/acceptance/", "/call/mediaAnswer/", ""] {
        assert_eq!(Inbound::parse(path, &value["body"]), Inbound::IncomingCall, "{path}");
    }
}

#[test]
fn an_incoming_call_is_ignored_in_every_state_and_never_answered() {
    let incoming = inbound("incoming-call.json").unwrap();
    let script = [
        None,
        Some(inbound("progress.json").unwrap()),
        Some(inbound("media-answer.json").unwrap()),
        Some(inbound("acceptance.json").unwrap()),
        Some(inbound("end.json").unwrap()),
    ];
    let mut call = OutgoingCall::new();
    for (step, delivery) in script.into_iter().enumerate() {
        if let Some(delivery) = delivery {
            call.on_inbound(delivery);
        }
        let state = call.state();
        assert_eq!(call.on_inbound(incoming.clone()), Action::Nothing, "step {step}");
        assert_eq!(call.state(), state, "an incoming call changes nothing");
    }
    assert_eq!(
        call.ignored_incoming(),
        4,
        "counted while the call ran; ignored silently after it ended"
    );
}

#[test]
fn the_outgoing_call_follows_its_callbacks() {
    let mut call = OutgoingCall::new();
    assert_eq!(call.state(), CallState::Placing);
    assert_eq!(call.on_inbound(inbound("progress.json").unwrap()), Action::Nothing);
    assert_eq!(call.state(), CallState::Ringing);
    let Action::ApplyAnswer { sdp, acknowledgement } = call.on_inbound(inbound("media-answer.json").unwrap()) else {
        panic!("the answer is to be applied");
    };
    assert!(sdp.contains("m=audio") && acknowledgement.is_some());
    assert_eq!(call.state(), CallState::MediaAnswered);
    assert_eq!(call.on_inbound(inbound("acceptance.json").unwrap()), Action::Nothing);
    assert_eq!(call.state(), CallState::Connected);
    assert_eq!(call.on_inbound(inbound("end.json").unwrap()), Action::Hangup);
    assert_eq!(
        call.on_inbound(inbound("media-answer.json").unwrap()),
        Action::Nothing,
        "nothing after the end"
    );
}

async fn permitted_echo_call() -> TestResult<Authorized<StartCall>> {
    let fence = Fence::new(
        Roster::new([Mri::new(LAB_A)], [Mri::new(ECHO)]),
        FenceLog::from_writer(io::sink()),
    );
    let call = StartCall::new(Mri::new(LAB_A), [Mri::new(ECHO)]);
    let permit = fence.check(call.target(), &FailClosed).await?;
    Ok(Authorized::new(call, permit)?)
}

#[tokio::test]
async fn the_call_start_carries_the_permitted_participants_the_offer_and_the_callbacks() {
    let call = permitted_echo_call().await.unwrap();
    let me = Endpoint {
        display_name: "<user:a>".into(),
        endpoint_id: "<guid:1>".into(),
        participant_id: "<guid:2>".into(),
    };
    let body = call_start_body(&call, &me, "v=0\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\n", CALLBACK_BASE);

    assert_eq!(body["participants"]["from"]["id"], LAB_A);
    assert_eq!(body["participants"]["from"]["endpointId"], "<guid:1>");
    assert_eq!(body["participants"]["to"], serde_json::json!([{ "id": ECHO }]));
    let invitation = &body["callInvitation"];
    assert_eq!(invitation["callModalities"], serde_json::json!(["Audio"]));
    assert_eq!(invitation["mediaContent"]["contentType"], SDP_NGC);
    assert!(invitation["mediaContent"]["blob"].as_str().unwrap().contains("m=audio"));
    for callback in Callback::ALL {
        let link = invitation["links"][callback.link_name()].as_str().unwrap();
        assert_eq!(
            link,
            format!("https://callback.example.invalid/endpoint{}", callback.path())
        );
    }
    assert!(body["conversationRequest"]["links"]["conversationEnd"].is_string());
}

#[test]
fn a_relay_descriptor_gives_udp_servers_first_then_tcp() {
    let value = fixture("relay.json").unwrap();
    let relay = RelayDescriptor::from_json(&value["relay"]).unwrap();
    assert_eq!(relay.addresses.len(), 2, "a non-IP address is skipped");
    let servers: Vec<(Transport, SocketAddr)> = relay.servers().iter().map(|s| (s.transport, s.addr)).collect();
    assert_eq!(
        servers,
        [
            (Transport::Udp, "192.0.2.10:3478".parse().unwrap()),
            (Transport::Udp, "192.0.2.11:3478".parse().unwrap()),
            (Transport::Tcp, "192.0.2.10:443".parse().unwrap()),
            (Transport::Tcp, "192.0.2.11:443".parse().unwrap()),
        ]
    );

    let mut partial = value["relay"].clone();
    partial.as_object_mut().unwrap().remove("tcpPort");
    assert_eq!(RelayDescriptor::from_json(&partial), None, "all four fields or nothing");
}

#[test]
fn a_relay_token_gives_turn_credentials() {
    let value = fixture("relay.json").unwrap();
    let token = RelayToken::from_json(&value["token"]).unwrap();
    assert_eq!(token.credentials.username, "<turn-user>");
    assert_eq!(token.realm.as_deref(), Some("<turn-realm>"));
    assert_eq!(token.expires, Some(3600));
    assert!(
        !format!("{token:?}").contains("<turn-password>"),
        "Debug hides the password"
    );
    assert_eq!(RelayToken::from_json(&serde_json::json!({ "username": "u" })), None);
}
