//! A chat's system messages in Teams' words (`teams_ui::system_message`): the line each kind shows in a chat (a 1:1
//! or group chat's transcript its recap card) and the chat list's preview of it. Every value here is made up.

// test code throughout (its helpers panic like the tests do)
#![cfg(test)]

use teams_store::Message;
use teams_ui::chat_list;
use teams_ui::system_message::{
    self, EventIcon, EventLine, Naming, Preview, Shown, Span, days_left, duration_text, expiry_clause, people, preview,
};

const ME: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const BEN: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
const CLEO: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const DEE: &str = "8:orgid:00000000-0000-4000-8000-00000000000d";
const BOT: &str = "28:00000000-0000-4000-8000-0000000000bb";
const MEETING: &str = "19:meeting_bWFkZS11cA@thread.v2";
const GROUP: &str = "19:00000000000000000000000000000001@thread.v2";
const ONE_TO_ONE: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";

/// Ben and Cleo by name, Dee unknown.
struct Names;

impl Naming for Names {
    fn is_me(&self, mri: &str) -> bool {
        mri == ME
    }

    fn name(&self, mri: &str, carried: Option<&str>) -> String {
        match mri {
            ME => "Ann Able".to_owned(),
            BEN => "Ben Bee".to_owned(),
            CLEO => "Cleo Sea".to_owned(),
            _ => carried.unwrap_or("Unknown user").to_owned(),
        }
    }
}

/// What a system message shows, its lines told by their words (their icons and marks checked on their own).
#[derive(Debug, PartialEq, Eq)]
enum Said {
    Line(String),
    Timed(String),
    Transcript,
    Nothing,
}

/// [`system_message::shown`], its lines as their words.
fn shown(kind: &str, content: &str, conversation: &str, sender: Option<&str>, names: &dyn Naming) -> Option<Said> {
    Some(
        match system_message::shown(kind, content, conversation, sender, names)? {
            Shown::Line(line) => Said::Line(line.text()),
            Shown::TimedLine(line) => Said::Timed(line.text()),
            Shown::Transcript => Said::Transcript,
            Shown::Nothing => Said::Nothing,
        },
    )
}

/// [`system_message::shown`]'s line, as it is.
fn shown_line(kind: &str, content: &str, conversation: &str) -> EventLine {
    match system_message::shown(kind, content, conversation, Some(BEN), &Names) {
        Some(Shown::Line(line) | Shown::TimedLine(line)) => line,
        other => panic!("not a line: {other:?}"),
    }
}

fn line(text: &str) -> Said {
    Said::Line(text.to_owned())
}

fn timed(text: &str) -> Said {
    Said::Timed(text.to_owned())
}

fn part(mri: &str, seconds: u64) -> String {
    format!(
        "<part identity=\"{mri}\"><name>{mri}</name><displayName>x</displayName><duration>{seconds}</duration></part>"
    )
}

fn ended(parts: &[(&str, u64)]) -> String {
    let parts: String = parts.iter().map(|(mri, seconds)| part(mri, *seconds)).collect();
    format!(
        "<ended/><partlist alt=\"\" count=\"{}\">{parts}</partlist><callEventType>callEnded</callEventType>",
        parts.len()
    )
}

const STARTED: &str = "<partlist alt =\"\"></partlist><callEventType>callStarted</callEventType>";

#[test]
fn a_length_is_worded_as_teams_words_it() {
    assert_eq!(duration_text(0), "0s");
    assert_eq!(duration_text(4), "4s");
    assert_eq!(duration_text(32 * 60 + 10), "32m 10s");
    assert_eq!(duration_text(3_600 + 120), "1h 2m");
}

