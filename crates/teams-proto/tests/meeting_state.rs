//! A meeting's shared states and settings (`docs/protocol/calling.md`, "More of a running call"): the timer's and the
//! lock's writes in their captured shapes, and the timer, the lock and the join info read from a conversation update.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use serde_json::{Value, json};
use teams_proto::{
    CallContext, CallLink, CallParticipant, CallWriteError, MeetingJoinInfo, MeetingTimer, Mri, MuteParticipants,
    TimerState, UpdateMeetingSettings, UpdateMeetingStates,
};

const LAB_A: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const PROXY: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep";
const STATUS: &str = "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint00000/callAgent/00000000-0000-4000-8000-0000000000a1/01010101/conversation/updateMeetingStatesStatus/";
const FIXTURE: &str = include_str!("../../../tests/fixtures/calling/meeting/states.json");

fn link(op: &str) -> CallLink {
    let url = format!("{PROXY}/conv-zz-01-prod-aks.conv.skype.com/conv/Synthetic0/{op}?i=0-0-0-0&e=1");
    CallLink::parse(&url).unwrap_or_else(|| panic!("not a call link: {url}"))
}

fn participant() -> CallParticipant {
    CallParticipant {
        mri: Mri::new(LAB_A),
        display_name: Some("Lab".into()),
        endpoint_id: "00000000-0000-4000-8000-0000000000e1".into(),
        participant_id: Some("00000000-0000-4000-8000-0000000000e2".into()),
        language_id: Some("en-us".into()),
    }
}

fn context() -> CallContext {
    CallContext::new("00000000-0000-4000-8000-0000000000c1").with_region(Some("amer".into()), Some("amer01".into()))
}

/// The fixture, its placeholders filled in: a made-up meeting code, join key and thread.
fn fixture() -> Value {
    let filled = FIXTURE
        .replace("<code:1>", "1112223334445")
        .replace("<key:1>", "SyntheticJoinKey01")
        .replace("<thread:1>", "19%3ameeting_SyntheticMeeting0001%40thread.v2");
    serde_json::from_str(&filled).unwrap_or_else(|error| panic!("the fixture doesn't read: {error}"))
}

fn timer(state: TimerState, remaining_ms: u64, previous: Option<TimerState>) -> MeetingTimer {
    MeetingTimer {
        state,
        remaining_ms,
        original_ms: 300_000,
        edited_ms: None,
        previous,
    }
}

#[test]
fn a_timer_started_goes_as_the_web_client_s_first_start() {
    let started = timer(TimerState::InProgress, 300_000, None);
    let write = UpdateMeetingStates::timer(
        &context(),
        &link("updateMeetingStates"),
        &participant(),
        (&started, 0, None),
        STATUS,
    )
    .unwrap();
    let body = teams_proto::Request::body(&write).unwrap();
    assert_eq!(body["from"]["id"], LAB_A);
    assert_eq!(body["meetingStates"]["timer"]["sequenceNumber"], json!(0));
    assert_eq!(body["updateMeetingStatesStatus"], STATUS);
    assert_eq!(body["operationId"].as_str().unwrap().len(), 36);
    let data: Value = serde_json::from_str(body["meetingStates"]["timer"]["value"]["data"].as_str().unwrap()).unwrap();
    assert_eq!(
        data["adhocTimer"],
        json!({"state": "InProgress", "remainingDuration": 300_000, "sections": [{"title": "", "originalDuration": 300_000}],
            "pendingConsent": false, "consentRequested": false})
    );
    assert_eq!(data["agendaTimer"]["state"], "Cancelled");
    assert_eq!(data["agendaTimer"]["sections"], json!([]));
    assert_eq!(data["rosTimer"]["sections"][0]["originalDuration"], json!(300_000));
}

#[test]
fn a_timer_change_names_the_sequence_told_and_sends_its_other_timers_back_as_told() {
    let told = MeetingTimer::read(&fixture()["conversation_update"]["meetingStates"]["timer"]).unwrap();
    assert_eq!(told.sequence, 3);
    let reset = timer(TimerState::Paused, 300_000, Some(TimerState::InProgress));
    let write = UpdateMeetingStates::timer(
        &context(),
        &link("updateMeetingStates"),
        &participant(),
        (&reset, told.sequence, Some(&told.data)),
        STATUS,
    )
    .unwrap();
    let body = teams_proto::Request::body(&write).unwrap();
    assert_eq!(body["meetingStates"]["timer"]["sequenceNumber"], json!(3));
    let data: Value = serde_json::from_str(body["meetingStates"]["timer"]["value"]["data"].as_str().unwrap()).unwrap();
    assert_eq!(data["adhocTimer"]["state"], "Paused");
    assert_eq!(data["adhocTimer"]["previousState"], "InProgress");
    let told: Value = serde_json::from_str(&told.data).unwrap();
    assert_eq!(data["agendaTimer"], told["agendaTimer"]);
    assert_eq!(data["rosTimer"], told["rosTimer"]);
}

