//! The account's own actions in a call that is up (its mute told, its hand raised and lowered, its live reactions, a
//! meeting ended for everyone) and what its roster and messages tell of the others, on synthetic deliveries.

use super::*;
use teams_proto::Request as _;

/// a's call to b, its start answered with every link of a meeting's conversation (as captured 2026-11-06) and its
/// acceptance delivered: up.
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
    for op in ["updateEndpointState", "publishState", "removeState", "sendMessage"] {
        answer["links"][op] = json!(conv(&format!("/{op}")));
    }
    calls.on_outcome(start, &ok(&answer), now);
    let path = acceptance_path(&calls);
    calls.on_delivery(&delivery(&path, &acceptance()), Observed::Acceptance, now);
    let connected = effects(&mut calls);
    assert!(events(&connected).contains(&CallEvent::State {
        call,
        state: CallState::Connected
    }));
    (calls, call)
}

/// The writes among `effects`, each by its name with its body.
fn bodies(effects: &[Effect]) -> Vec<(WriteId, &'static str, Value)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Send { id, write } => {
                let body = match write.as_ref() {
                    CallWrite::EndpointState(write) => write.body(),
                    CallWrite::PublishState(write) => write.body(),
                    CallWrite::RemoveState(write) => write.body(),
                    CallWrite::Reaction(write) => write.body(),
                    CallWrite::EndMeeting(write) => write.body(),
                    _ => None,
                };
                Some((*id, write.name(), body.unwrap_or(Value::Null)))
            }
            _ => None,
        })
        .collect()
}

/// The path of the call's conversation callback `name`.
fn conversation_callback(calls: &Calls, name: &str) -> String {
    calls
        .call
        .as_ref()
        .and_then(|call| call.path_of(Kind::Conversation, name))
        .unwrap()
        .to_owned()
}

/// A roster entry in the call: muted or not, its hand's id while up, and its meeting role.
fn in_call(version: u64, muted: bool, hand: Option<&str>, role: &str) -> Value {
    let mut entry = json!({"version": version, "state": "active", "meetingRole": role,
        "endpoints": {"e": {"call": {}, "endpointState": {"endpointStateSequenceNumber": 2,
            "state": {"isMuted": muted}}}}});
    if let Some(hand) = hand {
        entry["publishedStates"] =
            json!([{"typeRank": 1, "stateType": "raiseHands", "content": {"skinTone": 0}, "stateId": hand}]);
    }
    entry
}

#[test]
fn a_mute_is_told_to_the_conversation_as_the_endpoint_state_s_next_report_and_its_failure_ends_nothing() {
    let now = Instant::now();
    let (mut calls, call) = up(now);
    calls.set_muted(call, true).unwrap();
    calls.set_muted(call, false).unwrap();
    let told = bodies(&effects(&mut calls));
    let [
        (first, "update_endpoint_state", muted),
        (_, "update_endpoint_state", unmuted),
    ] = &told[..]
    else {
        panic!("{told:#?}");
    };
    assert_eq!(muted["endpointState"]["endpointStateSequenceNumber"], json!(2));
    assert_eq!(muted["endpointState"]["state"]["isMuted"], json!(true));
    assert_eq!(unmuted["endpointState"]["endpointStateSequenceNumber"], json!(3));
    assert_eq!(unmuted["endpointState"]["state"]["isMuted"], json!(false));
    calls.on_outcome(*first, &failed(), now);
    assert!(
        !events(&effects(&mut calls))
            .iter()
            .any(|event| matches!(event, CallEvent::Ended { .. })),
        "the call goes on"
    );
    assert!(matches!(calls.set_muted(CallId(99), true), Err(CallError::NoSuchCall)));
}

#[test]
fn a_hand_goes_up_at_publish_state_and_comes_down_by_the_id_the_roster_gives_it() {
    let now = Instant::now();
    let (mut calls, call) = up(now);
    assert!(
        matches!(calls.raise_hand(call, false), Err(CallError::NotNow)),
        "no hand is up"
    );
    calls.raise_hand(call, true).unwrap();
    let raised = bodies(&effects(&mut calls));
    let [(_, "publish_state", published)] = &raised[..] else {
        panic!("{raised:#?}");
    };
    assert_eq!(published["publishedState"]["stateType"], json!("raiseHands"));
    assert_eq!(published["publishedState"]["sequenceNumber"], json!(1));
    // the roster tells the account's hand up, and b muted with theirs
    let roster = conversation_callback(&calls, "rosterUpdate");
    let update = json!({"type": "Delta", "participants": {
        A: in_call(3, false, Some("hand-a"), "attendee"),
        B: in_call(4, true, Some("hand-b"), "attendee"),
    }});
    calls.on_delivery(&delivery(&roster, &update), Observed::Nothing, now);
    let told = events(&effects(&mut calls));
    assert!(told.contains(&CallEvent::Own {
        call,
        hand_raised: true,
        organizer: false
    }));
    let participants = told
        .iter()
        .find_map(|event| match event {
            CallEvent::Participants { participants, .. } => Some(participants.clone()),
            _ => None,
        })
        .expect("b told");
    assert!(participants[0].muted && participants[0].hand_raised);
    calls.raise_hand(call, false).unwrap();
    let lowered = bodies(&effects(&mut calls));
    let [(_, "remove_state", removed)] = &lowered[..] else {
        panic!("{lowered:#?}");
    };
    assert_eq!(removed["stateIds"], json!(["hand-a"]));
    assert_eq!(removed["sequenceNumber"], json!(2));
}

