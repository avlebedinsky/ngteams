//! What a meeting chat's Recap and Shared tabs show (`teams_ui::recap`), worked out from synthetic data: the
//! meeting's line, its parts, the transcript's paragraphs and search, and what the chat shared.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

#[path = "support/recap_fixtures.rs"]
#[expect(dead_code, reason = "fixtures shared with the recap view tests")]
mod fixtures;

use std::time::Duration;

use chrono::FixedOffset;
use teams_proto::recap::recap_parts;
use teams_ui::clock::HourCycle;
use teams_ui::recap::{
    Search, SharedFilter, SharedKind, Transcript, filtered, listed, matched_ranges, part_label, session_line, sessions,
    shared_items, shows_parts, transcript_file_name,
};

const ME: &str = "8:orgid:00000000-0000-4000-8000-0000000000aa";

#[test]
fn the_meetings_line_spans_its_parts_on_the_accounts_clock() {
    let parts = recap_parts(&fixtures::recorded());
    let utc = FixedOffset::east_opt(0).expect("a zone");
    assert_eq!(
        session_line(&parts, &utc, HourCycle::Twelve).as_deref(),
        Some("Friday, January 2, 2026 10:00 AM - 10:06 AM")
    );
    let plus_three = FixedOffset::east_opt(3 * 3600).expect("a zone");
    assert_eq!(
        session_line(&parts, &plus_three, HourCycle::TwentyFour).as_deref(),
        Some("Friday, January 2, 2026 13:00 - 13:06")
    );
    assert_eq!(session_line(&[], &utc, HourCycle::Twelve), None);
}

#[test]
fn two_recordings_are_two_parts_a_transcript_alone_one() {
    let parts = recap_parts(&fixtures::recorded());
    assert!(shows_parts(&parts));
    assert_eq!(part_label(0), "Part 1");
    assert_eq!(
        parts[0].recording.as_deref(),
        Some(fixtures::sharing_link("ITEM1").as_str())
    );
    assert_eq!(
        parts[1].transcript.as_deref(),
        Some(fixtures::transcript_location("ITEM2").as_str())
    );
    let alone = recap_parts(&fixtures::transcribed());
    assert!(!shows_parts(&alone));
    assert_eq!(alone[0].recording, None);
}

#[test]
fn a_1_1_chats_calls_are_sessions_of_their_own_the_latest_first() {
    let mut parts = recap_parts(&fixtures::recorded());
    // a meeting's parts are its one session
    assert_eq!(
        sessions(&parts, false, None),
        vec![(0..parts.len()).collect::<Vec<_>>()]
    );
    assert_eq!(sessions(&[], true, None), Vec::<Vec<usize>>::new());
    // a 1:1 chat's: each call its own, a part naming no call (a transcript saved here) one of its own
    let call = |id: &str| Some(format!("00000000-0000-4000-8000-0000000000{id}"));
    parts[0].call_id = call("c1");
    parts[1].call_id = call("c1");
    let mut saved = parts[1].clone();
    saved.call_id = None;
    parts.push(saved);
    let mut later = parts[0].clone();
    later.call_id = call("c2");
    parts.push(later);
    assert_eq!(sessions(&parts, true, None), vec![vec![3], vec![2], vec![0, 1]]);
}

#[test]
fn a_call_transcribed_alone_is_left_out_until_its_card_asks_for_it() {
    let call = |id: &str| Some(format!("00000000-0000-4000-8000-0000000000{id}"));
    let mut parts = recap_parts(&fixtures::recorded());
    parts.truncate(1);
    parts[0].call_id = call("c1");
    // a transcript with nothing to read of the recorded call, then a later call transcribed alone
    let unread = |call_id: Option<String>, start: i64| teams_proto::recap::RecapPart {
        recording: None,
        transcript: None,
        sharing_link: Some(format!(
            "https://madeup-my.sharepoint.com/:v:/g/personal/lab_a/t{start}"
        )),
        call_id,
        drive_id: Some("b!drive1".to_owned()),
        item_id: Some(format!("T{start}")),
        start_ms: Some(start),
        end_ms: Some(start + 60_000),
        organizer_id: None,
        tenant_id: None,
    };
    let first = parts[0].start_ms.expect("a start");
    parts.insert(0, unread(call("c1"), first - 120_000));
    parts.push(unread(call("c2"), first + 3_600_000));
    // the Recap lists the recording alone, as Teams' does
    assert_eq!(sessions(&parts, true, None), vec![vec![1]]);
    assert!(listed(&parts, &[1]));
    // its card asks: the recorded call's transcribed part joins its call, the call transcribed alone is its own
    assert_eq!(sessions(&parts, true, Some(0)), vec![vec![0, 1]]);
    assert_eq!(sessions(&parts, true, Some(2)), vec![vec![2], vec![1]]);
    assert!(!listed(&parts, &[2]), "a call transcribed alone isn't in the menu");
    assert!(listed(&parts, &[0, 1]));
    // a meeting's: the same, in its one session
    assert_eq!(sessions(&parts, false, None), vec![vec![1]]);
    // only calls transcribed alone: all shown rather than none
    let alone = vec![unread(call("c1"), first), unread(call("c2"), first + 60_000)];
    assert_eq!(sessions(&alone, true, None), vec![vec![1], vec![0]]);
    assert!(listed(&alone, &[0]));
}

