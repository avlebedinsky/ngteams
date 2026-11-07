//! Caption actions and framed media through the real loopback engine, HTTP transport and mixer data channel.

use std::time::Duration;

use serde_json::{Value, json};
use teams_core::{CallAction, CallCaptions, CallId, CallStatus, CaptionPhase, End, Event};
use teams_media::mcp::{MIXER_RECIPIENT, Packetizer};
use teams_mock::{ABC_CHAT, LAB_A};
use teams_proto::captions::CAPTION_SERVICE_MRI;

use super::support::{A_TONE, Account, Lab, WAIT, jwt, wait_until};

const COMMAND: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/aks-prod-zz-api.callrecorder.teams.cloud.microsoft:21606/v2/oncommand/00000000-0000-4000-8000-000000000001";
const COMMAND_END: &str = "/oncommand/00000000-0000-4000-8000-000000000001";

async fn connected() -> (Lab, Account, CallId) {
    let lab = Lab::start_group_video().await;
    let oid = LAB_A.strip_prefix("8:orgid:").unwrap();
    assert!(
        lab.mock
            .set_token(LAB_A, jwt(&json!({"aud": teams_proto::IC3_AUDIENCE, "oid": oid})))
    );
    assert!(
        lab.mock
            .set_skypetoken(LAB_A, jwt(&json!({"skypeid": format!("orgid:{oid}")})))
    );
    let mut a = lab.account(LAB_A, A_TONE).await;
    let call = a
        .handle
        .requests()
        .call(CallAction::Call { chat: ABC_CHAT.into() })
        .await
        .unwrap();
    a.status(call, &CallStatus::Connected).await;
    let id = lab.last_call();
    wait_until(|| {
        lab.mock
            .mixer_stats(&id, LAB_A)
            .is_some_and(|stats| stats.handshakes > 0)
    })
    .await;
    (lab, a, call)
}

async fn callback(lab: &Lab, name: &str) -> String {
    fn find(value: &Value, name: &str) -> Option<String> {
        match value {
            Value::Object(fields) => fields.iter().find_map(|(key, value)| {
                if key == name {
                    value.as_str().map(str::to_owned)
                } else {
                    find(value, name)
                }
            }),
            Value::Array(values) => values.iter().find_map(|value| find(value, name)),
            _ => None,
        }
    }
    let url = lab
        .mock
        .received_requests()
        .await
        .iter()
        .find_map(|request| {
            serde_json::from_slice::<Value>(&request.body)
                .ok()
                .and_then(|value| find(&value, name))
        })
        .expect("the actual call request carries the callback");
    let suffix = url.split_once("/v4/f/").unwrap().1;
    suffix.split_once('/').unwrap().1.to_owned()
}

async fn service(lab: &Lab, state: &str, version: u64) {
    let path = callback(lab, "rosterUpdate").await;
    let body = json!({"participants": {CAPTION_SERVICE_MRI: {
        "state": "active", "version": version, "endpoints": {"synthetic-recorder": {"endpointMetadata": {
            "commandUrl": COMMAND, "spokenLanguage": "en-us", "processingModes": {"closedCaptions": {"state": state}}
        }}}
    }}});
    let _ = lab.trouter.deliver_call(LAB_A, &path, &body);
}

async fn phase(account: &mut Account, call: CallId, wanted: CaptionPhase) -> CallCaptions {
    account
        .next(|event| match event {
            Event::CallCaptions { call: of, captions } if *of == call && captions.phase == wanted => {
                Some(captions.clone())
            }
            _ => None,
        })
        .await
}

async fn enable(lab: &Lab, account: &mut Account, call: CallId) {
    lab.mock.answer_next("POST", "/addParticipant", 200, &json!({})).await;
    lab.mock.answer_next("POST", COMMAND_END, 200, &json!({})).await;
    account
        .handle
        .requests()
        .call(CallAction::Captions { call, enabled: true })
        .await
        .unwrap();
    phase(account, call, CaptionPhase::Starting).await;
    lab.wait_attempts("addParticipant", 1).await;
    service(lab, "Inactive", 1).await;
    lab.wait_attempts("00000000-0000-4000-8000-000000000001", 1).await;
    service(lab, "Active", 2).await;
    phase(account, call, CaptionPhase::On).await;
}

async fn language(account: &Account, call: CallId, value: &str) {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        match account
            .handle
            .requests()
            .call(CallAction::CaptionLanguage {
                call,
                language: value.into(),
            })
            .await
        {
            Ok(_) => return,
            Err(teams_core::EngineError::Call(teams_core::CallActionError::Call(teams_core::CallError::NotNow))) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "the previous caption write settled"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(error) => panic!("caption language action failed: {error}"),
        }
    }
}

fn packet(packets: &mut Packetizer, text: &str, final_result: bool) -> Vec<u8> {
    packets.packetize(
        127,
        &[MIXER_RECIPIENT],
        json!({
            "version": "1.1", "recognitionResults": [{
                "id": if final_result { "synthetic/2" } else { "synthetic/1" },
                "userId": LAB_A, "displayName": "Synthetic speaker", "text": text, "isFinal": final_result,
                "spokenLanguage": "en-us", "trackLanguage": "en-us", "timestampAudioSent": 123, "duration": 100
            }]
        })
        .to_string()
        .as_bytes(),
    )
}

