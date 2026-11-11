//! What a meeting shares with everyone in it (its timer, its lock, how to join it, a recording or a transcription someone
//! runs) told from its conversation's updates and its roster, and the account's timer and lock sent, on synthetic
//! deliveries.

use super::*;
use teams_proto::Request as _;
use teams_proto::{MeetingTimer, TimerState};

const FIXTURE: &str = include_str!("../../../../tests/fixtures/calling/meeting/states.json");

/// The fixture, its placeholders filled in: the recording bot, a, and a made-up meeting code, join key and thread.
fn fixture() -> Value {
    let filled = FIXTURE
        .replace("<mri:recorder>", teams_proto::captions::CAPTION_SERVICE_MRI)
        .replace("<mri:a>", A)
        .replace("<code:1>", "1112223334445")
        .replace("<key:1>", "SyntheticJoinKey01")
        .replace("<thread:1>", "19%3ameeting_SyntheticMeeting0001%40thread.v2");
    serde_json::from_str(&filled).unwrap()
}

/// a's meeting call, its start answered with the conversation's meeting links and its acceptance delivered: up.
fn up(now: Instant) -> (Calls, CallId) {
    let mut calls = Calls::new(identity(A));
    calls.set_push_url(PUSH).unwrap();
    let placement = Placement::Chat {
        callees: vec![Mri::new(B)],
        group_chat: None,
    };
    let call = calls.place(&placement, media(), now).unwrap();
    let placed = effects(&mut calls);
    let [(start, "start")] = sent(&placed)[..] else {
        panic!("{placed:#?}");
    };
    let mut answer = conversation_answer();
    for op in ["updateMeetingStates", "updateMeetingSettings"] {
        answer["links"][op] = json!(conv(&format!("/{op}")));
    }
    calls.on_outcome(start, &ok(&answer), now);
    let path = acceptance_path(&calls);
    calls.on_delivery(&delivery(&path, &acceptance()), Observed::Acceptance, now);
    let _ = effects(&mut calls);
    (calls, call)
}

fn callback(calls: &Calls, name: &str) -> String {
    calls
        .call
        .as_ref()
        .and_then(|call| call.path_of(Kind::Conversation, name))
        .unwrap()
        .to_owned()
}

/// The writes among `effects`, by name, with their bodies.
fn meeting_writes(effects: &[Effect]) -> Vec<(WriteId, &'static str, Value)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Send { id, write } => {
                let body = match write.as_ref() {
                    CallWrite::MeetingStates(write) => write.body(),
                    CallWrite::MeetingSettings(write) => write.body(),
                    _ => None,
                };
                Some((*id, write.name(), body.unwrap_or(Value::Null)))
            }
            _ => None,
        })
        .collect()
}

fn meeting_told(effects: &[Effect]) -> Vec<crate::MeetingState> {
    events(effects)
        .into_iter()
        .filter_map(|event| match event {
            CallEvent::Meeting { state, .. } => Some(state),
            _ => None,
        })
        .collect()
}