#[test]
fn the_transcript_follows_the_recording_by_paragraph() {
    let transcript = Transcript::of(fixtures::VTT).expect("a transcript");
    assert_eq!(transcript.turns.len(), 3);
    assert_eq!(transcript.turns[0].paragraphs.len(), 2);
    assert_eq!(transcript.spoken_at(Duration::from_secs(1)), None);
    assert_eq!(transcript.spoken_at(Duration::from_secs(12)), Some((0, 1)));
    assert_eq!(transcript.spoken_at(Duration::from_secs(16)), Some((1, 0)));
    assert_eq!(Transcript::of("not a transcript"), None);
}

#[test]
fn a_search_counts_its_matches_and_goes_round_them() {
    let transcript = Transcript::of(fixtures::VTT).expect("a transcript");
    let mut search = Search::new(&transcript, " Video ");
    assert_eq!(search.query, "Video");
    // each place counts: Bo's paragraph names it twice
    assert_eq!(search.label(), "1/4");
    assert_eq!(search.shown(), Some((0, 1)));
    search.next();
    assert_eq!(search.shown(), Some((1, 0)));
    assert_eq!(search.current_range(1, 0), Some(9..14));
    assert_eq!(search.current_range(0, 1), None);
    search.next();
    assert_eq!(search.shown(), Some((1, 0)));
    assert_eq!(search.current_range(1, 0), Some(53..58));
    search.next();
    search.next();
    assert_eq!(search.label(), "1/4");
    search.previous();
    assert_eq!(search.label(), "4/4");
    let mut none = Search::new(&transcript, "nobody said it");
    none.next();
    none.previous();
    assert_eq!(none.label(), "0/0");
    assert_eq!(none.shown(), None);
}

#[test]
fn each_paragraph_has_a_row_of_its_own() {
    let transcript = Transcript::of(fixtures::VTT).expect("a transcript");
    // the note, who started it, Ada's header, her two paragraphs, Bo's header...
    assert_eq!(teams_ui::recap::row_of(&transcript.turns, (0, 0)), 3);
    assert_eq!(teams_ui::recap::row_of(&transcript.turns, (0, 1)), 4);
    assert_eq!(teams_ui::recap::row_of(&transcript.turns, (1, 0)), 6);
    assert_eq!(teams_ui::recap::row_of(&transcript.turns, (2, 0)), 8);
}

#[test]
fn a_match_is_highlighted_where_it_stands() {
    assert_eq!(matched_ranges("The Video and the video", "video"), [4..9, 18..23]);
    assert!(matched_ranges("anything", "  ").is_empty());
    // a text whose lowercase is longer is matched as written
    assert_eq!(
        matched_ranges("İstanbul video", "video"),
        vec![std::ops::Range { start: 10, end: 15 }]
    );
}

#[test]
fn the_chat_shared_a_file_and_a_link_newest_first() {
    let items = shared_items(&fixtures::messages(ME));
    let names: Vec<_> = items.iter().map(|item| (item.name.as_str(), item.is_file())).collect();
    // each recording ready is listed as its link, as Teams lists it
    let (first, second) = (fixtures::sharing_link("ITEM1"), fixtures::sharing_link("ITEM2"));
    assert_eq!(
        names,
        [
            (second.as_str(), false),
            ("the wiki", false),
            ("Agenda.pdf", true),
            (first.as_str(), false)
        ]
    );
    assert_eq!(items[1].url, "https://example.com/notes");
    assert!(matches!(&items[2].kind, SharedKind::File(file) if file.extension == "pdf"));
    assert_eq!(
        items[2].url,
        "https://madeup-my.sharepoint.com/:b:/g/personal/lab_a/agenda"
    );
    assert_eq!(items[2].sender.as_deref(), Some(ME));
    assert_eq!(filtered(&items, SharedFilter::Files, "").len(), 1);
    assert_eq!(filtered(&items, SharedFilter::Links, "").len(), 3);
    assert_eq!(filtered(&items, SharedFilter::Recent, "AGENDA").len(), 1);
    assert_eq!(filtered(&items, SharedFilter::Recent, "example.com").len(), 1);
    assert!(filtered(&items, SharedFilter::Links, "agenda").is_empty());
    assert_eq!(SharedFilter::default().label(), "Recent");
}

#[test]
fn a_recording_stopped_and_not_ready_is_being_saved() {
    let mut messages = fixtures::messages(ME);
    // two ready
    assert!(!teams_ui::recap::recording_saving(&messages));
    let stopped = |id: &str| {
        let mut message = messages[0].clone();
        message.id = id.to_owned();
        message.content = message.content.replace(
            "status=\"Success\" code=\"200\"",
            "status=\"ChunkFinished\" code=\"202\"",
        );
        message
    };
    let more = [stopped("6"), stopped("7"), stopped("8")];
    messages.extend(more);
    assert!(teams_ui::recap::recording_saving(&messages));
}

#[test]
fn a_deleted_message_shares_nothing() {
    let mut messages = fixtures::messages(ME);
    for message in &mut messages {
        message.delete_time = Some(1);
    }
    assert!(shared_items(&messages).is_empty());
}

#[test]
fn the_transcripts_file_is_named_by_the_meeting() {
    assert_eq!(transcript_file_name("Weekly sync"), "Weekly sync.vtt");
    assert_eq!(transcript_file_name("a/b\\c"), "a-b-c.vtt");
    assert_eq!(transcript_file_name("  "), "Transcript.vtt");
}