async fn language_failure_and_retry(lab: &Lab, account: &mut Account, call: CallId) {
    lab.mock.answer_next("POST", COMMAND_END, 403, &json!({})).await;
    language(account, call, "en-gb").await;
    let failed = account
        .next(|event| match event {
            Event::CallCaptions { call: of, captions } if *of == call && captions.error.is_some() => {
                Some(captions.clone())
            }
            _ => None,
        })
        .await;
    assert_eq!(
        failed.phase,
        CaptionPhase::On,
        "a language failure never stops the voice call or caption view"
    );
    lab.mock.answer_next("POST", COMMAND_END, 200, &json!({})).await;
    language(account, call, "en-gb").await;
    lab.wait_attempts("00000000-0000-4000-8000-000000000001", 3).await;
    let commands: Vec<Value> = lab
        .mock
        .received_requests()
        .await
        .iter()
        .filter(|request| request.url.path().ends_with(COMMAND_END))
        .map(|request| serde_json::from_slice(&request.body).unwrap())
        .collect();
    assert_eq!(commands[0]["action"], "start");
    assert_eq!(commands[1]["actionParameters"]["spokenLanguage"], "en-gb");
    assert_eq!(commands[2]["action"], "setLanguage");
}

async fn service_restart(lab: &Lab, account: &mut Account, call: CallId) {
    service(lab, "Inactive", 3).await;
    let failed = phase(account, call, CaptionPhase::Failed).await;
    assert!(failed.error.unwrap().contains("service stopped"));
    account
        .handle
        .requests()
        .call(CallAction::Mute { call, muted: true })
        .await
        .unwrap();
    lab.mock.answer_next("POST", COMMAND_END, 200, &json!({})).await;
    account
        .handle
        .requests()
        .call(CallAction::Captions { call, enabled: true })
        .await
        .unwrap();
    phase(account, call, CaptionPhase::Starting).await;
    lab.wait_attempts("00000000-0000-4000-8000-000000000001", 4).await;
    service(lab, "Active", 4).await;
    assert!(phase(account, call, CaptionPhase::On).await.lines.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn caption_toggle_http_media_receive_language_hide_and_call_end_are_one_native_lifecycle() {
    let (lab, mut a, call) = connected().await;
    enable(&lab, &mut a, call).await;
    let id = lab.last_call();
    let mut packets = Packetizer::new(3);
    for (text, final_result) in [("Synthetic interim", false), ("Synthetic caption sentence.", true)] {
        assert!(
            lab.mock
                .send_call_data(&id, LAB_A, packet(&mut packets, text, final_result))
        );
        let seen = a
            .next(|event| match event {
                Event::CallCaptions { call: of, captions }
                    if *of == call && captions.lines.last().is_some_and(|line| line.text == text) =>
                {
                    Some(captions.clone())
                }
                _ => None,
            })
            .await;
        assert_eq!(seen.lines.len(), 1);
        assert_eq!(seen.lines[0].final_result, final_result);
        assert!(!format!("{seen:?}").contains(text));
    }
    assert!(
        a.handle
            .requests()
            .search("Synthetic caption sentence", 10)
            .await
            .unwrap()
            .is_empty()
    );
    language_failure_and_retry(&lab, &mut a, call).await;
    service_restart(&lab, &mut a, call).await;
    a.handle
        .requests()
        .call(CallAction::Captions { call, enabled: false })
        .await
        .unwrap();
    assert!(phase(&mut a, call, CaptionPhase::Off).await.lines.is_empty());
    a.handle
        .requests()
        .call(CallAction::Captions { call, enabled: true })
        .await
        .unwrap();
    phase(&mut a, call, CaptionPhase::On).await;
    a.handle.requests().call(CallAction::HangUp { call }).await.unwrap();
    let reset = phase(&mut a, call, CaptionPhase::Off).await;
    assert!(reset.lines.is_empty());
    assert_eq!(a.ended(call).await, End::HungUp);
    assert!(
        a.handle
            .requests()
            .call(CallAction::Captions { call, enabled: true })
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refused_caption_invitation_does_not_end_the_connected_voice_call() {
    let (lab, mut a, call) = connected().await;
    lab.mock.refuse_next("POST", "/addParticipant", 403).await;
    a.handle
        .requests()
        .call(CallAction::Captions { call, enabled: true })
        .await
        .unwrap();
    let failed = phase(&mut a, call, CaptionPhase::Failed).await;
    assert!(failed.error.unwrap().contains("not allowed"));
    a.handle
        .requests()
        .call(CallAction::Mute { call, muted: true })
        .await
        .unwrap();
    a.handle.requests().call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(a.ended(call).await, End::HungUp);
    assert_eq!(lab.attempts("addParticipant").await, 1);
}

/// A busy caption service fails the invitation as busy; asked again, the invitation goes, but a start command the
/// service fails (`500`) fails the captions with the connection's hint, and the voice call goes on either way.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_busy_invitation_and_a_failed_start_command_fail_captions_with_why() {
    let (lab, mut a, call) = connected().await;
    lab.mock.refuse_next("POST", "/addParticipant", 503).await;
    let captions = |enabled| CallAction::Captions { call, enabled };
    a.handle.requests().call(captions(true)).await.unwrap();
    let failed = phase(&mut a, call, CaptionPhase::Failed).await;
    assert_eq!(
        failed.error.as_deref(),
        Some("The live caption service is busy. Try again.")
    );

    lab.mock.answer_next("POST", "/addParticipant", 200, &json!({})).await;
    lab.mock.refuse_next("POST", COMMAND_END, 500).await;
    a.handle.requests().call(captions(true)).await.unwrap();
    phase(&mut a, call, CaptionPhase::Starting).await;
    service(&lab, "Inactive", 1).await;
    let failed = phase(&mut a, call, CaptionPhase::Failed).await;
    assert_eq!(
        failed.error.as_deref(),
        Some("Live captions could not start. Check your connection and try again.")
    );
    assert_eq!(lab.attempts("00000000-0000-4000-8000-000000000001").await, 1);

    a.handle
        .requests()
        .call(CallAction::Mute { call, muted: true })
        .await
        .unwrap();
    a.handle.requests().call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(a.ended(call).await, End::HungUp);
}
