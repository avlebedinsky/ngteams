//! A chat's system messages read into what Teams draws for them (`teams_proto::chat::chat_event`), in the shapes the
//! lab's calls and the personal build's stores showed (`docs/protocol/chat.md`, "System messages"). Every value here
//! is made up.

use teams_proto::chat::Field;
use teams_proto::chat::{
    CallEnd, CallStart, ChatEvent, ChatPolicy, Member, Members, RecordingContents, RecordingItem, RecordingStatus,
    Removed, TopicUpdate, chat_event, meeting_times, thread_activity,
};

const ANN: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const BEN: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
const BOT: &str = "28:00000000-0000-4000-8000-0000000000bb";
const MEETING: &str = "19:meeting_bWFkZS11cA@thread.v2";

fn part(mri: &str, name: &str, seconds: &str) -> String {
    format!(
        "<part identity=\"{mri}\"><name>{mri}</name><displayName>{name}</displayName><duration>{seconds}</duration></part>"
    )
}

fn details(event: &str) -> String {
    format!(
        "<meetingDetails><meetingDetails><organizerUpn>ann@example.test</organizerUpn><meetingType>Scheduled</meetingType>\
         <startTime>01/02/2026 10:00:00</startTime><endTime>01/02/2026 10:30:00</endTime></meetingDetails></meetingDetails>\
         <callEventType>{event}</callEventType>\n<conversationStartTime>01/02/2026 10:00:05</conversationStartTime>\n\
         <callId>11111111-2222-4333-8444-555555555555</callId>\n"
    )
}

#[test]
fn a_call_event_gives_its_meetings_scheduled_times_and_call() {
    let times = meeting_times(&format!("<partlist alt =\"\"></partlist>{}", details("callStarted"))).expect("times");
    // 2026-01-02 10:00 and 10:30 UTC
    assert_eq!(times.scheduled, Some((1_767_348_000_000, 1_767_349_800_000)));
    assert_eq!(times.call_id.as_deref(), Some("11111111-2222-4333-8444-555555555555"));
    // a leap day, and an end before the start that is no schedule
    let leap = meeting_times(
        "<meetingDetails><startTime>02/29/2028 23:59:59</startTime><endTime>03/01/2028 00:00:00</endTime></meetingDetails>",
    )
    .expect("times");
    assert_eq!(leap.scheduled, Some((1_835_481_599_000, 1_835_481_600_000)));
    let backwards =
        "<startTime>01/02/2026 10:30:00</startTime><endTime>01/02/2026 10:00:00</endTime><callId>c</callId>";
    assert_eq!(meeting_times(backwards).and_then(|times| times.scheduled), None);
    for bad in [
        "13/01/2026 10:00:00",
        "01/02/2026 24:00:00",
        "01/02/2026",
        "1/2/2026 1:2:3:4",
        "01/02/9223372036854775807 10:00:00",
        "02/31/2026 10:00:00",
        "02/29/2027 10:00:00",
        "x",
    ] {
        let content = format!("<startTime>{bad}</startTime><endTime>01/02/2027 10:00:00</endTime>");
        assert_eq!(meeting_times(&content), None, "{bad}");
    }
}

#[test]
fn a_call_start_names_no_one_until_it_ends() {
    let start = format!("<partlist alt =\"\"></partlist>{}", details("callStarted"));
    assert_eq!(
        chat_event("Event/Call", &start),
        Some(ChatEvent::CallStarted(CallStart {
            huddle: false,
            initiator: None
        }))
    );
    // the oldest shape: a bare part list
    assert!(matches!(
        chat_event("Event/Call", "<partlist alt =\"\"></partlist>"),
        Some(ChatEvent::CallStarted(_))
    ));
}

