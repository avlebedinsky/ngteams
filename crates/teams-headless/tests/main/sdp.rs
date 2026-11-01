//! A malformed synthetic SDP runs through the real native parser, fenced callback delivery and diagnostic sinks.

#![cfg(test)]

use super::*;
use log::{Level, Record};
use serde_json::{Value, json};
use teams_core::{CallAction, CallStatus, End, Event};
use teams_fence::Mri;
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_media::sdp::{NgcSdp, NgcSession, SDP_NGC};
use teams_mock::{ECHO, LAB_A};

const CREDENTIAL: &str = "SyntheticIceCredentialMustNeverReachDiagnostics";

fn malformed() -> String {
    format!(
        "v=0\r\no=- a=ice-pwd:{CREDENTIAL}\r\ns=-\r\nt=0 0\r\nm=audio 3478 RTP/SAVP 111\r\nc=IN IP4 198.51.100.20\r\na=mid:0\r\na=rtpmap:111 opus/48000/2\r\na=setup:actpass\r\n"
    )
}

async fn acceptance_callback(lab: &TestLab) -> String {
    let from_http = lab
        .mock
        .received_requests()
        .await
        .iter()
        .filter_map(|request| serde_json::from_slice::<Value>(&request.body).ok())
        .collect::<Vec<_>>();
    let from_socket = lab
        .trouter
        .sockets()
        .into_iter()
        .filter(|socket| socket.calling && socket.account.as_deref() == Some(LAB_A))
        .flat_map(|socket| socket.frames)
        .filter_map(|frame| {
            frame
                .strip_prefix("3:::")
                .and_then(|frame| serde_json::from_str::<Value>(frame).ok())
        })
        .filter_map(|reply| {
            reply["body"]
                .as_str()
                .and_then(|body| serde_json::from_str::<Value>(body).ok())
        })
        .collect::<Vec<_>>();
    from_http
        .iter()
        .chain(&from_socket)
        .find_map(|body| {
            body.pointer("/callAcceptanceAcknowledgement/links/mediaRenegotiation")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .expect("the accepted call hands out its own media callback")
}

async fn ended(handle: &mut EngineHandle, call: teams_core::CallId) -> End {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(Event::Call {
                call: of,
                status: CallStatus::Ended(end),
            }) = handle.next_event().await
                && of == call
            {
                return end;
            }
        }
    })
    .await
    .unwrap()
}

async fn connected(lab: &TestLab) -> (EngineHandle, teams_core::CallId) {
    let devices = FileDevices {
        input: AudioInput::Silence,
        recording: None,
        camera: PatternCamera::default(),
        pacing: Pacing::RealTime,
    };
    let mut handle = lab.start_calling(devices, false, None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !matches!(
            handle.next_event().await,
            Some(Event::CallingRegistration(teams_core::RegistrationReport::Registered))
        ) {}
    })
    .await
    .unwrap();
    let call = handle
        .call(CallAction::TestCall { echo: Mri::new(ECHO) })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !matches!(handle.next_event().await,Some(Event::Call {call: of,status:CallStatus::Connected}) if of==call)
        {
        }
    })
    .await
    .unwrap();
    (handle, call)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_remote_sdp_is_rejected_without_copying_its_credential_into_diagnostics() {
    let blob = malformed();
    let ngc = NgcSdp::new(SDP_NGC, &blob).unwrap();
    let parser_error = NgcSession::new().read_offer(&ngc).unwrap_err().to_string();
    assert!(parser_error.contains("libwebrtc refused"), "{parser_error}");
    let lab = TestLab::new().await;
    let (mut handle, call) = connected(&lab).await;
    let callback = acceptance_callback(&lab).await;
    let callback = callback_path(&callback);
    let fixture: Value =
        serde_json::from_str(include_str!("../../../../tests/fixtures/calling/chat/follow_ups.json")).unwrap();
    let call_id = lab.mock.calls().last().unwrap().id.clone();
    let mut body: Value = serde_json::from_str(
        &fixture["deliveries"][0]["body"]
            .to_string()
            .replace("SyntheticCall1/27", &format!("{call_id}/1")),
    )
    .unwrap();
    body["mediaNegotiation"]["mediaContent"]["blob"] = json!(blob);
    let before = support::diagnostic_lines().len();
    let _delivery = lab.trouter.deliver_call(LAB_A, &callback, &body);
    assert_eq!(ended(&mut handle, call).await, End::Failed("media"));
    handle.shutdown();
    let line = support::diagnostic_lines()
        .into_iter()
        .skip(before)
        .find(|line| line.contains("its media failed:") && line.contains("libwebrtc refused"))
        .expect("the actual core logs the parser failure");
    let persistent = teams_log::format_line(
        std::time::SystemTime::UNIX_EPOCH,
        Level::Warn,
        "teams_core::calling",
        &line,
    );
    assert!(
        !persistent.contains(CREDENTIAL),
        "persistent logger already redacts ICE"
    );
    let stderr = teams_headless::debug_log::line(
        7,
        &Record::builder()
            .level(Level::Warn)
            .target("teams_core::calling")
            .args(format_args!("{line}"))
            .build(),
    );
    assert!(
        !line.contains(CREDENTIAL),
        "core SDP failure copied a credential: {line}"
    );
    assert!(
        !stderr.contains(CREDENTIAL),
        "headless stderr copied a credential: {stderr}"
    );
    assert!(!parser_error.contains(CREDENTIAL));
    assert!(
        line.contains("invalid SDP origin line"),
        "structural context remains: {line}"
    );
}

fn callback_path(callback: &str) -> String {
    // The calling endpoint's path is transport-owned; deliver_call takes the callback relative to it.
    format!("callAgent/{}", callback.split_once("/callAgent/").unwrap().1)
}