#[test]
fn the_lock_goes_as_its_captured_put() {
    let write =
        UpdateMeetingSettings::lock(&context(), &link("updateMeetingSettings"), &participant(), true, 1).unwrap();
    let body = teams_proto::Request::body(&write).unwrap();
    assert_eq!(body["meetingSettings"], json!({"lockMeeting": true}));
    assert_eq!(body["sequenceNumber"], json!(1));
    assert_eq!(body["from"]["id"], LAB_A);
    // each goes only to its own link
    assert!(matches!(
        UpdateMeetingSettings::lock(&context(), &link("updateMeetingStates"), &participant(), false, 2),
        Err(CallWriteError::Unmapped)
    ));
    assert!(matches!(
        UpdateMeetingStates::timer(
            &context(),
            &link("updateMeetingSettings"),
            &participant(),
            (&timer(TimerState::Cancelled, 0, None), 0, None),
            STATUS
        ),
        Err(CallWriteError::Unmapped)
    ));
}

#[test]
fn mute_all_goes_as_its_captured_post() {
    const OPERATION: &str = "00000000-0000-4000-8000-0000000000f1";
    let callback = format!(
        "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint00000/callAgent/\
         00000000-0000-4000-8000-0000000000a1/{OPERATION}/conversation/muteUnmuteAsync/"
    );
    let write = MuteParticipants::everyone(&context(), &link("mute"), &participant(), &callback, OPERATION).unwrap();
    assert_eq!(teams_proto::Request::method(&write), teams_proto::Method::Post);
    let body = teams_proto::Request::body(&write).unwrap();
    assert_eq!(body["from"]["id"], LAB_A);
    assert_eq!(body["scope"], "all");
    assert_eq!(body["muteParticipants"], json!([]));
    assert_eq!(body["mediaTypes"], json!(["audio"]));
    assert_eq!(body["links"], json!({"clientCallback": callback}));
    assert_eq!(body["operationId"], OPERATION);
    // only at its own link, its callback named by its operation, the operation a GUID
    for (link_to, callback_at, operation) in [
        (link("leave"), callback.as_str(), OPERATION),
        (
            link("mute"),
            "https://example.invalid/conversation/muteUnmuteAsync/",
            OPERATION,
        ),
        (link("mute"), callback.as_str(), "not-a-guid"),
    ] {
        assert!(matches!(
            MuteParticipants::everyone(&context(), &link_to, &participant(), callback_at, operation),
            Err(CallWriteError::Unmapped)
        ));
    }
}

#[test]
fn a_conversation_update_tells_the_timer_and_the_lock() {
    let update = &fixture()["conversation_update"];
    let told = MeetingTimer::read(&update["meetingStates"]["timer"]).unwrap();
    assert_eq!(told.timer.state, TimerState::InProgress);
    assert_eq!(told.timer.remaining_ms, 300_000);
    assert_eq!(told.timer.original_ms, 300_000);
    let edited = told.timer.edited_ms.unwrap();
    // in progress, it counts down from its last edit; paused, it stays
    assert_eq!(told.timer.left_at(edited + 61_000), 239_000);
    assert_eq!(told.timer.left_at(edited + 400_000), 0);
    assert_eq!(timer(TimerState::Paused, 120_000, None).left_at(edited), 120_000);
    assert_eq!(teams_proto::meeting_locked(update), Some(true));
    assert_eq!(teams_proto::meeting_locked(&json!({"meetingDetails": {}})), None);
}

#[test]
fn a_conversation_update_tells_how_to_join_as_its_invitation_says() {
    let update = &fixture()["conversation_update"];
    let join = MeetingJoinInfo::read(update).unwrap();
    assert_eq!(
        join.join_link.as_deref(),
        Some("https://teams.microsoft.com/meet/1112223334445?p=SyntheticJoinKey01")
    );
    assert_eq!(join.meeting_id.as_deref(), Some("111 222 333 444 5"));
    assert_eq!(join.passcode.as_deref(), Some("Synth3ic"));
    assert_eq!(
        join.help_link.as_deref(),
        Some("https://aka.ms/JoinTeamsMeeting?omkt=en-US")
    );
    assert!(
        join.reference_link
            .unwrap()
            .starts_with("https://teams.microsoft.com/l/meetup-join/")
    );
    assert_eq!(
        join_text(&MeetingJoinInfo::read(update).unwrap()),
        "Microsoft Teams meeting\nJoin: https://teams.microsoft.com/meet/1112223334445?p=SyntheticJoinKey01\n\
         Meeting ID: 111 222 333 444 5\nPasscode: Synth3ic"
    );
    assert_eq!(
        MeetingJoinInfo::read(&json!({"meetingDetails": {"invitation": "plain"}})),
        None
    );
}

fn join_text(info: &MeetingJoinInfo) -> String {
    info.text()
}