#[test]
fn a_call_end_takes_the_last_participants_duration() {
    let end = format!(
        "<ended/><partlist alt=\"\" count=\"2\">{}{}</partlist>{}<wasGcpBotInvited>False</wasGcpBotInvited>",
        part(ANN, "Ann Able", "1925"),
        part(BEN, "Ben Bee", "1930"),
        details("callEnded")
    );
    assert_eq!(
        chat_event("Event/Call", &end),
        Some(ChatEvent::CallEnded(CallEnd {
            huddle: false,
            duration: Some(1930),
            participants: vec![ANN.to_owned(), BEN.to_owned()],
        }))
    );
    // nobody in it: no length, which Teams draws nothing for
    let empty = format!(
        "<ended/><partlist alt=\"\" count=\"0\"></partlist>{}",
        details("callEnded")
    );
    assert_eq!(
        chat_event("Event/Call", &empty),
        Some(ChatEvent::CallEnded(CallEnd {
            huddle: false,
            duration: None,
            participants: Vec::new(),
        }))
    );
    // a length in a form no capture showed: not read at all
    let odd = format!(
        "<ended/><partlist alt=\"\">{}</partlist>",
        part(ANN, "Ann Able", "12.5")
    );
    assert_eq!(chat_event("Event/Call", &odd), None);
}

#[test]
fn a_group_chats_meeting_names_who_started_it() {
    let start = format!(
        "<partlist type=\"started\" alt=\"\"><part identity=\"{ANN}\"><name>{ANN}</name></part></partlist>\
         <conversationType>\"huddleGroupCall\"</conversationType>{}",
        details("callStarted")
    );
    assert_eq!(
        chat_event("Event/Call", &start),
        Some(ChatEvent::CallStarted(CallStart {
            huddle: true,
            initiator: Some(ANN.to_owned())
        }))
    );
}

fn recording(status: &str, code: &str, contents: Option<&str>, duration: &str) -> String {
    let types = contents
        .map(|types| format!(" contentTypes=\"{types}\""))
        .unwrap_or_default();
    format!(
        "<URIObject format_version=\"1.1\" type=\"Video.2/CallRecording.1\" url_thumbnail=\"\" uri=\"\" version=\"1.0\">\
         <RecordingStatus status=\"{status}\" code=\"{code}\" shortChunk=\"False\"><amsErrorResult /></RecordingStatus>\
         <SessionEndReason value=\"CallEnded\" /><Title>Weekly sync</Title><a href=\"\">Play</a>\
         <OriginalName v=\"Weekly sync-20260102_100010-Meeting Recording.mp4\" />\
         <RecordingInitiatorId value=\"{ANN}\" isAutoRecording=\"False\" />\
         <RecordingContent{types} timestamp=\"2026-01-02T10:00:10.0000000Z\" duration=\"{duration}\" canVideoExpire=\"False\">\
         <item type=\"video\" uri=\"\" /></RecordingContent><RequestedExports /></URIObject>"
    )
}

#[test]
fn a_recording_reads_its_state_its_contents_its_name_and_its_length() {
    let ready = chat_event(
        "RichText/Media_CallRecording",
        &recording("Success", "200", Some("Recording+Transcript"), "0:16:50.32"),
    );
    let Some(ChatEvent::Recording(ready)) = ready else {
        panic!("not a recording: {ready:?}");
    };
    assert_eq!(ready.status, RecordingStatus::Ready);
    assert_eq!(ready.contents, RecordingContents::RecordingAndTranscript);
    assert_eq!(ready.duration, Some(16 * 60 + 50));
    assert_eq!(ready.title.as_deref(), Some("Weekly sync"));
    assert_eq!(ready.name(), Some("Weekly sync-20260102_100010-Meeting Recording"));
    assert_eq!(ready.initiator.as_deref(), Some(ANN));
    assert!(!ready.automatic);

    let started = chat_event(
        "RichText/Media_CallRecording",
        &recording("Initial", "0", Some("Transcript"), "0:00:00"),
    );
    assert!(matches!(
        started,
        Some(ChatEvent::Recording(ref recording))
            if recording.status == RecordingStatus::Started && recording.contents == RecordingContents::Transcript
    ));
    // a channel meeting's names no contents: a recording
    let stopped = chat_event(
        "RichText/Media_CallRecording",
        &recording("ChunkFinished", "202", None, "1.02:03:04.5"),
    );
    assert!(matches!(
        stopped,
        Some(ChatEvent::Recording(ref recording))
            if recording.status == RecordingStatus::Stopped
                && recording.contents == RecordingContents::Recording
                && recording.duration == Some(((24 + 2) * 60 + 3) * 60 + 4)
    ));
}