#[test]
fn a_meeting_chats_meeting_is_a_line_after_its_time_its_length_in_bold() {
    assert_eq!(
        shown("Event/Call", STARTED, MEETING, Some(BEN), &Names),
        Some(timed("Meeting started"))
    );
    let end = ended(&[(BEN, 3697), (ME, 3697)]);
    assert_eq!(
        shown_line("Event/Call", &end, MEETING),
        EventLine {
            icon: EventIcon::Meeting,
            spans: vec![
                Span::Text("Meeting ended: ".to_owned()),
                Span::Strong("1h 1m 37s".to_owned())
            ],
        }
    );
    assert_eq!(shown_line("Event/Call", STARTED, MEETING).icon, EventIcon::Meeting);
    assert_eq!(shown_line("Event/Call", STARTED, ONE_TO_ONE).icon, EventIcon::Call);
    // an end with nobody in it: nothing, as in Teams
    assert_eq!(
        shown("Event/Call", &ended(&[]), MEETING, Some(BEN), &Names),
        Some(Said::Nothing)
    );
}

#[test]
fn a_call_is_a_line_after_its_time() {
    assert_eq!(
        shown("Event/Call", STARTED, ONE_TO_ONE, Some(BEN), &Names),
        Some(timed("Call started"))
    );
    assert_eq!(
        shown(
            "Event/Call",
            &ended(&[(BEN, 63), (ME, 63)]),
            ONE_TO_ONE,
            Some(BEN),
            &Names
        ),
        Some(timed("Call ended 1m 3s"))
    );
    // someone else's call nobody answered
    assert_eq!(
        shown("Event/Call", &ended(&[(BEN, 20)]), ONE_TO_ONE, Some(BEN), &Names),
        Some(timed("Call missed"))
    );
    // the account's own unanswered call still ended
    assert_eq!(
        shown("Event/Call", &ended(&[(ME, 20)]), ONE_TO_ONE, Some(ME), &Names),
        Some(timed("Call ended 20s"))
    );
    let huddle = format!(
        "<partlist type=\"started\" alt=\"\"><part identity=\"{BEN}\"><name>{BEN}</name></part></partlist>\
         <conversationType>\"huddleGroupCall\"</conversationType>"
    );
    assert_eq!(
        shown("Event/Call", &huddle, GROUP, Some(BEN), &Names),
        Some(timed("Ben Bee started a meeting."))
    );
}

fn recording(status: &str, code: &str, contents: &str) -> String {
    format!(
        "<URIObject type=\"Video.2/CallRecording.1\"><RecordingStatus status=\"{status}\" code=\"{code}\" />\
         <Title>Weekly sync</Title><a href=\"\">Play</a><OriginalName v=\"Weekly sync-Meeting Recording.mp4\" />\
         <RecordingInitiatorId value=\"{BEN}\" isAutoRecording=\"False\" />\
         <RecordingContent contentTypes=\"{contents}\" duration=\"0:16:50.3\"></RecordingContent></URIObject>"
    )
}

const LINK: &str = "https://madeup-my.sharepoint.com/:v:/g/personal/ben_madeup/MadeUpToken";

/// A recording ready on its file in the organizer's (Cleo's) OneDrive, stopped by `terminator` when given.
fn saved_recording(status: &str, code: &str, terminator: Option<&str>) -> String {
    let terminator = terminator
        .map(|mri| format!("<RecordingTerminatorId value=\"{mri}\" />"))
        .unwrap_or_default();
    format!(
        "<URIObject type=\"Video.2/CallRecording.1\"><RecordingStatus status=\"{status}\" code=\"{code}\" />\
         <Title>Weekly sync</Title><a href=\"{LINK}\">Play</a>\
         <MeetingOrganizerId value=\"{CLEO}\" /><RecordingInitiatorId value=\"{ME}\" isAutoRecording=\"False\" />\
         {terminator}<RecordingContent contentTypes=\"Recording+Transcript\" duration=\"0:28:38\">\
         <item type=\"onedriveForBusinessVideo\" uri=\"{LINK}\" driveId=\"b!drive1\" driveItemId=\"ITEM1\" />\
         </RecordingContent></URIObject>"
    )
}

