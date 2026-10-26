//! `abc-call-mixed`'s judgement of what it can judge offline: the forced denies each page expects, and the native
//! client's output it reads (who its call shows on which line, the frames each line brought).

use proto_lab::scenario::ScenarioName;
use proto_lab::scenario::ab_chat::EARLIER_RUN_PROBE;
use proto_lab::scenario::abc_call_mixed::{
    FORCED_B, FORCED_C, Shown, frames_on, latest_participants, latest_video_lines, native_lines, shows,
    video_lines_after,
};

#[test]
fn each_page_expects_only_an_earlier_run_s_probe() {
    assert_eq!(FORCED_B, [EARLIER_RUN_PROBE]);
    assert_eq!(FORCED_C, [EARLIER_RUN_PROBE]);
    assert_eq!(ScenarioName::AbcCallMixed.as_str(), "abc-call-mixed");
}

const OUTPUT: &str = "\
1792 call call=1 status=\"connected\"
1793 call_participants call=1 participants=[] speaking=true
1794 call_participants call=1 participants=[{\"line\":1,\"speaking\":false,\"video_on\":true,\"who\":\"b\"},\
{\"line\":2,\"speaking\":true,\"video_on\":true,\"who\":\"c\"}] speaking=false
video received=600 changed=600 indexed=600 gaps=0 last_index=300 sent=380 lines=2
video line=1 received=320 indexed=320 gaps=0 last_index=300
video line=2 received=280 indexed=280 gaps=0 last_index=290
";

#[test]
fn the_native_client_s_last_participants_line_says_who_is_on_which_line() {
    let shown = latest_participants(OUTPUT).unwrap();
    assert_eq!(
        shown,
        [
            Shown {
                who: "b".into(),
                line: Some(1),
                video_on: true
            },
            Shown {
                who: "c".into(),
                line: Some(2),
                video_on: true
            },
        ]
    );
    assert!(shows(Some(&shown), &["b", "c"]));
    assert!(!shows(Some(&shown), &["b"]), "c is still shown");
    assert!(!shows(None, &["b"]));
    assert_eq!(latest_participants("1792 call call=1 status=\"connected\"\n"), None);
    // a participant with no line, or two on one line, or a camera off isn't shown as the run wants
    let off = [Shown {
        who: "b".into(),
        line: Some(1),
        video_on: false,
    }];
    assert!(!shows(Some(&off), &["b"]));
    let lineless = [Shown {
        who: "b".into(),
        line: None,
        video_on: true,
    }];
    assert!(!shows(Some(&lineless), &["b"]));
}

#[test]
fn the_frames_each_line_brought_are_read_from_its_summary() {
    assert_eq!(native_lines(OUTPUT), [(1, 320), (2, 280)]);
    assert!(native_lines("video received=0 changed=0 lines=0\n").is_empty());
}

#[test]
fn the_frames_each_line_brought_while_the_call_ran_are_read_from_its_latest_counts() {
    let output = "\
1792 call call=1 status=\"connected\"
1800 video_lines received=1:40,2:35
1802 call_participants call=1 participants=[] speaking=false
1810 video_lines received=1:90,2:35,3:12
video received=600 changed=600 indexed=600 gaps=0 last_index=300 sent=380 lines=2
";
    let latest = latest_video_lines(output).unwrap();
    assert_eq!(latest, [(1, 90), (2, 35), (3, 12)]);
    assert_eq!(frames_on(&latest, 3), 12);
    assert_eq!(frames_on(&latest, 4), 0, "a line that brought nothing");
    assert_eq!(latest_video_lines(OUTPUT), None, "no counts while the call ran");
    assert_eq!(latest_video_lines("1810 video_lines received=1:x\n"), None);
}

#[test]
fn the_counts_before_a_late_join_are_only_those_printed_after_c_was_seen_gone() {
    let output = "\
1800 video_lines received=1:40,2:100
1802 call_participants call=1 participants=[] speaking=false
";
    // c was seen gone at 1805: line 2's 100 may still grow by frames c sent before it left, so it is no baseline
    assert_eq!(video_lines_after(output, 1805), None);
    assert_eq!(
        video_lines_after(output, 1800),
        None,
        "printed at that very moment isn't after it"
    );
    let later = format!("{output}1806 video_lines received=1:60,2:140\n");
    assert_eq!(video_lines_after(&later, 1805), Some(vec![(1, 60), (2, 140)]));
    assert_eq!(video_lines_after(OUTPUT, 0), None, "no counts while the call ran");
    assert_eq!(
        video_lines_after("x video_lines received=1:5\n", 0),
        None,
        "no time it was printed at"
    );
}
