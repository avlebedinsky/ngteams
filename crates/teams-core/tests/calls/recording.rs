//! A recording the account starts and stops through the real loopback engine and HTTP transport: the recording bot
//! invited, its start sent once the roster names its command link, its stop; a refusal told in Teams' words.

use serde_json::{Value, json};
use teams_core::{CallAction, CallId, CallRecording, CallStatus, End, Event, RecordingPhase};
use teams_mock::{ABC_CHAT, LAB_A};
use teams_proto::captions::CAPTION_SERVICE_MRI;

use super::support::{A_TONE, Account, Lab, jwt, wait_until};

const COMMAND: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/aks-prod-zz-api.callrecorder.teams.cloud.microsoft:21606/v2/oncommand/00000000-0000-4000-8000-000000000001";
const COMMAND_ID: &str = "00000000-0000-4000-8000-000000000001";
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

/// The path of the call's own `rosterUpdate` callback, as its start named it.
async fn roster_path(lab: &Lab) -> String {
    fn find(value: &Value) -> Option<String> {
        match value {
            Value::Object(fields) => fields.iter().find_map(|(key, value)| {
                if key == "rosterUpdate" {
                    value.as_str().map(str::to_owned)
                } else {
                    find(value)
                }
            }),
            Value::Array(values) => values.iter().find_map(find),
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
                .and_then(|body| find(&body))
        })
        .expect("the call's start names its roster callback");
    let suffix = url.split_once("/v4/f/").unwrap().1;
    suffix.split_once('/').unwrap().1.to_owned()
}

/// The recording bot in the roster, recording (`Active`) or not, at `version`.
async fn bot(lab: &Lab, recording: &str, version: u64) {
    let path = roster_path(lab).await;
    let body = json!({"participants": {CAPTION_SERVICE_MRI: {
        "state": "active", "version": version, "endpoints": {"synthetic-recorder": {"endpointMetadata": {
            "commandUrl": COMMAND, "spokenLanguage": "en-us", "processingModes": {
                "closedCaptions": {"state": "Inactive"},
                "recording": {"state": recording, "userId": if recording == "Active" { LAB_A } else { "" }}
            }
        }}}
    }}});
    let _ = lab.trouter.deliver_call(LAB_A, &path, &body);
}

async fn told(account: &mut Account, call: CallId, wanted: impl Fn(&CallRecording) -> bool) -> CallRecording {
    account
        .next(|event| match event {
            Event::CallRecording { call: of, recording } if *of == call && wanted(recording) => Some(*recording),
            _ => None,
        })
        .await
}

/// The roster told the call recording (`recording`) or not.
async fn meeting_recording(account: &mut Account, call: CallId, recording: bool) {
    account
        .next(|event| match event {
            Event::CallMeeting { call: of, state } if *of == call && state.recording.is_some() == recording => Some(()),
            _ => None,
        })
        .await;
}

fn start(call: CallId) -> CallAction {
    CallAction::Record {
        call,
        title: "Call with Lab Group".into(),
        organizer: None,
        language: "en-gb".into(),
    }
}

async fn commands(lab: &Lab) -> Vec<Value> {
    lab.mock
        .received_requests()
        .await
        .iter()
        .filter(|request| request.url.path().ends_with(COMMAND_END))
        .map(|request| serde_json::from_slice(&request.body).unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_invites_the_bot_then_records_and_stop_stops_it() {
    let (lab, mut a, call) = connected().await;
    lab.mock.answer_next("POST", "/addParticipant", 202, &json!({})).await;
    lab.mock
        .answer_next("POST", COMMAND_END, 200, &json!({"CorrelationId": "synthetic"}))
        .await;
    a.handle.requests().call(start(call)).await.unwrap();
    told(&mut a, call, |recording| recording.phase == RecordingPhase::Starting).await;
    lab.wait_attempts("addParticipant", 1).await;
    // the bot joins, not recording yet: the start goes to its command link
    bot(&lab, "Inactive", 1).await;
    lab.wait_attempts(COMMAND_ID, 1).await;
    let done = told(&mut a, call, |recording| recording.phase == RecordingPhase::Idle).await;
    assert_eq!(done.problem, None);
    let sent = commands(&lab).await;
    assert_eq!(sent[0]["action"], "start");
    assert_eq!(sent[0]["processingModes"], json!(["recording", "realTimeTranscript"]));
    assert_eq!(sent[0]["actionParameters"]["spokenLanguage"], "en-gb");
    assert!(
        sent[0]["actionParameters"]["recordingStorageSettings"][0]["FileName"]
            .as_str()
            .unwrap()
            .starts_with("Call with Lab Group-")
    );
    // the roster tells it records; Stop stops it
    bot(&lab, "Active", 2).await;
    lab.mock.answer_next("POST", COMMAND_END, 200, &json!({})).await;
    a.handle
        .requests()
        .call(CallAction::StopRecording { call })
        .await
        .unwrap();
    told(&mut a, call, |recording| recording.phase == RecordingPhase::Stopping).await;
    lab.wait_attempts(COMMAND_ID, 2).await;
    told(&mut a, call, |recording| recording.phase == RecordingPhase::Idle).await;
    assert_eq!(commands(&lab).await[1]["action"], "stop");
    assert_eq!(lab.attempts("addParticipant").await, 1, "the bot is invited once");
    a.handle.requests().call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(a.ended(call).await, End::HungUp);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_start_says_why_and_the_call_goes_on() {
    let (lab, mut a, call) = connected().await;
    // the bot is in the call already (someone recorded before): the start goes at once
    bot(&lab, "Active", 1).await;
    meeting_recording(&mut a, call, true).await;
    bot(&lab, "Inactive", 2).await;
    meeting_recording(&mut a, call, false).await;
    lab.mock.answer_next("POST", COMMAND_END, 403, &json!({})).await;
    a.handle.requests().call(start(call)).await.unwrap();
    let failed = told(&mut a, call, |recording| recording.problem.is_some()).await;
    assert_eq!(failed.phase, RecordingPhase::Idle);
    assert_eq!(
        failed.problem.unwrap().detail,
        "Recording not started. Your IT admin has disabled Recording."
    );
    assert_eq!(lab.attempts("addParticipant").await, 0);
    a.handle
        .requests()
        .call(CallAction::Mute { call, muted: true })
        .await
        .unwrap();
    a.handle.requests().call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(a.ended(call).await, End::HungUp);
}
