//! A made-up meeting for the recap's tests: its chat and messages (the meeting's start and end, a recording ready, a
//! file and a link shared), what MCPS answers for it (two recordings with their transcripts) and for a meeting with a
//! transcript alone, and a transcript's `WebVTT`. Every id, host, name and word here is made up.

use teams_proto::recap::{ContentKind, MeetingContent};
use teams_store::{Conversation, ConversationKind, Message};

pub const MEETING: &str = "19:meeting_bWFkZS11cC1yZWNhcA@thread.v2";
pub const TRANSCRIPT_ONLY: &str = "19:meeting_dHJhbnNjcmlwdC1vbmx5@thread.v2";
pub const TITLE: &str = "Weekly sync";
pub const ORGANIZER: &str = "00000000-0000-4000-8000-00000000000a";
pub const SITE: &str = "https://madeup-my.sharepoint.com/personal/lab_a/";
/// 2026-01-02 10:00 UTC.
pub const START: i64 = 1_767_348_000_000;

pub fn transcript_location(item: &str) -> String {
    format!("{SITE}_api/v2.1/drives/b!drive1/items/{item}/versions/current/media/transcripts/t-{item}/content")
}

pub fn sharing_link(item: &str) -> String {
    format!("https://madeup-my.sharepoint.com/:v:/g/personal/lab_a/{item}")
}

fn content(kind: ContentKind, item: &str, from: i64, to: i64) -> MeetingContent {
    MeetingContent {
        kind,
        location: match kind {
            ContentKind::Recording => sharing_link(item),
            ContentKind::Transcript => transcript_location(item),
        },
        call_id: Some("00000000-0000-4000-8000-0000000000c1".to_owned()),
        drive_id: Some("b!drive1".to_owned()),
        item_id: Some(item.to_owned()),
        start_ms: Some(from),
        end_ms: Some(to),
        web_url: (kind == ContentKind::Transcript).then(|| sharing_link(item)),
        organizer_id: Some(ORGANIZER.to_owned()),
        tenant_id: Some("00000000-0000-4000-8000-0000000000f0".to_owned()),
    }
}

/// Two recordings, each with its transcript, as MCPS lists them (not in order).
pub fn recorded() -> Vec<MeetingContent> {
    let minute = 60_000;
    vec![
        content(ContentKind::Transcript, "ITEM2", START + 4 * minute, START + 6 * minute),
        content(ContentKind::Recording, "ITEM1", START, START + 2 * minute),
        content(ContentKind::Transcript, "ITEM1", START, START + 2 * minute),
        content(ContentKind::Recording, "ITEM2", START + 4 * minute, START + 6 * minute),
    ]
}

/// A meeting transcribed and not recorded.
pub fn transcribed() -> Vec<MeetingContent> {
    vec![content(ContentKind::Transcript, "ITEM9", START, START + 120_000)]
}

/// A transcript of two speakers, the first speaking twice in a row (two utterances), as Teams writes one.
pub const VTT: &str = "WEBVTT\n\n\
    u1/4-0\n00:00:03.791 --> 00:00:04.191\n<v Ada Lab>Today.</v>\n\n\
    u1/5-0\n00:00:07.271 --> 00:00:10.585\n<v Ada Lab>Hello everyone,\nthis is the weekly sync.</v>\n\n\
    u1/5-1\n00:00:10.585 --> 00:00:14.188\n<v Ada Lab>We look at the video player first.</v>\n\n\
    u1/6-0\n00:00:15.000 --> 00:00:19.000\n<v Bo Lab>Yes, the video plays, and the transcript follows the video.</v>\n\n\
    u1/7-0\n00:00:20.000 --> 00:00:26.000\n<v Ada Lab>Then the search: it finds every video we named.</v>";

pub fn conversation(id: &str) -> Conversation {
    Conversation {
        id: id.to_owned(),
        kind: ConversationKind::Meeting,
        topic: Some(TITLE.to_owned()),
        version: 1,
        last_activity: Some(START + 600_000),
    }
}

