//! A meeting's recap contents (MCPS's `contents/` answer) read into the parts the recap shows, where a transcript's
//! `WebVTT` is read, and the link Share → Copy link copies. Every id, host and name here is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use teams_proto::recap::{
    ContentKind, MCPS_AUDIENCE, MeetingContent, MeetingContents, TranscriptLocation, recap_link, recap_parts,
    recap_span,
};
use teams_proto::{Read as _, Service};

const SITE: &str = "https://madeup-my.sharepoint.com/personal/lab_a/";
const THREAD: &str = "19:meeting_bWFkZS11cA@thread.v2";

fn transcript_location(item: &str, id: &str) -> String {
    format!("{SITE}_api/v2.1/drives/b!drive1/items/{item}/versions/current/media/transcripts/{id}/content")
}

fn resource(kind: &str, item: &str, location: &str, start: &str, end: &str) -> serde_json::Value {
    serde_json::json!({
        "id": format!("call-{item}-{kind}"),
        "type": kind,
        "location": location,
        "metadata": {
            "threadId": THREAD,
            "callId": "00000000-0000-4000-8000-0000000000c1",
            "driveId": "b!drive1",
            "driveItemId": item,
            // 2026-01-02 10:00:00 UTC and on, in .NET ticks
            "startTime": start,
            "endTime": end,
            "webUrl": if kind == "TranscriptV2" { "https://madeup-my.sharepoint.com/:v:/g/personal/lab_a/share1" } else { "" },
            "organizerId": "00000000-0000-4000-8000-00000000000a",
            "ownerTenantId": "00000000-0000-4000-8000-0000000000f0",
            "transcriptId": "t1"
        }
    })
}

fn answer() -> Vec<u8> {
    serde_json::json!({
        "changeKey": "",
        "resources": [
            resource("TranscriptV2", "ITEM2", &transcript_location("ITEM2", "t2"), "639029448600000000", "639029449200000000"),
            resource("TranscriptV2", "ITEM1", &transcript_location("ITEM1", "t1"), "639029448000000000", "639029448300000000"),
            resource("Recording", "ITEM1", "https://madeup-my.sharepoint.com/:v:/g/personal/lab_a/share1", "639029448000000000", "639029448300000000"),
            resource("Recording", "ITEM2", "https://madeup-my.sharepoint.com/:v:/g/personal/lab_a/share2", "639029448600000000", "639029449200000000"),
            resource("Notes", "ITEM3", "https://madeup-my.sharepoint.com/notes", "639029448000000000", "639029448300000000"),
            resource("TranscriptV2", "ITEM4", "http://madeup-my.sharepoint.com/plain", "1", "2"),
            resource("TranscriptV2", "ITEM9", &transcript_location("ITEM9", "t9"), "639029450000000000", "639029454600000000")
        ],
        "invalidResources": [],
        "paginationInfo": {"hasNextPage": false}
    })
    .to_string()
    .into_bytes()
}

#[test]
fn the_read_asks_mcps_for_the_meetings_chat() {
    let read = MeetingContents::of(THREAD);
    assert_eq!(read.service(), Service::Mcps);
    assert_eq!(read.endpoint().segments(), ["contents"]);
    assert_eq!(read.endpoint().query(), [("threadId".to_owned(), THREAD.to_owned())]);
    assert_eq!(MCPS_AUDIENCE, "6bc3b958-689b-49f5-9006-36d165f30e00");
}

#[test]
fn reads_the_recordings_and_transcripts_an_answer_lists() {
    let contents = MeetingContents::read_answer(&answer()).expect("an answer");
    let kinds: Vec<_> = contents
        .iter()
        .map(|content| (content.kind, content.item_id.as_deref()))
        .collect();
    // a notes resource and a transcript off https are left out
    assert_eq!(
        kinds,
        [
            (ContentKind::Transcript, Some("ITEM2")),
            (ContentKind::Transcript, Some("ITEM1")),
            (ContentKind::Recording, Some("ITEM1")),
            (ContentKind::Recording, Some("ITEM2")),
            (ContentKind::Transcript, Some("ITEM9")),
        ]
    );
    let first = &contents[2];
    assert_eq!(first.start_ms, Some(1_767_348_000_000));
    assert_eq!(first.end_ms, Some(1_767_348_030_000));
    assert_eq!(
        first.sharing_link(),
        Some("https://madeup-my.sharepoint.com/:v:/g/personal/lab_a/share1")
    );
    assert_eq!(
        first.organizer_id.as_deref(),
        Some("00000000-0000-4000-8000-00000000000a")
    );
    assert_eq!(first.tenant_id.as_deref(), Some("00000000-0000-4000-8000-0000000000f0"));
    assert_eq!(MeetingContents::read_answer(b"not json"), None);
    assert_eq!(MeetingContents::read_answer(b"{\"resources\":{}}"), None);
    assert_eq!(MeetingContents::read_answer(b"{\"resources\":[]}"), Some(Vec::new()));
}