/// A recording ready, shaped as a lab meeting's was: its organizer, who stopped it, its call, its start, its file and
/// its transcript on that file.
const READY: &str = "<URIObject format_version=\"1.1\" type=\"Video.2/CallRecording.1\" url_thumbnail=\"\" uri=\"\" \
    version=\"1.0\"><RecordingStatus status=\"Success\" code=\"200\" shortChunk=\"False\"><amsErrorResult />\
    </RecordingStatus><SessionEndReason value=\"RecordingStopCommand\" /><Title>Weekly sync</Title>\
    <a href=\"https://madeup-my.sharepoint.com/:v:/g/personal/ann_madeup/MadeUpShare\">Play</a>\
    <OriginalName v=\"Weekly sync-20260102_100010-Meeting Recording.mp4\" />\
    <MeetingOrganizerId value=\"8:orgid:00000000-0000-4000-8000-00000000000a\" />\
    <MeetingOrganizerTenantId value=\"00000000-0000-4000-8000-0000000000f0\" />\
    <RecordingInitiatorId value=\"8:orgid:00000000-0000-4000-8000-00000000000a\" isAutoRecording=\"False\" />\
    <RecordingTerminatorId value=\"8:orgid:00000000-0000-4000-8000-00000000000b\" />\
    <Identifiers><Id type=\"callId\" value=\"00000000-0000-4000-8000-0000000000c1\" />\
    <Id type=\"callLegId\" value=\"00000000-0000-4000-8000-0000000000c2\" /></Identifiers>\
    <RecordingContent contentTypes=\"Recording+Transcript\" timestamp=\"2026-01-02T10:00:10.0360480Z\" \
    duration=\"0:28:38.92\" canVideoExpire=\"True\"><item type=\"video\" uri=\"\" />\
    <item type=\"onedriveForBusinessVideo\" uri=\"https://madeup-my.sharepoint.com/:v:/g/personal/ann_madeup/MadeUpShare\" \
    driveId=\"b!drive1\" driveItemId=\"ITEM1\" />\
    <item type=\"onedriveForBusinessTranscript\" uri=\"https://madeup-my.sharepoint.com/personal/ann_madeup/_api/v2.1/\
    drives/b!drive1/items/ITEM1/versions/current/media/transcripts/t1/content\" id=\"t1\" driveId=\"b!drive1\" \
    driveItemId=\"ITEM1\" /></RecordingContent><RequestedExports /></URIObject>";

#[test]
fn a_ready_recording_reads_its_call_organizer_file_and_transcript() {
    let Some(ChatEvent::Recording(ready)) = chat_event("RichText/Media_CallRecording", READY) else {
        panic!("not a recording");
    };
    assert_eq!(ready.call_id.as_deref(), Some("00000000-0000-4000-8000-0000000000c1"));
    assert_eq!(ready.organizer.as_deref(), Some(ANN));
    assert_eq!(ready.tenant.as_deref(), Some("00000000-0000-4000-8000-0000000000f0"));
    assert_eq!(ready.terminator.as_deref(), Some(BEN));
    // 2026-01-02 10:00:10.036 UTC
    assert_eq!(ready.started, Some(1_767_348_010_036));
    assert_eq!(ready.duration, Some(28 * 60 + 38));
    assert_eq!(
        ready.file,
        Some(RecordingItem {
            drive: "b!drive1".to_owned(),
            item: "ITEM1".to_owned()
        })
    );
    assert!(
        ready
            .transcript
            .as_deref()
            .is_some_and(|uri| uri.ends_with("/media/transcripts/t1/content")),
        "{:?}",
        ready.transcript
    );
    // a recording started has none of what its ready message gives
    let Some(ChatEvent::Recording(started)) = chat_event(
        "RichText/Media_CallRecording",
        &recording("Initial", "0", Some("Recording"), "0:00:00"),
    ) else {
        panic!("not a recording");
    };
    assert_eq!((started.file, started.transcript, started.call_id), (None, None, None));
    assert_eq!(started.started, Some(1_767_348_010_000));
}

