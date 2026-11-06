//! A meeting chat's recap card worked out from its messages (`teams_ui::meeting_recap`): what each call left (its
//! recordings, its transcript, a recording still saved), its line (the meeting's scheduled times, else its first
//! recording's, else its transcript's, else the call's own), its thumbnail's badge and expiry, and Share's link. Every
//! id, host and value here is made up.

// test code throughout (it unwraps like the tests do)
#![cfg(test)]

use chrono::FixedOffset;
use teams_store::Message;
use teams_ui::clock::HourCycle;
use teams_ui::meeting_recap::{SAVING, SET_TO_EXPIRE, recap_cards};

const MEETING: &str = "19:meeting_bWFkZS11cA@thread.v2";
const CALL: &str = "00000000-0000-4000-8000-0000000000c1";
const ORGANIZER: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
/// 2026-10-19 15:30 UTC.
const START: i64 = 1_792_423_800_000;
const MINUTE: i64 = 60_000;
const DAY: i64 = 24 * 60 * MINUTE;

fn message(id: &str, kind: &str, content: &str, minute: i64) -> Message {
    Message {
        conversation_id: MEETING.to_owned(),
        id: id.to_owned(),
        version: 1,
        client_message_id: None,
        sender: Some(ORGANIZER.to_owned()),
        sender_name: None,
        message_type: kind.to_owned(),
        content: content.to_owned(),
        properties: None,
        arrival_time: START + minute * MINUTE,
        compose_time: None,
        edit_time: None,
        delete_time: None,
    }
}

/// A recording's message of `call`: `item` its file once ready, begun at `started` (`HH:MM` on 2026-10-19, UTC).
fn recording(call: &str, status: &str, contents: &str, item: &str, started: &str, length: &str) -> String {
    let code = match status {
        "Initial" => "0",
        "ChunkFinished" => "202",
        _ => "200",
    };
    let link = format!("https://madeup-my.sharepoint.com/:v:/g/personal/ann_madeup/{item}");
    let (href, file) = if status == "Success" {
        (
            link.clone(),
            format!(
                "<item type=\"onedriveForBusinessVideo\" uri=\"{link}\" driveId=\"b!drive1\" driveItemId=\"{item}\" />"
            ),
        )
    } else {
        (String::new(), String::new())
    };
    format!(
        "<URIObject type=\"Video.2/CallRecording.1\"><RecordingStatus status=\"{status}\" code=\"{code}\" />\
         <Title>Evolve call</Title><a href=\"{href}\">Play</a><MeetingOrganizerId value=\"{ORGANIZER}\" />\
         <MeetingOrganizerTenantId value=\"00000000-0000-4000-8000-0000000000f0\" />\
         <Identifiers><Id type=\"callId\" value=\"{call}\" /></Identifiers>\
         <RecordingContent contentTypes=\"{contents}\" timestamp=\"2026-10-19T{started}:00.0000000Z\" duration=\"{length}\">\
         {file}</RecordingContent></URIObject>"
    )
}

fn ended(call: &str, details: &str) -> String {
    format!(
        "<ended/><partlist alt=\"\" count=\"1\"><part identity=\"{ORGANIZER}\"><name>{ORGANIZER}</name>\
         <duration>3697</duration></part></partlist><meetingDetails><meetingDetails>{details}</meetingDetails>\
         </meetingDetails><callEventType>callEnded</callEventType><callId>{call}</callId>"
    )
}

const MEET_NOW: &str = "<meetingType>MeetNow</meetingType><startTime>01/01/0001 00:00:00</startTime>\
    <endTime>01/01/0001 00:00:00</endTime>";

#[test]
fn a_scheduled_meeting_s_card_shows_its_schedule_its_recording_and_transcript() {
    let scheduled = "<meetingType>Scheduled</meetingType><startTime>10/19/2026 15:30:00</startTime>\
        <endTime>10/19/2026 16:00:00</endTime>";
    let messages = vec![
        message(
            "1",
            "RichText/Media_CallRecording",
            &recording(CALL, "Initial", "Recording", "", "15:31", "0:00:00"),
            1,
        ),
        message("2", "Event/Call", &ended(CALL, scheduled), 29),
        message(
            "3",
            "RichText/Media_CallRecording",
            &recording(CALL, "ChunkFinished", "Recording", "", "15:31", "0:28:38"),
            29,
        ),
        message(
            "4",
            "RichText/Media_CallRecording",
            &recording(CALL, "Success", "Recording+Transcript", "ITEM1", "15:31", "0:28:38.5"),
            30,
        ),
    ];
    let cards = recap_cards(&messages);
    let card = &cards[&Some(CALL.to_owned())];
    assert!(card.shows());
    assert!(card.transcript);
    assert!(!card.saving);
    assert_eq!(card.recordings.len(), 1);
    assert_eq!(card.badge().as_deref(), Some("28m 38s"));
    let utc = FixedOffset::east_opt(0).unwrap();
    assert_eq!(
        card.span_line(&utc, HourCycle::Twelve).as_deref(),
        Some("Monday, October 19, 2026 3:30 PM - 4:00 PM")
    );
    let now = START + 18 * DAY;
    assert_eq!(card.expiry_line(START + 120 * DAY, now), "Expires in 102 days");
    assert_eq!(card.expiry_line(now + 1, now), "Expires in 1 day");
    assert_eq!(card.expiry_line(now - 1, now), "Expired");
    let link = card.share_link(MEETING).unwrap();
    for part in [
        "driveId=b%21drive1",
        "driveItemId=ITEM1",
        "organizerId=00000000-0000-4000-8000-00000000000a",
        "recapType=Recording",
    ] {
        assert!(link.contains(part), "{part} in {link}");
    }
}