#[test]
fn a_meeting_chat_names_everyone_by_name_and_tells_a_recording_saved() {
    let kind = "RichText/Media_CallRecording";
    // the account by its name, as Teams' meeting chat names it ("Nick started recording.")
    assert_eq!(
        shown(kind, &saved_recording("Initial", "0", None), MEETING, Some(ME), &Names),
        Some(timed("Ann Able started recording and transcription"))
    );
    assert_eq!(
        shown(
            kind,
            &saved_recording("ChunkFinished", "202", Some(BEN)),
            MEETING,
            Some(ME),
            &Names
        ),
        Some(timed("Recording and transcription stopped"))
    );
    let ready = shown_line(kind, &saved_recording("Success", "200", None), MEETING);
    assert_eq!(ready.icon, EventIcon::RecordingSaved);
    assert_eq!(ready.text(), "Recording has been saved to Cleo Sea's OneDrive.");
    assert!(
        ready.spans.iter().any(|span| matches!(
            span,
            Span::Expiry { link, file } if link == LINK && file.drive == "b!drive1" && file.item == "ITEM1"
        )),
        "{ready:?}"
    );
    // elsewhere too a recording names the account by its name, as Teams' 1:1 chat does ("AI started recording."); a
    // ready recording is the same line (its recap card goes under it)
    assert_eq!(
        shown(kind, &saved_recording("Initial", "0", None), GROUP, Some(ME), &Names),
        Some(timed("Ann Able started recording and transcription"))
    );
    let started = recording("Initial", "0", "Recording").replace(BEN, ME);
    assert_eq!(
        shown(kind, &started, ONE_TO_ONE, Some(ME), &Names),
        Some(timed("Ann Able started recording."))
    );
    let stopped = recording("ChunkFinished", "202", "Recording").replace(
        "<RecordingContent",
        &format!("<RecordingTerminatorId value=\"{ME}\" /><RecordingContent"),
    );
    assert_eq!(
        shown(kind, &stopped, ONE_TO_ONE, Some(ME), &Names),
        Some(timed("Ann Able stopped recording."))
    );
    assert_eq!(
        shown(kind, &saved_recording("Success", "200", None), GROUP, Some(ME), &Names),
        Some(timed("Recording has been saved to Cleo Sea's OneDrive."))
    );
    // a call's recording names no organizer: the OneDrive that holds it, the account's too by its name, as Teams' 1:1
    // chat says it
    let call = recording("Success", "200", "Recording").replace(
        "<RecordingContent",
        &format!(
            "<RecordingStorage type=\"RecordingInitiatorOneDrive\" userId=\"{}\" /><RecordingContent",
            ME.trim_start_matches("8:orgid:")
        ),
    );
    assert_eq!(
        shown(kind, &call, ONE_TO_ONE, Some(ME), &Names),
        Some(timed("Recording has been saved to Ann Able's OneDrive."))
    );
    assert!(people(kind, &call).contains(&ME.to_owned()));
    let started = shown_line(kind, &saved_recording("Initial", "0", None), MEETING);
    assert_eq!(started.icon, EventIcon::RecordingStarted);
    // without an organizer named: whose OneDrive isn't said
    assert_eq!(
        shown(
            kind,
            &recording("Success", "200", "Recording+Transcript"),
            MEETING,
            Some(BEN),
            &Names
        ),
        Some(timed("Recording has been saved to OneDrive."))
    );
}

#[test]
fn a_recording_stopped_names_who_stopped_it() {
    let kind = "RichText/Media_CallRecording";
    let stopped = |terminator: &str| {
        format!(
            "<URIObject type=\"Video.2/CallRecording.1\"><RecordingStatus status=\"ChunkFinished\" code=\"202\" />\
             <RecordingTerminatorId value=\"{terminator}\" /><RecordingContent contentTypes=\"Recording\" \
             duration=\"0:01:00\"></RecordingContent></URIObject>"
        )
    };
    assert_eq!(
        shown(kind, &stopped(BEN), MEETING, Some(BEN), &Names),
        Some(timed("Ben Bee stopped recording."))
    );
    assert_eq!(
        shown_line(kind, &stopped(BEN), MEETING).icon,
        EventIcon::RecordingStopped
    );
}