/// A 1:1 call's recording ready, shaped as a lab call's was: no organizer named, its file in its initiator's OneDrive,
/// no transcript item on it.
const CALL_READY: &str = "<URIObject format_version=\"1.1\" type=\"Video.2/CallRecording.1\" uri=\"\" version=\"1.0\">\
    <RecordingStatus status=\"Success\" code=\"200\" shortChunk=\"False\"><amsErrorResult /></RecordingStatus>\
    <Title>Call with Ben</Title><a href=\"https://madeup-my.sharepoint.com/:v:/g/personal/ann_madeup/MadeUpCall\">Play</a>\
    <OriginalName v=\"Call with Ben-20260102_100010-Meeting Recording.mp4\" /><MeetingOrganizerId value=\"\" />\
    <MeetingOrganizerTenantId value=\"\" />\
    <RecordingInitiatorId value=\"8:orgid:00000000-0000-4000-8000-00000000000a\" isAutoRecording=\"False\" />\
    <RecordingStorage type=\"RecordingInitiatorOneDrive\" userId=\"00000000-0000-4000-8000-00000000000a\" />\
    <Identifiers><Id type=\"callId\" value=\"00000000-0000-4000-8000-0000000000c3\" /></Identifiers>\
    <RecordingContent contentTypes=\"Recording+Transcript\" timestamp=\"2026-01-02T10:00:10.0000000Z\" \
    duration=\"0:00:51.54\" canVideoExpire=\"True\"><item type=\"video\" uri=\"\" />\
    <item type=\"onedriveForBusinessVideo\" uri=\"https://madeup-my.sharepoint.com/:v:/g/personal/ann_madeup/MadeUpCall\" \
    driveId=\"b!drive1\" driveItemId=\"ITEM3\" /></RecordingContent><RequestedExports /></URIObject>";

#[test]
fn a_calls_recording_names_whose_onedrive_holds_it_and_no_organizer() {
    let Some(ChatEvent::Recording(ready)) = chat_event("RichText/Media_CallRecording", CALL_READY) else {
        panic!("not a recording");
    };
    assert_eq!(ready.organizer, None);
    assert_eq!(ready.tenant, None);
    assert_eq!(ready.storage_owner.as_deref(), Some(ANN));
    assert_eq!(ready.duration, Some(51));
    assert_eq!(ready.transcript, None);
    assert_eq!(ready.file.map(|file| file.item).as_deref(), Some("ITEM3"));
    // a meeting's names its organizer and no storage
    let Some(ChatEvent::Recording(meeting)) = chat_event("RichText/Media_CallRecording", READY) else {
        panic!("not a recording");
    };
    assert_eq!(meeting.storage_owner, None);
}

#[test]
fn a_recording_in_a_state_or_with_contents_no_capture_showed_is_no_event() {
    for content in [
        recording("Failure", "500", Some("Recording"), "0:00:00"),
        recording("Success", "202", Some("Recording"), "0:00:00"),
        recording("Success", "200", Some("Recording+Notes"), "0:00:00"),
        "<URIObject><Title>no status</Title></URIObject>".to_owned(),
    ] {
        assert_eq!(chat_event("RichText/Media_CallRecording", &content), None, "{content}");
    }
}

