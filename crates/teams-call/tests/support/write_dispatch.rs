//! Each call write as the engine's sink gets it ([`CallWrite::submit_to`]): its own sealed `teams-proto` type, under
//! the id its outcome comes back by, at the link it was built for; and the step each is tracked as names it as the
//! write does (what a failed step's end says).

use super::*;
use crate::agent::guid;
use crate::build;
use crate::call::{Call, Step};
use teams_proto::{
    CallLink, ChannelParameters, Links, MediaDescription, MediaDescriptions, MeetingTimer, MuteParticipants, Request,
    TimerState, UpdateMeetingSettings, UpdateMeetingStates,
};

/// What the sink got: the write's id, the name of its type, its link, whether it is a write, and whether it enters a
/// call (the cross-account floor's question).
#[derive(Debug, PartialEq, Eq)]
struct Submitted {
    id: u64,
    kind: &'static str,
    url: Option<String>,
    write: bool,
    enters: bool,
}

#[derive(Default)]
struct Recorder(Vec<Submitted>);

impl crate::WriteSink for Recorder {
    fn submit<R: Request + Clone + Send + 'static>(&mut self, id: WriteId, write: R) {
        let kind = std::any::type_name::<R>().rsplit("::").next().unwrap_or_default();
        self.0.push(Submitted {
            id: id.get(),
            kind,
            url: write.call_url().map(ToString::to_string),
            write: write.is_write(),
            enters: crate::Tracker::enters(&write),
        });
    }
}

fn link(url: &str) -> CallLink {
    CallLink::parse(url).unwrap()
}

/// A write as the machine builds it, the step it is tracked as, its sealed type's name and its link.
struct Case {
    step: Step,
    write: CallWrite,
    kind: &'static str,
    at: String,
}

fn case(step: Step, write: CallWrite, kind: &'static str, at: &str) -> Case {
    Case {
        step,
        write,
        kind,
        at: at.to_owned(),
    }
}

/// The writes of a call that is up, each at its own link.
fn in_call_writes(call: &mut Call) -> Vec<Case> {
    let acknowledge = cc("callAcceptance", "acknowledge");
    let parameters = cc("mcProxy", "applyChannelParameters");
    let properties = conv("/updateParticipantProperties");
    let answer = cc("negotiations", "answer");
    let described = cc("active", "updateMediaDescriptions");
    let leg = call_leg();
    let acknowledged = cc("mediaAnswers", "acknowledge");
    let progress = cc("incoming", "progress");
    let descriptions = MediaDescriptions {
        descriptions: vec![MediaDescription {
            mid: "1".into(),
            direction: "recvonly".into(),
            label: None,
        }],
        request_id: Some(1),
    };
    let capabilities = ChannelParameters {
        mids: vec!["*".into()],
        media_parameter: "{}".into(),
    };
    vec![
        case(
            Step::Acknowledge,
            build::acknowledge(call, &link(&acknowledge), &Links::new()).unwrap(),
            "AcknowledgeAcceptance",
            &acknowledge,
        ),
        case(
            Step::ChannelParameters,
            build::channel_parameters(call, &link(&parameters), &capabilities).unwrap(),
            "ApplyChannelParameters",
            &parameters,
        ),
        case(
            Step::Properties,
            build::screen_sharer(call, &link(&properties)).unwrap(),
            "UpdateParticipantProperties",
            &properties,
        ),
        case(
            Step::MediaAnswer,
            build::answer_media(call, &link(&answer), media()).unwrap(),
            "AnswerMedia",
            &answer,
        ),
        case(
            Step::Describe,
            build::describe(call, &link(&described), &descriptions, None).unwrap(),
            "UpdateMediaDescriptions",
            &described,
        ),
        case(
            Step::KeepAlive,
            build::keep_alive(call, &link(&leg)).unwrap(),
            "KeepAliveCall",
            &leg,
        ),
        case(
            Step::MediaAcknowledge,
            build::acknowledge_media(call, &link(&acknowledged)).unwrap(),
            "AcknowledgeMediaAnswer",
            &acknowledged,
        ),
        case(
            Step::Ringing,
            build::ringing(call, &link(&progress)).unwrap(),
            "ReportRinging",
            &progress,
        ),
    ]
}

/// The writes of the conversation of a call that is up: the lobby, the hand, reactions, the meeting's own.
fn conversation_writes(call: &mut Call) -> Vec<Case> {
    let admit = conv("/admit");
    let publish = conv("/publishState");
    let remove = conv("/removeState");
    let message = conv("/sendMessage");
    let conversation = conv("");
    let add = conv("/add");
    vec![
        case(
            Step::Lobby,
            build::lobby(call, &link(&admit), &Mri::new(B), true).unwrap(),
            "DecideLobby",
            &admit,
        ),
        case(
            Step::Hand,
            build::hand(call, &link(&publish), None).unwrap(),
            "PublishState",
            &publish,
        ),
        case(
            Step::Hand,
            build::hand(call, &link(&remove), Some("hand-a")).unwrap(),
            "RemoveState",
            &remove,
        ),
        case(
            Step::React,
            build::reaction(call, &link(&message), "like").unwrap(),
            "SendCallReaction",
            &message,
        ),
        case(
            Step::EndMeeting,
            build::end_meeting(call, &link(&conversation)).unwrap(),
            "EndConversation",
            &conversation,
        ),
        case(
            Step::Invite,
            build::add_person(
                call,
                &link(&add),
                &Mri::new(C),
                "19:00000000000000000000000000010001@thread.v2",
            )
            .unwrap(),
            "AddToCall",
            &add,
        ),
    ]
}