fn message(id: &str, sender: &str, kind: &str, content: &str, properties: Option<&str>, minute: i64) -> Message {
    Message {
        conversation_id: MEETING.to_owned(),
        id: id.to_owned(),
        version: 1,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: Some("Ada Lab".to_owned()),
        message_type: kind.to_owned(),
        content: content.to_owned(),
        properties: properties.map(str::to_owned),
        arrival_time: START + minute * 60_000,
        compose_time: None,
        edit_time: None,
        delete_time: None,
    }
}

/// The meeting chat's messages, the newest first: the second recording ready, a link, a file, the first recording
/// ready, the meeting's start.
pub fn messages(me: &str) -> Vec<Message> {
    let file = r#"[{"@type":"http://schema.skype.com/File","version":2,"id":"f1","baseUrl":"https://madeup-my.sharepoint.com/personal/lab_a/","type":"pdf","title":"Agenda.pdf","state":"active","objectUrl":"https://madeup-my.sharepoint.com/personal/lab_a/Documents/Agenda.pdf","itemid":"f1","fileName":"Agenda.pdf","fileType":"pdf","fileInfo":{"fileUrl":"https://madeup-my.sharepoint.com/personal/lab_a/Documents/Agenda.pdf","siteUrl":"https://madeup-my.sharepoint.com/personal/lab_a/","serverRelativeUrl":"","shareUrl":"https://madeup-my.sharepoint.com/:b:/g/personal/lab_a/agenda","shareId":"s1"},"permissionScope":"users"}]"#;
    let recording = |item: &str| {
        format!(
            "<URIObject type=\"Video.2/CallRecording.1\"><RecordingStatus status=\"Success\" code=\"200\" />\
             <Title>{TITLE}</Title><a href=\"{}\">Play</a><OriginalName v=\"{TITLE}-Meeting Recording.mp4\" />\
             <RecordingInitiatorId value=\"8:orgid:{ORGANIZER}\" isAutoRecording=\"False\" />\
             <RecordingContent contentTypes=\"Recording+Transcript\" duration=\"0:02:00.0\">\
             <item type=\"onedriveForBusinessVideo\" uri=\"{0}\" driveId=\"b!drive1\" driveItemId=\"{item}\" />\
             </RecordingContent></URIObject>",
            sharing_link(item)
        )
    };
    vec![
        message(
            "5",
            &format!("8:orgid:{ORGANIZER}"),
            "RichText/Media_CallRecording",
            &recording("ITEM2"),
            None,
            10,
        ),
        message(
            "4",
            me,
            "RichText/Html",
            "<p>Notes are on <a href=\"https://example.com/notes\">the wiki</a></p>",
            None,
            9,
        ),
        message(
            "3",
            me,
            "RichText/Html",
            "<p>The agenda</p>",
            Some(&format!("{{\"files\":{file:?}}}")),
            8,
        ),
        message(
            "2",
            &format!("8:orgid:{ORGANIZER}"),
            "RichText/Media_CallRecording",
            &recording("ITEM1"),
            None,
            7,
        ),
        // the meeting's end, its recordings saved after it: its recap card holds them
        message(
            "6",
            &format!("8:orgid:{ORGANIZER}"),
            "Event/Call",
            &format!(
                "<ended/><partlist alt=\"\" count=\"1\"><part identity=\"8:orgid:{ORGANIZER}\">\
                 <name>8:orgid:{ORGANIZER}</name><duration>360</duration></part></partlist>\
                 <callEventType>callEnded</callEventType>"
            ),
            None,
            6,
        ),
        message(
            "1",
            &format!("8:orgid:{ORGANIZER}"),
            "Event/Call",
            "<partlist alt =\"\"></partlist><meetingDetails><meetingDetails><meetingType>Scheduled</meetingType>\
             </meetingDetails></meetingDetails>",
            None,
            0,
        ),
    ]
}