#[test]
fn a_transcript_is_read_with_its_quotes_escaped_or_not() {
    let escaped = r#"{\"scopeId\":\"s\",\"callId\":\"11111111-2222-4333-8444-555555555555\",\"isDeleted\":false,\"isExportedToOdsp\":true}"#;
    let Some(ChatEvent::Transcript(transcript)) = chat_event("RichText/Media_CallTranscript", escaped) else {
        panic!("not a transcript");
    };
    assert_eq!(
        transcript.call_id.as_deref(),
        Some("11111111-2222-4333-8444-555555555555")
    );
    let plain = r#"{"callId":"c","isDeleted":false}"#;
    assert!(matches!(
        chat_event("RichText/Media_CallTranscript", plain),
        Some(ChatEvent::Transcript(_))
    ));
    // deleted, or no word on it: no event
    assert_eq!(
        chat_event("RichText/Media_CallTranscript", r#"{"isDeleted":true}"#),
        None
    );
    assert_eq!(chat_event("RichText/Media_CallTranscript", r#"{"callId":"c"}"#), None);
    assert_eq!(chat_event("RichText/Media_CallTranscript", "not json"), None);
}

#[test]
fn members_joined_and_left_name_each_member() {
    let joined = format!(
        r#"{{"eventtime":1,"initiator":"{ANN}","members":[{{"id":"{BEN}","friendlyname":""}},{{"id":"8:teamsvisitor:abc","friendlyname":"Note Taker"}}]}}"#
    );
    assert_eq!(
        chat_event("ThreadActivity/MemberJoined", &joined),
        Some(ChatEvent::MembersJoined(Members {
            initiator: Some(ANN.to_owned()),
            members: vec![
                Member {
                    mri: BEN.to_owned(),
                    name: None
                },
                Member {
                    mri: "8:teamsvisitor:abc".to_owned(),
                    name: Some("Note Taker".to_owned())
                },
            ],
            temporary: false,
        }))
    );
    let left = format!(
        r#"{{"eventtime":1,"initiator":"{ANN}","members":[{{"id":"{BEN}","friendlyname":"Ben","meetingMemberType":"temp"}}]}}"#
    );
    assert!(matches!(
        chat_event("ThreadActivity/MemberLeft", &left),
        Some(ChatEvent::MembersLeft(Members { temporary: true, .. }))
    ));
    assert_eq!(chat_event("ThreadActivity/MemberLeft", r#"{"members":[]}"#), None);
    assert_eq!(
        chat_event("ThreadActivity/MemberLeft", r#"{"members":[{"id":7}]}"#),
        None
    );
}

#[test]
fn the_threads_xml_events_are_read() {
    let removed = format!(
        "<deletemember><eventtime>1</eventtime><initiator>{ANN}</initiator><target>{BEN}</target></deletemember>"
    );
    assert_eq!(
        chat_event("ThreadActivity/DeleteMember", &removed),
        Some(ChatEvent::MembersRemoved(Removed {
            initiator: ANN.to_owned(),
            targets: vec![BEN.to_owned()]
        }))
    );
    let left = format!("<deletemember><initiator>{ANN}</initiator><target>{ANN}</target></deletemember>");
    assert_eq!(
        chat_event("ThreadActivity/DeleteMember", &left),
        Some(ChatEvent::MembersRemoved(Removed {
            initiator: ANN.to_owned(),
            targets: Vec::new()
        }))
    );
    let named = format!(
        "<topicupdate><eventtime>1</eventtime><initiator>{ANN}</initiator><value>Weekly sync</value></topicupdate>"
    );
    assert_eq!(
        chat_event("ThreadActivity/TopicUpdate", &named),
        Some(ChatEvent::TopicUpdated(TopicUpdate {
            initiator: ANN.to_owned(),
            topic: Some("Weekly sync".to_owned())
        }))
    );
    let policy = format!(
        "<meetingpolicyupdated><eventtime>1</eventtime><initiator>{MEETING}</initiator><value>ChatEnabled</value></meetingpolicyupdated>"
    );
    assert_eq!(
        chat_event("ThreadActivity/MeetingPolicyUpdated", &policy),
        Some(ChatEvent::MeetingChatPolicy(ChatPolicy::Enabled))
    );
    let unknown = "<meetingpolicyupdated><initiator>x</initiator><value>SomethingNew</value></meetingpolicyupdated>";
    assert_eq!(chat_event("ThreadActivity/MeetingPolicyUpdated", unknown), None);
    // a type no capture showed
    assert_eq!(chat_event("ThreadActivity/RoleUpdate", "<roleupdate/>"), None);
    assert_eq!(chat_event("RichText/Html", "<p>hi</p>"), None);
}

#[test]
fn an_addition_reads_how_far_back_history_was_shared() {
    let added = format!(
        "<addmember><eventtime>172800000</eventtime><initiator>{ANN}</initiator><target>{BOT}</target>\
         <detailedtargetinfo><id>{BOT}</id><sharehistorytime>0</sharehistorytime></detailedtargetinfo></addmember>"
    );
    let activity = thread_activity(&added).expect("an addition");
    assert_eq!(activity.share_history_time, Field::Known(0));
    assert_eq!(activity.targets, vec![BOT.to_owned()]);
    let without = format!("<addmember><initiator>{ANN}</initiator><target>{BEN}</target></addmember>");
    assert_eq!(
        thread_activity(&without).expect("an addition").share_history_time,
        Field::Absent
    );
}
