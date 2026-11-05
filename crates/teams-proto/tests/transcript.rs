//! A meeting transcript's `WebVTT` read into cues and the recap's turns. Every name and word here is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::time::Duration;

use teams_proto::transcript::{Cue, Paragraph, Turn, find, paragraph_at, read_vtt, turn_at, turns};

const FILE: &str = "WEBVTT\r\n\r\n\
    0f1e2d3c-aaaa-bbbb-cccc-000000000001/12-0\r\n\
    00:00:03.120 --> 00:00:05.000\r\n\
    <v Ada Lab>The first thing.</v>\r\n\r\n\
    0f1e2d3c-aaaa-bbbb-cccc-000000000001/13-0\r\n\
    00:00:05.400 --> 00:00:07.900\r\n\
    <v Bo Lab>Yes, and &amp; the second</v>\r\n\r\n\
    NOTE a note is no cue\r\n\r\n\
    00:00:08.000 --> 00:00:09.500 align:start\r\n\
    <v Bo Lab>one more\r\nline.</v>\r\n\r\n\
    01:02:03.004 --> 01:02:04.000\r\n\
    <v Ada Lab>Late.</v>\r\n";

fn at(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

#[test]
fn reads_each_cue_with_its_speaker_and_times() {
    let cues = read_vtt(FILE).expect("a transcript");
    assert_eq!(cues.len(), 4);
    assert_eq!(
        cues[0],
        Cue {
            id: Some("0f1e2d3c-aaaa-bbbb-cccc-000000000001/12-0".to_owned()),
            start: at(3_120),
            end: at(5_000),
            speaker: Some("Ada Lab".to_owned()),
            text: "The first thing.".to_owned(),
        }
    );
    assert_eq!(cues[1].text, "Yes, and & the second");
    // the settings after the end time are left out, a cue's lines are one paragraph
    assert_eq!(cues[2].end, at(9_500));
    assert_eq!(cues[2].text, "one more line.");
    assert_eq!(cues[3].start, at(((60 + 2) * 60 + 3) * 1_000 + 4));
}

#[test]
fn groups_a_speakers_cues_in_a_row_into_one_turn() {
    let read = turns(&read_vtt(FILE).expect("a transcript"));
    let speakers: Vec<_> = read.iter().map(|turn| turn.speaker.as_deref()).collect();
    assert_eq!(speakers, [Some("Ada Lab"), Some("Bo Lab"), Some("Ada Lab")]);
    assert_eq!(read[1].start, at(5_400));
    assert_eq!(read[1].end(), at(9_500));
    assert_eq!(
        read[1].paragraphs[1],
        Paragraph {
            start: at(8_000),
            end: at(9_500),
            text: "one more line.".to_owned(),
        }
    );
}

#[test]
fn tells_the_turn_spoken_at_a_time() {
    let read = turns(&read_vtt(FILE).expect("a transcript"));
    assert_eq!(turn_at(&read, at(0)), None);
    assert_eq!(turn_at(&read, at(3_120)), Some(0));
    assert_eq!(turn_at(&read, at(6_000)), Some(1));
    assert_eq!(turn_at(&read, at(60 * 60 * 1_000)), Some(1));
    assert_eq!(turn_at(&read, at(4 * 60 * 60 * 1_000)), Some(2));
    assert_eq!(turn_at(&[], at(1)), None);
}

#[test]
fn finds_the_paragraphs_that_hold_a_query() {
    let read = turns(&read_vtt(FILE).expect("a transcript"));
    assert_eq!(find(&read, "ONE MORE"), [(1, 1)]);
    assert_eq!(find(&read, "the"), [(0, 0), (1, 0)]);
    assert!(find(&read, "  ").is_empty());
    assert!(find(&read, "nobody said this").is_empty());
}

#[test]
fn a_file_that_isnt_a_transcript_reads_as_none() {
    assert_eq!(read_vtt(""), None);
    assert_eq!(read_vtt("00:00:01.000 --> 00:00:02.000\nno header"), None);
    // a header and no cue, or only cues whose times don't read
    assert_eq!(read_vtt("WEBVTT\n\n"), None);
    assert_eq!(
        read_vtt("WEBVTT\n\n00:00:1.0 --> 00:00:02.000\nbad\n\n00:61:00.000 --> 00:62:00.000\nbad"),
        None
    );
    // an hour count past what a time can hold
    assert_eq!(
        read_vtt("WEBVTT\n\n18446744073709551615:00:00.000 --> 18446744073709551615:00:01.000\nlong"),
        None
    );
    // an empty cue says nothing
    assert_eq!(
        read_vtt("WEBVTT\n\n00:00:01.000 --> 00:00:02.000\n<v Ada Lab></v>"),
        None
    );
}

#[test]
fn reads_a_cue_without_a_speaker_and_with_a_byte_order_mark() {
    let cues = read_vtt(
        "\u{feff}WEBVTT - made up\n\n00:01.500 --> 00:02.000\nNo voice &lt;here&gt; &#x41;&#66; &bogus; & done",
    )
    .expect("a transcript");
    assert_eq!(
        cues,
        [Cue {
            id: None,
            start: at(1_500),
            end: at(2_000),
            speaker: None,
            text: "No voice <here> AB &bogus; & done".to_owned(),
        }]
    );
    let read = turns(&cues);
    assert_eq!(
        read,
        [Turn {
            speaker: None,
            start: at(1_500),
            paragraphs: vec![Paragraph {
                start: at(1_500),
                end: at(2_000),
                text: "No voice <here> AB &bogus; & done".to_owned(),
            }],
        }]
    );
}

#[test]
fn an_utterances_cues_are_one_paragraph() {
    let file = "WEBVTT\n\nu/4-0\n00:00:03.791 --> 00:00:04.191\n<v Ada Lab>Today.</v>\n\n\
        u/5-0\n00:00:07.271 --> 00:00:10.585\n<v Ada Lab>Hello everyone,\nthis is the lab.</v>\n\n\
        u/5-1\n00:00:10.585 --> 00:00:14.188\n<v Ada Lab>Today we look.</v>\n\n\
        u/6-0\n00:00:15.000 --> 00:00:16.000\n<v Bo Lab>Yes.</v>\n\n\
        u/7-0\n00:00:17.000 --> 00:00:18.000\n<v Ada Lab>Bye.</v>";
    let read = turns(&read_vtt(file).expect("a transcript"));
    assert_eq!(read.len(), 3);
    let texts: Vec<_> = read[0]
        .paragraphs
        .iter()
        .map(|paragraph| paragraph.text.as_str())
        .collect();
    assert_eq!(texts, ["Today.", "Hello everyone, this is the lab. Today we look."]);
    assert_eq!(read[0].paragraphs[1].start, at(7_271));
    assert_eq!(read[0].paragraphs[1].end, at(14_188));
    assert_eq!(paragraph_at(&read, at(12_000)), Some((0, 1)));
    assert_eq!(paragraph_at(&read, at(4_000)), Some((0, 0)));
    assert_eq!(paragraph_at(&read, at(15_500)), Some((1, 0)));
    assert_eq!(paragraph_at(&read, at(1_000)), None);
}