/// The meeting's own writes, as the organizer's controls build them.
fn meeting_writes(call: &mut Call) -> Vec<Case> {
    let states_at = conv("/updateMeetingStates");
    let settings_at = conv("/updateMeetingSettings");
    let mute_at = conv("/mute");
    let countdown = MeetingTimer {
        state: TimerState::InProgress,
        remaining_ms: 60_000,
        original_ms: 60_000,
        edited_ms: None,
        previous: None,
    };
    let status = call.callback(Kind::Conversation, "updateMeetingStatesStatus").unwrap();
    let timing = UpdateMeetingStates::timer(
        &call.context,
        &link(&states_at),
        &call.me,
        (&countdown, 0, None),
        &status,
    )
    .unwrap();
    let locking = UpdateMeetingSettings::lock(&call.context, &link(&settings_at), &call.me, true, 1).unwrap();
    let operation = guid().unwrap();
    let callback = call.callback_in(Kind::Conversation, "muteUnmuteAsync", &operation);
    let muting = MuteParticipants::everyone(&call.context, &link(&mute_at), &call.me, &callback, &operation).unwrap();
    vec![
        case(
            Step::MeetingStates,
            CallWrite::MeetingStates(timing),
            "UpdateMeetingStates",
            &states_at,
        ),
        case(
            Step::MeetingSettings,
            CallWrite::MeetingSettings(locking),
            "UpdateMeetingSettings",
            &settings_at,
        ),
        case(Step::MuteAll, CallWrite::MuteAll(muting), "MuteParticipants", &mute_at),
    ]
}

/// The call leg's own writes: hold, resume, transfer and a transfer taken.
fn leg_writes(call: &mut Call) -> Vec<Case> {
    let hold = cc("active", "hold");
    let resume = cc("holdSession", "resume");
    let transfer = cc("active", "transfer");
    let acceptance = cc("transfer", "acceptance");
    vec![
        case(
            Step::Hold,
            build::hold(call, &link(&hold), true).unwrap(),
            "HoldCall",
            &hold,
        ),
        case(
            Step::Resume,
            build::hold(call, &link(&resume), false).unwrap(),
            "ResumeCall",
            &resume,
        ),
        case(
            Step::Transfer,
            build::transfer(call, &link(&transfer), &Mri::new(C)).unwrap(),
            "TransferCall",
            &transfer,
        ),
        case(
            Step::AcceptTransfer,
            build::accept_transfer(call, &link(&acceptance)).unwrap(),
            "AcceptTransfer",
            &acceptance,
        ),
    ]
}

fn all_writes(call: &mut Call) -> Vec<Case> {
    let mut cases = in_call_writes(call);
    cases.extend(conversation_writes(call));
    cases.extend(meeting_writes(call));
    cases.extend(leg_writes(call));
    cases
}

#[test]
fn each_write_reaches_the_sink_as_its_own_sealed_type_under_its_id_at_its_link() {
    let (mut calls, _) = up(Instant::now());
    let call = calls.call.as_mut().unwrap();
    let cases = all_writes(call);
    let mut sink = Recorder::default();
    let mut expected = Vec::new();
    for (n, case) in (100..).zip(cases) {
        expected.push(Submitted {
            id: n,
            kind: case.kind,
            url: Some(case.at),
            write: true,
            enters: false,
        });
        case.write.submit_to(WriteId(n), &mut sink);
    }
    assert_eq!(sink.0, expected);
}

#[test]
fn each_step_names_its_write_as_the_write_names_itself_but_a_hand_raised_or_lowered() {
    let (mut calls, _) = up(Instant::now());
    let call = calls.call.as_mut().unwrap();
    for case in all_writes(call) {
        if case.step == Step::Hand {
            assert_eq!(case.step.name(), "hand");
            assert!(matches!(case.write.name(), "publish_state" | "remove_state"));
        } else {
            assert_eq!(case.step.name(), case.write.name(), "{:?}", case.step);
        }
    }
    // a person added names the same write as the echo service added to the test call
    assert_eq!(Step::Add.name(), Step::Invite.name());
}

#[test]
fn a_start_enters_a_call_and_an_attach_needs_an_invitation_to_answer() {
    let now = Instant::now();
    let (mut calls, _) = up(now);
    let call = calls.call.as_mut().unwrap();
    assert!(
        matches!(build::attach(call, &identity(A)), Err(CallError::NotNow)),
        "a placed call answers no invitation"
    );
    let placement = Placement::Chat {
        callees: vec![Mri::new(B)],
        group_chat: None,
    };
    let start = build::start(call, &identity(A), &placement, media()).unwrap();
    assert_eq!(start.name(), Step::Start.name());
    let mut sink = Recorder::default();
    start.submit_to(WriteId(7), &mut sink);
    let [submitted] = &sink.0[..] else {
        panic!("{:?}", sink.0);
    };
    assert_eq!((submitted.id, submitted.kind), (7, "StartCall"));
    assert!(submitted.write && submitted.enters, "{submitted:?}");
}