#[test]
fn a_reaction_goes_to_send_message_and_one_delivered_is_told_with_who_sent_it() {
    let now = Instant::now();
    let (mut calls, call) = up(now);
    calls.react(call, "like").unwrap();
    let sent_reaction = bodies(&effects(&mut calls));
    let [(_, "send_message", message)] = &sent_reaction[..] else {
        panic!("{sent_reaction:#?}");
    };
    assert_eq!(message["messageContent"][0]["type"], json!("Reaction"));
    assert_eq!(
        message["messageContent"][0]["payload"]["values"][0]["name"],
        json!("like")
    );
    assert!(matches!(calls.react(call, "wave"), Err(CallError::Write(_))));
    let received = conversation_callback(&calls, "receiveMessage");
    let reaction = json!({"from": {"id": B, "displayName": "Lab Bee"}, "type": "Reaction",
        "payload": {"values": [{"name": "heart", "count": 1}]}});
    calls.on_delivery(&delivery(&received, &reaction), Observed::Nothing, now);
    calls.on_delivery(
        &delivery(&received, &json!({"type": "Something", "from": {"id": B}})),
        Observed::Nothing,
        now,
    );
    assert_eq!(
        events(&effects(&mut calls)),
        [CallEvent::Reaction {
            call,
            from: Mri::new(B),
            name: Some("Lab Bee".to_owned()),
            reaction: "heart".to_owned(),
        }]
    );
}

#[test]
fn only_the_organizer_ends_the_meeting_for_everyone_and_the_call_is_over_once_it_went() {
    let now = Instant::now();
    let (mut calls, call) = up(now);
    assert!(
        matches!(calls.end_meeting(call), Err(CallError::NotNow)),
        "not the organizer"
    );
    let roster = conversation_callback(&calls, "rosterUpdate");
    let update = json!({"type": "Delta", "participants": {A: in_call(3, false, None, "organizer")}});
    calls.on_delivery(&delivery(&roster, &update), Observed::Nothing, now);
    assert!(events(&effects(&mut calls)).contains(&CallEvent::Own {
        call,
        hand_raised: false,
        organizer: true
    }));
    calls.end_meeting(call).unwrap();
    let ended = bodies(&effects(&mut calls));
    let [(id, "end_conversation", body)] = &ended[..] else {
        panic!("{ended:#?}");
    };
    assert_eq!(
        body["conversationTransactionEnd"]["phrase"],
        json!("ConversationEndForAllInitiated")
    );
    calls.on_outcome(*id, &ok(&json!({})), now);
    assert!(events(&effects(&mut calls)).contains(&CallEvent::Ended { call, end: End::HungUp }));
}

#[test]
fn a_meeting_end_not_taken_leaves_the_call_instead() {
    let now = Instant::now();
    let (mut calls, call) = up(now);
    let roster = conversation_callback(&calls, "rosterUpdate");
    let update = json!({"type": "Delta", "participants": {A: in_call(3, false, None, "organizer")}});
    calls.on_delivery(&delivery(&roster, &update), Observed::Nothing, now);
    let _ = effects(&mut calls);
    calls.end_meeting(call).unwrap();
    let ended = sent(&effects(&mut calls));
    let [(id, "end_conversation")] = ended[..] else {
        panic!("{ended:#?}");
    };
    calls.on_outcome(id, &failed(), now);
    assert_eq!(names(&effects(&mut calls)), ["leave"]);
}

#[test]
fn nothing_goes_while_the_call_isn_t_up() {
    let now = Instant::now();
    let (mut calls, call) = ringing_call(now);
    assert!(matches!(calls.set_muted(call, true), Err(CallError::NotNow)));
    assert!(matches!(calls.raise_hand(call, true), Err(CallError::NotNow)));
    assert!(matches!(calls.react(call, "like"), Err(CallError::NotNow)));
    assert!(matches!(calls.end_meeting(call), Err(CallError::NotNow)));
    assert!(
        effects(&mut calls)
            .iter()
            .all(|effect| !matches!(effect, Effect::Send { .. }))
    );
}

#[test]
fn a_call_up_whose_conversation_named_none_of_its_links_sends_none_of_them() {
    let now = Instant::now();
    let (mut connected, up) = connected_call(now);
    assert!(matches!(connected.set_muted(up, true), Err(CallError::NotNow)));
    assert!(matches!(connected.raise_hand(up, true), Err(CallError::NotNow)));
    assert!(matches!(connected.react(up, "like"), Err(CallError::NotNow)));
    assert!(effects(&mut connected).is_empty());
}

#[test]
fn a_callee_names_itself_by_the_participant_id_its_invitation_gave_it() {
    let now = Instant::now();
    let mut calls = Calls::new(identity(B));
    calls.set_push_url(PUSH).unwrap();
    let gp = json!({
        "callNotification": {
            "from": {"id": A, "displayName": "Lab A"},
            "to": {"id": B, "participantId": "00000000-0000-4000-8000-0000000000bb"},
            "links": {"attach": cc("forked", "attach")},
        },
        "conversationInvitation": {"conversationController": conv("")},
    });
    let invited = json!({"evt": 107, "gp": base64::engine::general_purpose::STANDARD.encode(gp.to_string())});
    calls.on_delivery(
        &delivery("/v4/f/SyntheticEndpoint00000/", &invited),
        Observed::Offered,
        now,
    );
    let me = calls.call.as_ref().map(|call| call.me.participant_id.clone());
    assert_eq!(me, Some(Some("00000000-0000-4000-8000-0000000000bb".to_owned())));
}