#[test]
fn an_expiry_is_counted_in_days_up() {
    let day = 24 * 60 * 60 * 1_000;
    let now = 1_000_000_000_000;
    assert_eq!(days_left(now + 102 * day + 1, now), Some(103));
    assert_eq!(days_left(now + 120 * day, now), Some(120));
    assert_eq!(days_left(now + 1, now), Some(1));
    assert_eq!(days_left(now, now), None);
    assert_eq!(expiry_clause(now + 120 * day, now), " and will expire in 120 days");
    assert_eq!(expiry_clause(now + 5, now), " and will expire in 1 day");
    assert_eq!(expiry_clause(now - 5, now), "");
}

#[test]
fn an_anonymous_member_is_marked_unverified() {
    let joined = format!(
        r#"{{"eventtime":1,"initiator":"{BEN}","members":[{{"id":"8:teamsvisitor:abc","friendlyname":"Note Taker"}}]}}"#
    );
    let line = shown_line("ThreadActivity/MemberJoined", &joined, MEETING);
    assert_eq!(line.icon, EventIcon::Invited);
    assert_eq!(
        line.spans,
        vec![
            Span::Text("Note Taker".to_owned()),
            Span::Unverified,
            Span::Text(" was invited to the meeting.".to_owned())
        ]
    );
    let left = shown_line("ThreadActivity/MemberLeft", &joined, MEETING);
    assert_eq!(left.icon, EventIcon::Left);
    assert_eq!(left.text(), "Note Taker left the chat.");
}

#[test]
fn a_recording_is_a_line_while_it_runs_and_once_saved() {
    let kind = "RichText/Media_CallRecording";
    assert_eq!(
        shown(
            kind,
            &recording("Initial", "0", "Recording"),
            MEETING,
            Some(BEN),
            &Names
        ),
        Some(timed("Ben Bee started recording."))
    );
    assert_eq!(
        shown(
            kind,
            &recording("Initial", "0", "Recording+Transcript"),
            MEETING,
            Some(BEN),
            &Names
        ),
        Some(timed("Ben Bee started recording and transcription"))
    );
    assert_eq!(
        shown(
            kind,
            &recording("ChunkFinished", "202", "Recording"),
            MEETING,
            Some(BEN),
            &Names
        ),
        Some(timed("Recording stopped."))
    );
    assert_eq!(
        shown(
            kind,
            &recording("ChunkFinished", "202", "Recording+Transcript"),
            MEETING,
            Some(BEN),
            &Names
        ),
        Some(timed("Recording and transcription stopped"))
    );
    assert_eq!(
        shown(
            kind,
            &recording("Success", "200", "Recording+Transcript"),
            GROUP,
            Some(BEN),
            &Names
        ),
        Some(timed("Recording has been saved to OneDrive."))
    );
    // the transcript's half of a recording: nothing, as in Teams
    assert_eq!(
        shown(
            kind,
            &recording("Initial", "0", "Transcript"),
            MEETING,
            Some(BEN),
            &Names
        ),
        Some(Said::Nothing)
    );
    // a state no capture showed: the placeholder stands for it
    assert_eq!(
        shown(
            kind,
            &recording("Failed", "500", "Recording"),
            MEETING,
            Some(BEN),
            &Names
        ),
        None
    );
}

#[test]
fn a_transcript_is_a_card_but_in_a_meeting_chat_whose_recap_card_tells_it() {
    let transcript = r#"{\"callId\":\"c\",\"isDeleted\":false}"#;
    assert_eq!(
        shown("RichText/Media_CallTranscript", transcript, GROUP, Some(BEN), &Names),
        Some(Said::Transcript)
    );
    assert_eq!(
        shown("RichText/Media_CallTranscript", transcript, MEETING, Some(BEN), &Names),
        Some(Said::Nothing)
    );
}