#[test]
fn pairs_each_recording_with_its_transcript_earliest_first() {
    let contents = MeetingContents::read_answer(&answer()).expect("an answer");
    let parts = recap_parts(&contents);
    let shown: Vec<_> = parts
        .iter()
        .map(|part| {
            (
                part.item_id.as_deref(),
                part.recording.is_some(),
                part.transcript.is_some(),
            )
        })
        .collect();
    // two recordings with their transcripts, then a transcript no recording holds
    assert_eq!(
        shown,
        [
            (Some("ITEM1"), true, true),
            (Some("ITEM2"), true, true),
            (Some("ITEM9"), false, true)
        ]
    );
    assert_eq!(
        parts[1].transcript.as_deref(),
        Some(transcript_location("ITEM2", "t2").as_str())
    );
    // the transcript-only part plays nothing but keeps its item's sharing link
    assert_eq!(
        parts[2].sharing_link.as_deref(),
        Some("https://madeup-my.sharepoint.com/:v:/g/personal/lab_a/share1")
    );
    assert_eq!(recap_span(&parts), Some((1_767_348_000_000, 1_767_348_660_000)));
    assert_eq!(recap_span(&[]), None);
}

#[test]
fn a_transcript_is_read_as_webvtt_on_its_personal_site() {
    let location = TranscriptLocation::of(&transcript_location("ITEM1", "t1")).expect("a location");
    assert_eq!(location.site().base(), SITE);
    assert_eq!(location.site().audience(), "https://madeup-my.sharepoint.com");
    assert_eq!(
        location.url(),
        format!(
            "{SITE}_api/v2.1/drives/b!drive1/items/ITEM1/media/transcripts/t1/streamContent?is=1&applymediaedits=false"
        )
    );
    let without_version = format!("{SITE}_api/v2.1/drives/d/items/i/media/transcripts/t/content");
    assert!(TranscriptLocation::of(&without_version).is_some());
    for refused in [
        // a team site, another shape, a query, a step out of the path
        "https://madeup.sharepoint.com/sites/x/_api/v2.1/drives/d/items/i/media/transcripts/t/content".to_owned(),
        format!("{SITE}_api/v2.1/drives/d/items/i/content"),
        format!("{SITE}_api/v2.1/drives/d/items/i/media/transcripts/t/content?x=1"),
        format!("{SITE}_api/v2.1/drives/d/items/i/media/transcripts/a.b/content"),
        "https://elsewhere.example/personal/lab_a/_api/v2.1/drives/d/items/i/media/transcripts/t/content".to_owned(),
    ] {
        assert_eq!(TranscriptLocation::of(&refused), None, "{refused}");
    }
}

#[test]
fn the_recap_link_names_the_part_and_its_meeting() {
    let contents = MeetingContents::read_answer(&answer()).expect("an answer");
    let parts = recap_parts(&contents);
    let link = recap_link(&parts[0], THREAD).expect("a link");
    let url = url::Url::parse(&link).expect("a URL");
    assert_eq!(url.host_str(), Some("teams.microsoft.com"));
    assert_eq!(url.path(), "/l/meetingrecap");
    let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    let get = |name: &str| {
        pairs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(get("driveItemId"), Some("ITEM1"));
    assert_eq!(
        get("sitePath"),
        Some("https://madeup-my.sharepoint.com/:v:/g/personal/lab_a/share1")
    );
    assert_eq!(get("threadId"), Some(THREAD));
    assert_eq!(get("recapType"), Some("RecordingAndTranscript"));
    assert_eq!(get("subType"), Some("RecapSharingLink_RecapCore"));
    let only_transcript = recap_link(&parts[2], THREAD).expect("a link");
    assert!(only_transcript.ends_with("recapType=Transcript"));
    let mut bare = parts[0].clone();
    bare.item_id = None;
    assert_eq!(recap_link(&bare, THREAD), None);
}

#[test]
fn a_content_without_times_or_metadata_still_reads() {
    let body = br#"{"resources":[{"type":"Recording","location":"https://madeup-my.sharepoint.com/:v:/g/x"},
        {"type":"Recording","location":"https://madeup-my.sharepoint.com/:v:/g/y","metadata":{"startTime":"12x","endTime":"5"}}]}"#;
    let contents: Vec<MeetingContent> = MeetingContents::read_answer(body).expect("an answer");
    assert_eq!(contents.len(), 2);
    assert_eq!(contents[0].start_ms, None);
    assert_eq!(contents[1].start_ms, None);
    // ticks before the epoch are no time
    assert_eq!(contents[1].end_ms, None);
    // items unknown: each is a part of its own, the ones with no time last
    assert_eq!(recap_parts(&contents).len(), 2);
}
