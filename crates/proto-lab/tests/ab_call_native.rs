//! `ab-call-native`'s judgement of what it can judge offline: the forced denies b's page expects, the native clients'
//! output it reads (a ring that ended as answered elsewhere among it), the scenario's name.

use proto_lab::scenario::ScenarioName;
use proto_lab::scenario::ab_call::{ANSWER_HELD, CALLER_JOIN_HELD, RING_SENDS};
use proto_lab::scenario::ab_call_native::{
    ANSWERED_ELSEWHERE, FORCED, HEADLESS_BIN, TONE_HZ, is_summary, native_heard, ring_ended_elsewhere,
};
use proto_lab::scenario::ab_chat::EARLIER_RUN_PROBE;

#[test]
fn b_s_page_expects_its_probe_ring_answer_and_join_holds_and_nothing_else() {
    assert_eq!(FORCED, [EARLIER_RUN_PROBE, RING_SENDS, ANSWER_HELD, CALLER_JOIN_HELD]);
    assert_eq!(ScenarioName::AbCallNative.as_str(), "ab-call-native");
    assert_eq!(HEADLESS_BIN, "TEAMS_HEADLESS_BIN");
    // the tone the page's audio tap reads
    assert_eq!(TONE_HZ, proto_lab::capture::TAP_TONE_HZ);
}

#[test]
fn a_native_client_heard_the_page_only_when_its_tone_line_says_so_at_the_page_s_tone() {
    let heard = format!("recording /x.wav\ntone hz={TONE_HZ} heard=true seconds=12.3 first_ms=900 peak=0.120\n");
    assert!(native_heard(&heard));
    let silent = format!("tone hz={TONE_HZ} heard=false seconds=0.0 first_ms=- peak=0.001\n");
    assert!(!native_heard(&silent));
    assert!(!native_heard(
        "tone hz=1000 heard=true seconds=3.0 first_ms=0 peak=0.4\n"
    ));
    // the word in another line doesn't count
    assert!(!native_heard(&format!(
        "1 incoming_call call=1 note=\"tone hz={TONE_HZ} heard=true\""
    )));
    assert!(!native_heard(""));
}

#[test]
fn the_run_notes_a_native_client_s_summary_lines_only() {
    for line in [
        "1792 call_summary call=1 setup_ms=2300 held=true end=\"hung_up\"",
        "tone hz=440 heard=true seconds=12.0 first_ms=900 peak=0.12",
        "echo heard=false",
        "devices microphone_opened_ms=1 speaker_opened_ms=2",
        "1792 call_answer call=1",
        "1792 ring_ended call=1 code=0 phrase=\"x\"",
    ] {
        assert!(is_summary(line), "{line}");
    }
    for line in [
        "1792 call call=1 status=\"connected\"",
        "1792 calling_registration registered=true",
        "caught_up conversations=3",
    ] {
        assert!(!is_summary(line), "{line}");
    }
}

#[test]
fn b_s_native_ring_ended_as_answered_elsewhere_only_on_that_end_s_phrase() {
    let elsewhere = "1792 ring_ended call=1 code=487 phrase=\"Call cancelled as it was accepted by another fork.\"";
    assert!(ring_ended_elsewhere(&format!(
        "1700 incoming_call call=1 caller=a\n{elsewhere}\n"
    )));
    for other in [
        // a's cancel, the ring's timeout, no phrase
        "1792 ring_ended call=1 code=487 phrase=\"Call cancelled by the caller.\"",
        "1792 ring_ended call=1 code=487 phrase=\"Call cancelled on timeout.\"",
        "1792 ring_ended call=1 code=0 phrase=null",
        // the phrase in another line
        "1792 call call=1 status=\"ended\" note=\"accepted by another fork\"",
        "",
    ] {
        assert!(!ring_ended_elsewhere(other), "{other}");
    }
    assert!(elsewhere.contains(ANSWERED_ELSEWHERE));
}