fn members(mris: &[&str], temp: bool) -> String {
    let kind = if temp { r#","meetingMemberType":"temp""# } else { "" };
    let list: Vec<String> = mris
        .iter()
        .map(|mri| format!(r#"{{"id":"{mri}","friendlyname":"Note Taker"{kind}}}"#))
        .collect();
    format!(
        r#"{{"eventtime":1,"initiator":"{BEN}","members":[{}]}}"#,
        list.join(",")
    )
}

#[test]
fn members_joining_and_leaving_are_worded_by_the_chat() {
    let joined = "ThreadActivity/MemberJoined";
    let left = "ThreadActivity/MemberLeft";
    assert_eq!(
        shown(joined, &members(&[DEE], false), MEETING, Some(MEETING), &Names),
        Some(line("Note Taker was invited to the meeting."))
    );
    assert_eq!(
        shown(joined, &members(&[ME], false), MEETING, Some(MEETING), &Names),
        Some(line("Ann Able was invited to the meeting."))
    );
    assert_eq!(
        shown(joined, &members(&[BEN, CLEO], false), MEETING, Some(MEETING), &Names),
        Some(line("Ben Bee and Cleo Sea were invited to the meeting."))
    );
    assert_eq!(
        shown(
            joined,
            &members(&[BEN, CLEO, ME], false),
            MEETING,
            Some(MEETING),
            &Names
        ),
        Some(line("Ben Bee and 2 others were invited to the meeting."))
    );
    assert_eq!(
        shown(joined, &members(&[BEN], true), MEETING, Some(MEETING), &Names),
        Some(line("Ben Bee has temporarily joined the chat."))
    );
    assert_eq!(
        shown(joined, &members(&[BEN], false), GROUP, Some(GROUP), &Names),
        Some(line("Ben Bee joined the conversation."))
    );
    assert_eq!(
        shown(left, &members(&[BEN], false), MEETING, Some(MEETING), &Names),
        Some(line("Ben Bee left the chat."))
    );
    assert_eq!(
        shown(left, &members(&[BEN], true), MEETING, Some(MEETING), &Names),
        Some(line("Ben Bee no longer has access to the chat."))
    );
}

#[test]
fn additions_removals_and_the_chats_name_are_worded_as_in_teams() {
    let add = |initiator: &str, targets: &[&str], share: &str| {
        let targets = targets
            .iter()
            .map(|mri| format!("<target>{mri}</target>"))
            .collect::<Vec<_>>()
            .concat();
        format!(
            "<addmember><eventtime>259200000</eventtime><initiator>{initiator}</initiator>{targets}{share}</addmember>"
        )
    };
    let kind = "ThreadActivity/AddMember";
    assert_eq!(
        shown(kind, &add(ME, &[BEN], ""), GROUP, Some(GROUP), &Names),
        Some(line("You added Ben Bee to the chat."))
    );
    assert_eq!(
        shown(kind, &add(BEN, &[ME, CLEO], ""), GROUP, Some(GROUP), &Names),
        Some(line("Ben Bee added you and Cleo Sea to the chat."))
    );
    assert_eq!(
        shown(
            kind,
            &add(BEN, &[CLEO], "<sharehistorytime>0</sharehistorytime>"),
            GROUP,
            Some(GROUP),
            &Names
        ),
        Some(line("Ben Bee added Cleo Sea to the chat and shared all chat history."))
    );
    assert_eq!(
        shown(
            kind,
            &add(BEN, &[CLEO], "<sharehistorytime>86400000</sharehistorytime>"),
            GROUP,
            Some(GROUP),
            &Names
        ),
        Some(line(
            "Ben Bee added Cleo Sea to the chat and shared chat history from the past 2 days."
        ))
    );
    assert_eq!(
        shown(kind, &add(BEN, &[BEN], ""), GROUP, Some(GROUP), &Names),
        Some(line("Ben Bee joined the conversation."))
    );
    assert_eq!(
        shown(kind, &add(BEN, &[BOT], ""), GROUP, Some(GROUP), &Names),
        Some(Said::Nothing)
    );
}

#[test]
fn removals_the_chats_name_and_its_policy_are_worded_as_in_teams() {
    let delete = |initiator: &str, targets: &[&str]| {
        let targets = targets
            .iter()
            .map(|mri| format!("<target>{mri}</target>"))
            .collect::<Vec<_>>()
            .concat();
        format!("<deletemember><initiator>{initiator}</initiator>{targets}</deletemember>")
    };
    let kind = "ThreadActivity/DeleteMember";
    assert_eq!(
        shown(kind, &delete(BEN, &[BEN]), GROUP, Some(GROUP), &Names),
        Some(line("Ben Bee left the chat."))
    );
    assert_eq!(
        shown(kind, &delete(ME, &[BEN, CLEO]), GROUP, Some(GROUP), &Names),
        Some(line("You removed Ben Bee and Cleo Sea from the chat."))
    );
    assert_eq!(
        shown(kind, &delete(GROUP, &[BEN]), GROUP, Some(GROUP), &Names),
        Some(line("Ben Bee was removed from the chat."))
    );

    let topic = |value: &str| format!("<topicupdate><initiator>{BEN}</initiator><value>{value}</value></topicupdate>");
    let kind = "ThreadActivity/TopicUpdate";
    assert_eq!(
        shown(kind, &topic("Weekly sync"), MEETING, Some(MEETING), &Names),
        Some(line("Ben Bee named the meeting Weekly sync."))
    );
    assert_eq!(
        shown(kind, &topic("Lunch"), GROUP, Some(GROUP), &Names),
        Some(line("Ben Bee changed the group name to Lunch."))
    );
    assert_eq!(
        shown(kind, &topic(""), GROUP, Some(GROUP), &Names),
        Some(line("Ben Bee removed the group name for this conversation."))
    );
    assert_eq!(
        shown(
            "ThreadActivity/MeetingPolicyUpdated",
            "<meetingpolicyupdated><initiator>x</initiator><value>ChatEnabled</value></meetingpolicyupdated>",
            MEETING,
            Some(MEETING),
            &Names
        ),
        Some(line("Chat has been turned on for this meeting."))
    );
    // a kind no capture showed: the placeholder stands for it
    assert_eq!(
        shown("ThreadActivity/RoleUpdate", "<roleupdate/>", GROUP, Some(GROUP), &Names),
        None
    );
}

#[test]
fn the_people_a_system_message_names_are_asked_for() {
    assert_eq!(
        people("ThreadActivity/MemberJoined", &members(&[BEN, CLEO], false)),
        vec![BEN, CLEO]
    );
    assert_eq!(
        people("RichText/Media_CallRecording", &recording("Initial", "0", "Recording")),
        vec![BEN]
    );
    assert!(people("RichText/Html", "<p>hi</p>").is_empty());
}

#[test]
fn the_list_previews_a_meeting_as_teams_does() {
    let said = |text: &str| Preview::Said(text.to_owned());
    assert_eq!(preview("Event/Call", STARTED, MEETING), said("Meeting started"));
    assert_eq!(
        preview("Event/Call", &ended(&[(BEN, 1930), (ME, 1930)]), MEETING),
        said("Meeting ended: 32m 10s")
    );
    assert_eq!(
        preview(
            "RichText/Media_CallRecording",
            &recording("Initial", "0", "Recording"),
            MEETING
        ),
        said("Recording has started")
    );
    assert_eq!(
        preview(
            "RichText/Media_CallRecording",
            &recording("ChunkFinished", "202", "Recording"),
            MEETING
        ),
        said("Recording has stopped")
    );
    assert_eq!(
        preview(
            "RichText/Media_CallRecording",
            &recording("Success", "200", "Recording+Transcript"),
            MEETING
        ),
        said("Recording is ready")
    );
    assert_eq!(
        preview(
            "RichText/Media_CallRecording",
            &recording("Initial", "0", "Transcript"),
            MEETING
        ),
        Preview::Skipped
    );
    assert_eq!(
        preview("RichText/Media_CallTranscript", r#"{"isDeleted":false}"#, MEETING),
        said("Transcript is available")
    );
    // a 1:1 call and the thread's events leave the line to the text before them
    assert_eq!(preview("Event/Call", STARTED, ONE_TO_ONE), Preview::Skipped);
    assert_eq!(
        preview("ThreadActivity/MemberJoined", &members(&[BEN], false), MEETING),
        Preview::Skipped
    );
    assert_eq!(preview("RichText/Html", "<p>hi</p>", MEETING), Preview::Text);
    assert_eq!(preview("RichText/Media_Card", "<URIObject/>", MEETING), Preview::Text);
}

#[test]
fn shared_history_is_worded_only_as_far_as_the_event_says_it_plainly() {
    let add = |event_time: &str, share: &str| {
        format!(
            "<addmember>{event_time}<initiator>{BEN}</initiator><target>{CLEO}</target>\
             <sharehistorytime>{share}</sharehistorytime></addmember>"
        )
    };
    let shown_for = |content: &str| shown("ThreadActivity/AddMember", content, GROUP, Some(GROUP), &Names);
    let plain = Some(line("Ben Bee added Cleo Sea to the chat."));
    // a share time an hour before the event: too close to tell, no clause
    assert_eq!(shown_for(&add("<eventtime>90000000</eventtime>", "86400000")), plain);
    // no event time, or one in a form no capture showed: the days can't be told
    assert_eq!(shown_for(&add("", "86400000")), plain);
    assert_eq!(shown_for(&add("<eventtime>soon</eventtime>", "86400000")), plain);
    // a share time in a form no capture showed: no clause
    assert_eq!(shown_for(&add("<eventtime>90000000</eventtime>", "yesterday")), plain);
    // `0`: all of it, whatever the event time
    assert_eq!(
        shown_for(&add("", "0")),
        Some(line("Ben Bee added Cleo Sea to the chat and shared all chat history."))
    );
    assert_eq!(
        shown_for(&add("<eventtime>172800000</eventtime>", "86400000")),
        Some(line(
            "Ben Bee added Cleo Sea to the chat and shared chat history from the past day."
        ))
    );
}

#[test]
fn an_addition_without_an_initiator_keeps_the_placeholder() {
    let content = format!("<addmember><eventtime>1</eventtime><target>{BEN}</target></addmember>");
    assert_eq!(
        shown("ThreadActivity/AddMember", &content, GROUP, Some(GROUP), &Names),
        None
    );
}

#[test]
fn a_call_s_media_message_this_build_doesn_t_read_never_previews_its_record() {
    let failed = recording("Failed", "500", "Recording");
    for (kind, content) in [
        // a deleted transcript: its ids stay out of the line
        (
            "RichText/Media_CallTranscript",
            r#"{\"callId\":\"c-1\",\"meetingOrganizerId\":\"8:orgid:x\",\"isDeleted\":true}"#,
        ),
        ("RichText/Media_CallRecording", failed.as_str()),
        (
            "RichText/Media_CallLogRecording",
            r#"{"CallId":"c-1","UserParticipant":"8:orgid:x"}"#,
        ),
        ("RichText/Media_CallLogTranscript", r#"{"CallId":"c-1"}"#),
    ] {
        assert_eq!(preview(kind, content, MEETING), Preview::Skipped, "{kind}");
        assert_eq!(shown(kind, content, MEETING, Some(BEN), &Names), None, "{kind}");
    }
}

#[test]
fn the_list_line_passes_over_a_call_s_record_to_the_text_before_it() {
    let stored = |id: &str, kind: &str, content: &str, at: i64| Message {
        conversation_id: MEETING.to_owned(),
        id: id.to_owned(),
        version: 1,
        client_message_id: None,
        sender: Some(BEN.to_owned()),
        sender_name: None,
        message_type: kind.to_owned(),
        content: content.to_owned(),
        properties: None,
        arrival_time: at,
        compose_time: None,
        edit_time: None,
        delete_time: None,
    };
    // newest first, as the store pages them
    let page = [
        stored(
            "3",
            "RichText/Media_CallTranscript",
            r#"{\"callId\":\"c-1\",\"isDeleted\":true}"#,
            3,
        ),
        stored("2", "RichText/Media_CallLogRecording", r#"{"CallId":"c-1"}"#, 2),
        stored("1", "RichText/Html", "<p>see you there</p>", 1),
    ];
    assert_eq!(chat_list::preview(&page, ME).as_ref(), "see you there");
}