#[test]
fn a_conversation_update_tells_the_timer_the_lock_and_how_to_join_once_each_changed() {
    let now = Instant::now();
    let (mut calls, call) = up(now);
    let updates = callback(&calls, "conversationUpdate");
    let update = fixture()["conversation_update"].clone();
    calls.on_delivery(&delivery(&updates, &update), Observed::Nothing, now);
    let told = meeting_told(&effects(&mut calls));
    let [state] = &told[..] else { panic!("{told:#?}") };
    assert!(state.locked);
    assert_eq!(
        state.timer.as_ref().map(|timer| timer.state),
        Some(TimerState::InProgress)
    );
    assert_eq!(
        state.join.as_ref().and_then(|join| join.passcode.as_deref()),
        Some("Synth3ic")
    );
    // the same again tells nothing; an older timer changes nothing
    calls.on_delivery(&delivery(&updates, &update), Observed::Nothing, now);
    let mut older = update.clone();
    older["meetingStates"]["timer"]["sequenceNumber"] = json!(1);
    older["meetingStates"]["timer"]["value"]["data"] =
        json!(r#"{"adhocTimer":{"state":"Paused","remainingDuration":5}}"#);
    calls.on_delivery(&delivery(&updates, &older), Observed::Nothing, now);
    assert!(meeting_told(&effects(&mut calls)).is_empty());
    // a cancelled timer shows none; the lock comes off
    let mut cancelled = json!({"meetingDetails": {"meetingCapability": {"lockMeeting": false}}});
    cancelled["meetingStates"]["timer"] = json!({"sequenceNumber": 4, "value": {"data":
        r#"{"adhocTimer":{"state":"Cancelled","remainingDuration":300000,"sections":[]}}"#}});
    calls.on_delivery(&delivery(&updates, &cancelled), Observed::Nothing, now);
    let told = meeting_told(&effects(&mut calls));
    let [state] = &told[..] else { panic!("{told:#?}") };
    assert!(!state.locked && state.timer.is_none() && state.join.is_some());
    let _ = call;
}

#[test]
fn the_recording_bot_is_told_as_a_recording_never_as_a_person() {
    let now = Instant::now();
    let (mut calls, call) = up(now);
    let roster = callback(&calls, "rosterUpdate");
    calls.on_delivery(
        &delivery(&roster, &fixture()["recording_roster"]),
        Observed::Nothing,
        now,
    );
    let effects_now = effects(&mut calls);
    assert!(
        !events(&effects_now)
            .iter()
            .any(|event| matches!(event, CallEvent::Participants { participants, .. } if !participants.is_empty())),
        "the bot is nobody in the call"
    );
    let told = meeting_told(&effects_now);
    assert_eq!(told.len(), 1);
    assert_eq!(
        told[0].recording,
        Some(crate::Recording {
            by: Some(Mri::new(A)),
            audio_only: false,
        })
    );
    assert_eq!(told[0].transcribing, Some(crate::Transcription { by: None }));
    calls.on_delivery(
        &delivery(&roster, &fixture()["recording_stopped_roster"]),
        Observed::Nothing,
        now,
    );
    let told = meeting_told(&effects(&mut calls));
    assert_eq!(told.len(), 1);
    assert_eq!(told[0].recording, None);
    assert_eq!(told[0].transcribing, None);
    let _ = call;
}

#[test]
fn another_region_keeps_recording_when_the_invited_leg_leaves_before_caption_metadata_arrives() {
    use teams_proto::captions::{CAPTION_SERVICE_ALTERNATE_MRI, CAPTION_SERVICE_MRI};
    let now = Instant::now();
    let (mut calls, call) = up(now);
    let roster = callback(&calls, "rosterUpdate");
    // This recording-only fixture carries no caption state or command URL.
    let active = json!({"participants": {
        CAPTION_SERVICE_ALTERNATE_MRI: fixture()["recording_roster"]["participants"][CAPTION_SERVICE_MRI]
    }});
    calls.on_delivery(&delivery(&roster, &active), Observed::Nothing, now);
    assert!(calls.caption_service(call).is_none());
    let told = meeting_told(&effects(&mut calls));
    let expected = calls.call.as_ref().unwrap().meeting.state.clone();
    assert_eq!(told, vec![expected.clone()]);
    assert!(expected.recording.is_some() && expected.transcribing.is_some());
    for (version, state) in [(22, "disconnected"), (23, "connecting"), (24, "active")] {
        let other = json!({"participants": {CAPTION_SERVICE_MRI: {"version": version, "state": state}}});
        calls.on_delivery(&delivery(&roster, &other), Observed::Nothing, now);
        assert_eq!(calls.call.as_ref().unwrap().meeting.state, expected);
        assert!(meeting_told(&effects(&mut calls)).is_empty());
    }
    let stopped = json!({"participants": {
        CAPTION_SERVICE_ALTERNATE_MRI: fixture()["recording_stopped_roster"]["participants"][CAPTION_SERVICE_MRI]
    }});
    calls.on_delivery(&delivery(&roster, &stopped), Observed::Nothing, now);
    let told = meeting_told(&effects(&mut calls));
    assert_eq!(told.len(), 1);
    assert!(told[0].recording.is_none() && told[0].transcribing.is_none());
}

#[test]
fn a_transcription_alone_an_audio_recording_and_their_stop_are_told_by_their_state() {
    let now = Instant::now();
    let (mut calls, _) = up(now);
    let roster = callback(&calls, "rosterUpdate");
    let mut told_after = |name: &str| {
        calls.on_delivery(&delivery(&roster, &fixture()[name]), Observed::Nothing, now);
        let told = meeting_told(&effects(&mut calls));
        let [state] = &told[..] else {
            panic!("{name}: {told:#?}")
        };
        state.clone()
    };
    let transcribing = Some(crate::Transcription { by: Some(Mri::new(A)) });
    let state = told_after("transcribing_roster");
    assert_eq!(
        (state.recording, state.transcribing.clone()),
        (None, transcribing.clone())
    );
    let state = told_after("audio_recording_roster");
    assert_eq!(
        state.recording,
        Some(crate::Recording {
            by: Some(Mri::new(A)),
            audio_only: true,
        })
    );
    assert_eq!(state.transcribing, transcribing);
    // the starter stays named once both stopped: their state says they don't run
    let state = told_after("transcription_stopped_roster");
    assert_eq!((state.recording, state.transcribing), (None, None));
}

#[test]
fn the_timer_goes_as_the_change_of_the_one_told_and_its_failure_ends_nothing() {
    let now = Instant::now();
    let (mut calls, call) = up(now);
    let start = MeetingTimer {
        state: TimerState::InProgress,
        remaining_ms: 120_000,
        original_ms: 120_000,
        edited_ms: None,
        previous: None,
    };
    calls.set_timer(call, &start).unwrap();
    let written = meeting_writes(&effects(&mut calls));
    let [(id, "update_meeting_states", body)] = &written[..] else {
        panic!("{written:#?}");
    };
    assert_eq!(body["meetingStates"]["timer"]["sequenceNumber"], json!(0));
    let status = body["updateMeetingStatesStatus"].as_str().unwrap();
    assert!(status.ends_with("/conversation/updateMeetingStatesStatus/"), "{status}");
    calls.on_outcome(*id, &failed(), now);
    assert!(
        !events(&effects(&mut calls))
            .iter()
            .any(|event| matches!(event, CallEvent::Ended { .. }))
    );
    // once a timer was told, the next change names its sequence
    let updates = callback(&calls, "conversationUpdate");
    calls.on_delivery(
        &delivery(&updates, &fixture()["conversation_update"]),
        Observed::Nothing,
        now,
    );
    let _ = effects(&mut calls);
    calls.set_timer(call, &start).unwrap();
    let written = meeting_writes(&effects(&mut calls));
    assert_eq!(written[0].2["meetingStates"]["timer"]["sequenceNumber"], json!(3));
}

#[test]
fn only_the_organizer_locks_the_meeting_each_change_counting_the_next() {
    let now = Instant::now();
    let (mut calls, call) = up(now);
    assert!(matches!(calls.lock_meeting(call, true), Err(CallError::NotNow)));
    let roster = callback(&calls, "rosterUpdate");
    let organizer = json!({"type": "Delta", "participants": {A: {"version": 3, "state": "active",
        "meetingRole": "organizer", "endpoints": {"e": {"call": {}}}}}});
    calls.on_delivery(&delivery(&roster, &organizer), Observed::Nothing, now);
    let _ = effects(&mut calls);
    calls.lock_meeting(call, true).unwrap();
    calls.lock_meeting(call, false).unwrap();
    let written = meeting_writes(&effects(&mut calls));
    let [
        (_, "update_meeting_settings", locked),
        (_, "update_meeting_settings", unlocked),
    ] = &written[..]
    else {
        panic!("{written:#?}");
    };
    assert_eq!(locked["meetingSettings"]["lockMeeting"], json!(true));
    assert_eq!(locked["sequenceNumber"], json!(1));
    assert_eq!(unlocked["meetingSettings"]["lockMeeting"], json!(false));
    assert_eq!(unlocked["sequenceNumber"], json!(2));
    assert!(matches!(
        calls.lock_meeting(CallId(99), true),
        Err(CallError::NoSuchCall)
    ));
}

#[test]
fn a_call_whose_conversation_named_no_meeting_links_sends_neither() {
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    let timer = MeetingTimer {
        state: TimerState::Cancelled,
        remaining_ms: 0,
        original_ms: 0,
        edited_ms: None,
        previous: None,
    };
    assert!(matches!(calls.set_timer(call, &timer), Err(CallError::NotNow)));
    assert!(matches!(calls.lock_meeting(call, true), Err(CallError::NotNow)));
    let (mut ringing, call) = ringing_call(now);
    assert!(matches!(ringing.set_timer(call, &timer), Err(CallError::NotNow)));
}
