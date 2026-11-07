//! The organizer's controls (Mute all, whose meeting options Controls change) and what binds the account when someone
//! else organizes (Mute all's mute, being an attendee, the options the conversation tells), on synthetic deliveries.

use super::*;
use teams_proto::Request as _;

const GROUP: &str = "19:00000000000000000000000000010001@thread.v2";
const C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";

/// a's group call with b and c in `GROUP`, its start answered with the conversation's `mute` link (none without
/// `mute`) and its acceptance delivered: up.
fn group_up(now: Instant, mute: bool) -> (Calls, CallId) {
    group_up_as(identity(A), now, mute)
}

/// As [`group_up`], the account `account`.
fn group_up_as(account: Identity, now: Instant, mute: bool) -> (Calls, CallId) {
    let mut calls = Calls::new(account);
    calls.set_push_url(PUSH).unwrap();
    let placement = Placement::Chat {
        callees: vec![Mri::new(B), Mri::new(C)],
        group_chat: Some(GROUP.to_owned()),
    };
    let call = calls.place(&placement, media(), now).unwrap();
    let placed = effects(&mut calls);
    let [(start, "start")] = sent(&placed)[..] else {
        panic!("{placed:#?}");
    };
    let mut answer = conversation_answer();
    if mute {
        answer["links"]["mute"] = json!(conv("/mute"));
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

/// A roster entry in the call: `role` its meeting role, its main audio muted by the organizer at `muted`'s
/// version (if any).
fn entry(version: u64, role: &str, muted: Option<u64>) -> Value {
    json!({"version": version, "state": "active", "meetingRole": role,
        "details": {"displayName": "Someone"},
        "endpoints": {"e": {"call": {
            "mediaStreams": [{"type": "audio", "label": "main-audio", "sourceId": 1, "direction": "sendrecv",
                "serverMuted": muted.is_some()}],
            "serverMuteVersion": muted.unwrap_or(1)},
            "endpointState": {"endpointStateSequenceNumber": 1, "state": {"isMuted": false}}}}})
}

fn told(effects: &[Effect]) -> Vec<crate::MeetingState> {
    events(effects)
        .into_iter()
        .filter_map(|event| match event {
            CallEvent::Meeting { state, .. } => Some(state),
            _ => None,
        })
        .collect()
}

/// The account organizes the call, as its roster says.
fn organize(calls: &mut Calls, now: Instant) {
    let roster = callback(calls, "rosterUpdate");
    let update = json!({"type": "Delta", "participants": {A: entry(2, "organizer", None)}});
    calls.on_delivery(&delivery(&roster, &update), Observed::Nothing, now);
    let _ = effects(calls);
}

#[test]
fn only_the_organizer_mutes_everyone_else_as_the_web_client_asks_and_its_failure_ends_nothing() {
    let now = Instant::now();
    let (mut calls, call) = group_up(now, true);
    assert!(matches!(calls.mute_all(call), Err(CallError::NotNow)));
    organize(&mut calls, now);
    calls.mute_all(call).unwrap();
    let written = effects(&mut calls);
    let mutes: Vec<(WriteId, Value)> = written
        .iter()
        .filter_map(|effect| match effect {
            Effect::Send { id, write } => match write.as_ref() {
                CallWrite::MuteAll(mute) => Some((*id, mute.body().unwrap())),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let [(id, body)] = &mutes[..] else {
        panic!("{written:#?}");
    };
    assert_eq!(sent(&written), [(*id, "mute")]);
    assert_eq!(body["scope"], "all");
    assert_eq!(body["mediaTypes"], json!(["audio"]));
    assert_eq!(body["from"]["id"], A);
    let operation = body["operationId"].as_str().unwrap();
    let callback = body["links"]["clientCallback"].as_str().unwrap();
    assert!(
        callback.ends_with(&format!("/{operation}/conversation/muteUnmuteAsync/")),
        "{callback}"
    );
    calls.on_outcome(*id, &failed(), now);
    assert!(
        !events(&effects(&mut calls))
            .iter()
            .any(|event| matches!(event, CallEvent::Ended { .. }))
    );
    assert!(matches!(calls.mute_all(CallId(99)), Err(CallError::NoSuchCall)));
}

#[test]
fn a_call_whose_conversation_named_no_mute_link_mutes_nobody() {
    let now = Instant::now();
    let (mut calls, call) = group_up(now, false);
    organize(&mut calls, now);
    assert!(matches!(calls.mute_all(call), Err(CallError::NotNow)));
}

#[test]
fn the_organizers_mute_and_an_attendees_role_are_told_and_the_others_show_muted() {
    let now = Instant::now();
    let (mut calls, call) = group_up(now, true);
    let roster = callback(&calls, "rosterUpdate");
    // b muted by the organizer: shown muted though its endpoint said nothing
    let update = json!({"type": "Delta", "participants": {B: entry(2, "presenter", Some(5))}});
    calls.on_delivery(&delivery(&roster, &update), Observed::Nothing, now);
    let effects_now = effects(&mut calls);
    let participants = events(&effects_now)
        .into_iter()
        .find_map(|event| match event {
            CallEvent::Participants { participants, .. } => Some(participants),
            _ => None,
        })
        .unwrap();
    assert!(participants[0].muted);
    assert!(told(&effects_now).is_empty(), "nothing of the account's own");
    // the account muted by the organizer, an attendee
    let update = json!({"type": "Delta", "participants": {A: entry(3, "attendee", Some(4))}});
    calls.on_delivery(&delivery(&roster, &update), Observed::Nothing, now);
    let state = told(&effects(&mut calls));
    let [state] = &state[..] else { panic!("{state:#?}") };
    assert_eq!(state.muted_by_organizer, Some(4));
    assert!(state.attendee);
    // the same entry again, or an older one, tells nothing
    calls.on_delivery(&delivery(&roster, &update), Observed::Nothing, now);
    assert!(told(&effects(&mut calls)).is_empty());
    // a presenter again, its mute taken off
    let update = json!({"type": "Delta", "participants": {A: entry(4, "presenter", None)}});
    calls.on_delivery(&delivery(&roster, &update), Observed::Nothing, now);
    let state = told(&effects(&mut calls));
    assert_eq!(state[0].muted_by_organizer, None);
    assert!(!state[0].attendee);
    let _ = call;
}

#[test]
fn the_conversation_tells_the_organizers_options_and_what_binds_an_attendee() {
    let now = Instant::now();
    let (mut calls, call) = group_up(now, true);
    let updates = callback(&calls, "conversationUpdate");
    let roster = callback(&calls, "rosterUpdate");
    let attendee = json!({"type": "Delta", "participants": {A: entry(2, "attendee", None)}});
    calls.on_delivery(&delivery(&roster, &attendee), Observed::Nothing, now);
    let _ = effects(&mut calls);
    let capability = |fields: Value| json!({"sequenceNumber": 3, "meetingDetails": {"meetingCapability": fields}});
    calls.on_delivery(
        &delivery(&updates, &capability(json!({"presenterOption": "OrganizerOnly"}))),
        Observed::Nothing,
        now,
    );
    let state = told(&effects(&mut calls)).pop().unwrap();
    assert_eq!(state.options.present, Some(false));
    assert!(state.share_blocked() && !state.unmute_blocked() && !state.camera_blocked());
    calls.on_delivery(
        &delivery(&updates, &capability(json!({"attendeeRestrictions": "DisableAv"}))),
        Observed::Nothing,
        now,
    );
    let state = told(&effects(&mut calls)).pop().unwrap();
    assert!(state.unmute_blocked() && state.camera_blocked());
    // everyone may present again: nothing binds
    calls.on_delivery(
        &delivery(
            &updates,
            &capability(json!({"presenterOption": "Everyone", "autoAdmittedUsers": "Everyone"})),
        ),
        Observed::Nothing,
        now,
    );
    let state = told(&effects(&mut calls)).pop().unwrap();
    assert_eq!(state.options.lobby, Some(false));
    assert!(!state.share_blocked() && !state.unmute_blocked() && !state.camera_blocked());
    let _ = call;
}

#[test]
fn the_meeting_options_are_the_organizers_of_the_call_in_its_chat() {
    let now = Instant::now();
    let (mut calls, call) = group_up(now, true);
    assert_eq!(calls.options_of(call), None, "not the organizer yet");
    organize(&mut calls, now);
    let of = calls.options_of(call).unwrap();
    assert_eq!(
        of,
        teams_proto::OptionsOf::new(
            "00000000-0000-4000-8000-0000000000d0",
            "00000000-0000-4000-8000-00000000000a",
            GROUP
        )
        .unwrap()
    );
    assert_eq!(calls.options_of(CallId(99)), None);
    // a 1:1 call has none
    let (one_to_one, call) = connected_call(now);
    assert_eq!(one_to_one.options_of(call), None);
}

#[test]
fn why_no_options_are_named_is_told_in_a_few_words_and_a_tenant_read_later_names_them() {
    let now = Instant::now();
    let tenantless = Identity {
        tenant_id: String::new(),
        ..identity(A)
    };
    let (mut calls, call) = group_up_as(tenantless, now, true);
    assert!(!calls.knows_tenant());
    assert_eq!(calls.options_missing(CallId(99)), "the call isn't up");
    assert_eq!(
        calls.options_missing(call),
        "the roster doesn't name the account its organizer"
    );
    organize(&mut calls, now);
    assert_eq!(calls.options_of(call), None);
    assert_eq!(calls.options_missing(call), "the account's tenant isn't known yet");
    calls.set_tenant(String::new());
    assert!(!calls.knows_tenant(), "an empty tenant changes nothing");
    calls.set_tenant("00000000-0000-4000-8000-0000000000d0".to_owned());
    assert!(calls.knows_tenant());
    assert!(calls.options_of(call).is_some(), "{}", calls.options_missing(call));
    assert_eq!(calls.me(), &Mri::new(A));
}

#[test]
fn a_call_without_a_chat_or_in_a_one_to_one_chat_names_no_options_and_says_why() {
    let now = Instant::now();
    let (mut test_call, call, _) = connected_test_call(now);
    organize(&mut test_call, now);
    assert_eq!(test_call.options_of(call), None);
    assert_eq!(test_call.options_missing(call), "the call names no chat");
    let (mut one_to_one, call) = connected_call(now);
    organize(&mut one_to_one, now);
    assert_eq!(one_to_one.options_of(call), None);
    assert_eq!(
        one_to_one.options_missing(call),
        "its tenant, organizer or chat isn't of the options' form"
    );
}
