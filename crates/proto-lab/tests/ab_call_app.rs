//! `ab-call-app`'s offline parts: the forced denies b's page expects in each step, the steps it takes, and when it
//! says its own microphone sounds. Its progress lines are read (and their parser tested) by the app's live test.

use proto_lab::scenario::ScenarioName;
use proto_lab::scenario::ab_call::{ANSWER_HELD, CALLER_JOIN_HELD, CallStep, RING_SENDS};
use proto_lab::scenario::ab_call_app::{FORCED_ANSWER, FORCED_PLACE, forced, microphone_sounds};
use proto_lab::scenario::ab_chat::EARLIER_RUN_PROBE;

#[test]
fn b_s_page_expects_its_probe_and_the_step_s_holds_and_nothing_else() {
    assert_eq!(FORCED_ANSWER, [EARLIER_RUN_PROBE, RING_SENDS, ANSWER_HELD]);
    assert_eq!(FORCED_PLACE, [EARLIER_RUN_PROBE, CALLER_JOIN_HELD]);
    assert_eq!(forced(CallStep::Answer), Ok(FORCED_ANSWER));
    assert_eq!(forced(CallStep::Place), Ok(FORCED_PLACE));
    assert_eq!(ScenarioName::AbCallApp.as_str(), "ab-call-app");
    assert_eq!(CallStep::Place.as_str(), "place");
}

#[test]
fn every_other_step_is_refused() {
    for step in [
        CallStep::StartHeld,
        CallStep::Decline,
        CallStep::Cancel,
        CallStep::Timeout,
        CallStep::Video,
        CallStep::Dropped,
        CallStep::Join,
        CallStep::KeepAlive,
    ] {
        let refused = forced(step).unwrap_err();
        assert!(refused.contains(step.as_str()), "{refused}");
    }
    // the page places a call: no callee of `ab-call` acts on it
    assert_eq!(CallStep::Place.callee_action(), None);
}

#[test]
fn the_page_s_microphone_sounds_only_on_a_loud_enough_audio_source_report() {
    let stats = |kind: &str, level: f64| {
        format!(
            r#"{{"type":"rtc","payload":{{"event":"stats","data":[{{"type":"media-source","kind":"{kind}","audioLevel":{level}}}]}}}}"#
        )
    };
    assert!(microphone_sounds(&format!(
        "{}\n{}\n",
        stats("audio", 0.0),
        stats("audio", 0.3)
    )));
    assert!(!microphone_sounds(&format!("{}\n", stats("audio", 0.01))));
    // a video source, another event, a line that isn't JSON
    assert!(!microphone_sounds(&stats("video", 0.9)));
    assert!(!microphone_sounds(
        r#"{"type":"rtc","payload":{"event":"audio","data":{"audioLevel":0.9}}}"#
    ));
    assert!(!microphone_sounds("{\"half"));
}
