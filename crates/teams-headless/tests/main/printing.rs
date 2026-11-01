//! Print the call events a command can receive, preserving metadata while escaping terminal controls and names.

#![cfg(test)]

use super::*;
use teams_core::{CallAction, CallId, CallParticipant, CallStatus, CallView, End, Event, IncomingCall, RemoteEnd};
use teams_fence::Mri;
use teams_headless::print::{call_status_word, end_word, event_json, event_line, is_terminal_control};
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_mock::ECHO;

const HOSTILE: &str = "private name\u{1b}[2J\rforged";

async fn call_id(lab: &TestLab) -> CallId {
    let devices = FileDevices {
        input: AudioInput::Silence,
        recording: None,
        camera: PatternCamera::default(),
        pacing: Pacing::RealTime,
    };
    let mut handle = lab.start_calling(devices, false, None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if matches!(
                handle.next_event().await,
                Some(Event::CallingRegistration(teams_core::RegistrationReport::Registered))
            ) {
                break;
            }
        }
    })
    .await
    .unwrap();
    let id = handle
        .call(CallAction::TestCall { echo: Mri::new(ECHO) })
        .await
        .unwrap();
    handle.shutdown();
    id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_call_status_and_event_prints_safe_metadata_in_json_and_text() {
    let lab = TestLab::new().await;
    let call = call_id(&lab).await;
    let events = [
        Event::CallingRegistration(teams_core::RegistrationReport::Failed(HOSTILE.to_owned())),
        Event::IncomingCall(IncomingCall {
            call,
            caller: Mri::new(LAB_B),
            caller_name: Some(HOSTILE.to_owned()),
            group_chat: Some("synthetic-group".to_owned()),
        }),
        Event::RingEnded {
            call,
            end: RemoteEnd {
                code: Some(487),
                phrase: Some(HOSTILE.to_owned()),
                ..RemoteEnd::default()
            },
        },
        Event::Voicemail { call },
        Event::CameraOff { call },
        Event::AudioClosed {
            call,
            microphone: false,
        },
        Event::CallParticipants {
            call,
            view: CallView {
                participants: vec![CallParticipant {
                    mri: LAB_B.to_owned(),
                    name: Some(HOSTILE.to_owned()),
                    video_on: true,
                    line: Some(2),
                    speaking: true,
                }],
                speaking: true,
                hearing: true,
            },
        },
    ];
    for event in events {
        let value = event_json(&event, 123, &lab.names);
        assert_eq!(value["received_ms"], 123);
        assert_ne!(value["event"], "other");
        let line = event_line(&event, 123, &lab.names);
        assert!(!line.chars().any(is_terminal_control), "{line:?}");
        if matches!(event, Event::IncomingCall(_) | Event::CallParticipants { .. }) {
            assert!(
                !line.contains("private name"),
                "roster and invitation display names are omitted"
            );
        }
    }
    for (status, expected) in [
        (CallStatus::Placing, "placing"),
        (CallStatus::Ringing, "ringing"),
        (CallStatus::Joining, "joining"),
        (CallStatus::Answering, "answering"),
        (CallStatus::Declining, "declining"),
        (CallStatus::Connecting, "connecting"),
        (CallStatus::Connected, "connected"),
        (CallStatus::Reconnecting, "reconnecting"),
        (CallStatus::Leaving, "leaving"),
        (CallStatus::Ended(End::HungUp), "ended"),
    ] {
        assert_eq!(call_status_word(&status), expected);
        let value = event_json(&Event::Call { call, status }, 123, &lab.names);
        assert_eq!(value["status"], expected);
    }
    for (end, expected) in [
        (End::HungUp, "hung_up"),
        (End::Declined, "declined"),
        (End::Failed("media"), "failed media"),
        (End::Remote(RemoteEnd::default()), "remote - -"),
    ] {
        assert_eq!(end_word(&end), expected);
    }
}