#[test]
fn a_meeting_not_scheduled_takes_its_first_recording_s_span_and_tells_several() {
    let messages = vec![
        message("1", "Event/Call", &ended(CALL, MEET_NOW), 10),
        message(
            "2",
            "RichText/Media_CallRecording",
            &recording(CALL, "Success", "Recording", "ITEM2", "15:36", "0:01:00"),
            11,
        ),
        message(
            "3",
            "RichText/Media_CallRecording",
            &recording(CALL, "Success", "Recording", "ITEM1", "15:31", "0:02:00"),
            12,
        ),
    ];
    let card = &recap_cards(&messages)[&Some(CALL.to_owned())];
    // the earliest first, whatever order they came in
    assert!(card.recordings[0].link.ends_with("ITEM1"));
    assert_eq!(card.span, Some((START + MINUTE, START + 3 * MINUTE)));
    assert_eq!(card.badge().as_deref(), Some("2 recordings"));
    assert_eq!(card.expiry_line(START + 120 * DAY, START), SET_TO_EXPIRE);
    assert!(!card.transcript);
}

#[test]
fn a_recording_still_saved_shows_saving_and_a_transcript_alone_its_own_span() {
    let other = "00000000-0000-4000-8000-0000000000c9";
    let messages = vec![
        message(
            "1",
            "RichText/Media_CallRecording",
            &recording(CALL, "Success", "Recording", "ITEM1", "15:31", "0:02:00"),
            3,
        ),
        message(
            "2",
            "RichText/Media_CallRecording",
            &recording(CALL, "ChunkFinished", "Recording", "", "15:31", "0:02:00"),
            3,
        ),
        message(
            "3",
            "RichText/Media_CallRecording",
            &recording(CALL, "ChunkFinished", "Recording", "", "15:36", "0:01:00"),
            7,
        ),
        message("4", "Event/Call", &ended(CALL, MEET_NOW), 8),
        // another call: transcribed, not recorded
        message(
            "5",
            "RichText/Media_CallTranscript",
            &format!(r#"{{\"callId\":\"{other}\",\"isDeleted\":false}}"#),
            20,
        ),
        message(
            "6",
            "RichText/Media_CallRecording",
            &recording(other, "Success", "Transcript", "ITEM9", "15:51", "0:01:11"),
            22,
        ),
        message("7", "Event/Call", &ended(other, MEET_NOW), 22),
    ];
    let cards = recap_cards(&messages);
    let saving = &cards[&Some(CALL.to_owned())];
    assert!(saving.saving);
    assert!(saving.has_picture());
    assert_eq!(saving.badge().as_deref(), Some(SAVING));
    // one ready and one saved: its expiry is still told
    assert_eq!(saving.expiry_line(START + 120 * DAY, START), "Expires in 120 days");
    let transcribed = &cards[&Some(other.to_owned())];
    assert!(transcribed.shows());
    assert!(transcribed.transcript);
    assert!(!transcribed.has_picture());
    assert_eq!(
        transcribed.span,
        Some((START + 21 * MINUTE, START + 22 * MINUTE + 11_000))
    );
    assert!(
        transcribed
            .share_link(MEETING)
            .is_some_and(|link| link.contains("recapType=Transcript")),
        "{:?}",
        transcribed.share_link(MEETING)
    );
}

#[test]
fn a_meeting_that_left_nothing_has_no_card() {
    let messages = vec![
        message(
            "1",
            "Event/Call",
            &format!(
                "<partlist alt =\"\"></partlist><callEventType>callStarted</callEventType><callId>{CALL}</callId>"
            ),
            0,
        ),
        message("2", "Event/Call", &ended(CALL, MEET_NOW), 5),
    ];
    let card = &recap_cards(&messages)[&Some(CALL.to_owned())];
    assert!(!card.shows());
    // with no recording, the call's own start and end
    assert_eq!(card.span, Some((START, START + 5 * MINUTE)));
}
