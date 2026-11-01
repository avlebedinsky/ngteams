//! App-call web endpoint decisions over armed synthetic CDP pages, with captured media evidence only.

use base64::Engine as _;
use serde_json::{Value, json};

use super::*;
use crate::browser::Display;
use crate::scenario::ab_chat_full::tests::{EventsOut, broadcast, page_with};
use crate::scenario::test_page::{Page, evaluated};

const A: &str = "8:orgid:synthetic-a";
const B: &str = "8:orgid:synthetic-b";

fn options(call: CallStep) -> RunOptions {
    RunOptions {
        display: Display::Headless,
        load_timeout: Duration::from_secs(1),
        call,
    }
}

// CDP responses cross asynchronous sockets even when the synthetic page answers immediately.
// Only observation sleeps may be zero; predicates need a real deadline under instrumentation.
fn quick() -> Pauses {
    Pauses {
        calling: CallPauses {
            page_steady: Duration::ZERO,
            calling_warm: Duration::ZERO,
            ..CallPauses::default()
        },
        ring: Duration::from_secs(1),
        app_hold: Duration::from_secs(1),
        answer: Duration::from_secs(1),
        page_hold: Duration::ZERO,
        connect: Duration::from_secs(1),
        microphone: Duration::from_millis(100),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ending {
    Remote,
    Local,
    Absent,
    Stuck,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fault {
    None,
    Start,
    ForeignRing,
    MultipleRings,
}

struct WebCall {
    ending: Ending,
    fault: Fault,
    sounds: bool,
    rings_sent: usize,
    camera: bool,
    hung_up: bool,
    pumps: Vec<tokio::task::JoinHandle<()>>,
}

fn rtc(relay: &EventsOut, payload: &Value) {
    broadcast(
        relay,
        "Runtime.bindingCalled",
        json!({"name":crate::capture::RTC_BINDING, "executionContextId":1, "payload":payload.to_string()}),
    );
}

fn stats(sounds: bool) -> Value {
    json!({"event":"stats", "data":[
        {"type":"outbound-rtp","kind":"audio","packetsSent":40},
        {"type":"inbound-rtp","kind":"audio","packetsReceived":30,"totalAudioEnergy":0.5},
        {"type":"inbound-rtp","kind":"video","framesDecoded":30},
        {"type":"media-source","kind":"audio","audioLevel":if sounds { 0.5 } else { 0.0 }},
    ]})
}

fn invitation(caller: &str) -> Value {
    let gp = base64::engine::general_purpose::STANDARD.encode(
        json!({
            "callNotification":{"from":{"id":caller},"to":{"id":B}},
            "conversationInvitation":{"conversationController":"https://synthetic.invalid/conv/call"}
        })
        .to_string(),
    );
    let body = json!({"evt":107,"gp":gp}).to_string();
    let frame = json!({"id":1,"method":"POST","url":"/v4/f/synthetic/","body":body});
    json!({"requestId":"synthetic-calling", "response":{"payloadData":format!("3:::{frame}")}})
}

impl WebCall {
    fn answer(&mut self, command: &Value, relay: &EventsOut) -> Option<Result<Value, String>> {
        if command["method"] == "Page.navigate" && self.fault == Fault::Start {
            return Some(Err("synthetic calling page initialization failed".to_owned()));
        }
        if command["method"] != "Runtime.evaluate" {
            return None;
        }
        let expression = command["params"]["expression"].as_str().unwrap();
        if expression.contains("notification-wrapper") && self.rings_sent == 0 {
            broadcast(
                relay,
                "Network.webSocketFrameReceived",
                invitation(if self.fault == Fault::ForeignRing {
                    "8:orgid:foreign"
                } else {
                    A
                }),
            );
            self.rings_sent += 1;
        }
        if expression.contains(".length === 1") {
            return Some(Ok(evaluated(json!(self.fault != Fault::MultipleRings))));
        }
        if expression.contains("background-suppression-switch") {
            rtc(relay, &stats(self.sounds));
            for _ in 0..10 {
                rtc(
                    relay,
                    &json!({"event":"audio","pc":1,"data":{"track":"synthetic-track","rms":0.25,"tone":0.99}}),
                );
            }
        }
        if expression.contains("getAttribute('aria-label')") && expression.contains("#microphone-button") {
            let relay = relay.clone();
            let sounds = self.sounds;
            self.pumps.push(tokio::spawn(async move {
                for _ in 0..60 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    rtc(&relay, &stats(sounds));
                }
            }));
            return Some(Ok(evaluated(json!("Mute microphone"))));
        }
        if expression.contains("#video-button") && expression.contains("e.click()") {
            self.camera = true;
        }
        if expression.contains("hangup") {
            if expression.ends_with(" === null") {
                let gone = (matches!(self.ending, Ending::Absent | Ending::Remote) && self.camera)
                    || (self.ending == Ending::Local && self.hung_up);
                return Some(Ok(evaluated(json!(gone))));
            }
            if expression.ends_with(" !== null") {
                return Some(Ok(evaluated(json!(
                    !(matches!(self.ending, Ending::Absent | Ending::Remote) && self.camera)
                ))));
            }
            if expression.contains("e.click()") {
                self.hung_up = true;
            }
        }
        None
    }
}

impl Drop for WebCall {
    fn drop(&mut self) {
        for pump in &self.pumps {
            pump.abort();
        }
    }
}

async fn page(ending: Ending, fault: Fault, sounds: bool) -> Page {
    let mut call = WebCall {
        ending,
        fault,
        sounds,
        rings_sent: 0,
        camera: false,
        hung_up: false,
        pumps: Vec::new(),
    };
    page_with(false, false, move |command, relay| call.answer(command, relay)).await
}

fn socket(page: &Page) {
    page.send("Network.webSocketCreated", json!({"requestId":"synthetic-calling","url":"wss://synthetic.trouter.teams.microsoft.com/socket.io/1/websocket/synthetic?client=SkypeSpaces"}));
    page.send(
        "Network.webSocketFrameReceived",
        json!({"requestId":"synthetic-calling","response":{"payloadData":"3:::{\"name\":\"trouter.connected\"}"}}),
    );
}

#[test]
fn app_call_live_waits_remain_pinned() {
    let defaults = Pauses::default();
    assert_eq!(defaults.calling, CallPauses::default());
    assert_eq!(defaults.ring, Duration::from_secs(90));
    assert_eq!(defaults.app_hold, Duration::from_secs(180));
    assert_eq!(defaults.answer, Duration::from_secs(90));
    assert_eq!(defaults.page_hold, Duration::from_secs(40));
    assert_eq!(defaults.connect, CONNECT_WAIT);
    assert_eq!(defaults.microphone, Duration::from_secs(5));
}

#[tokio::test]
async fn answer_binds_the_invitation_before_clicking_and_requires_captured_duplex_audio_video_and_microphone() {
    let mut page = page(Ending::Remote, Fault::None, true).await;
    let dir = tempfile::tempdir().unwrap();
    let mut session = page.session(dir.path(), true);
    socket(&page);
    let call = RunCall {
        caller: A.to_owned(),
        group_chat: None,
    };
    let mut report = ScenarioReport::new(ScenarioName::AbCallApp);
    let rang = drive(
        &mut session,
        (&call, B),
        options(CallStep::Answer),
        &mut report,
        &quick(),
    )
    .await;
    assert!(rang.is_some());
    assert!(report.failures.is_empty(), "{report:?}");
    for expected in [
        "clicked answer",
        "heard a's native client's tone",
        "microphone sounds after",
        "decoded 30 video frame",
    ] {
        assert!(report.notes.iter().any(|note| note.contains(expected)), "{report:?}");
    }
    let commands = page.commands();
    let checked = commands
        .iter()
        .position(|command| {
            command["params"]["expression"]
                .as_str()
                .is_some_and(|expression| expression.contains(".length === 1"))
        })
        .unwrap();
    let answered = commands
        .iter()
        .position(|command| {
            command["params"]["expression"]
                .as_str()
                .is_some_and(|expression| expression.contains("e.click()") && expression.contains("Accept with audio"))
        })
        .unwrap();
    assert!(checked < answered);
}

#[tokio::test]
async fn placing_waits_for_the_stack_and_media_then_hangs_up_the_page_call() {
    let mut page = page(Ending::Local, Fault::None, true).await;
    let dir = tempfile::tempdir().unwrap();
    let mut session = page.session(dir.path(), true);
    socket(&page);
    let call = RunCall {
        caller: A.to_owned(),
        group_chat: None,
    };
    let mut report = ScenarioReport::new(ScenarioName::AbCallApp);
    assert!(
        drive(
            &mut session,
            (&call, B),
            options(CallStep::Place),
            &mut report,
            &quick()
        )
        .await
        .is_none()
    );
    assert!(report.failures.is_empty(), "{report:?}");
    assert!(report.notes.iter().any(|note| note.starts_with("b: hung up at")));
    let expressions: Vec<String> = page
        .commands()
        .iter()
        .filter_map(|command| command["params"]["expression"].as_str().map(str::to_owned))
        .collect();
    let watched = expressions
        .iter()
        .position(|expression| expression.contains("window.__labRingControls"))
        .unwrap();
    let placed = expressions
        .iter()
        .position(|expression| expression.contains("chat-call-audio-button") && expression.contains("e.click()"))
        .unwrap();
    assert!(watched < placed);
}

#[tokio::test]
async fn unrelated_or_multiple_incoming_calls_are_never_answered_and_failed_initialization_is_cleaned_up() {
    for fault in [Fault::ForeignRing, Fault::MultipleRings, Fault::Start] {
        let mut page = page(Ending::Absent, fault, false).await;
        let dir = tempfile::tempdir().unwrap();
        let mut session = page.session(dir.path(), true);
        socket(&page);
        let call = RunCall {
            caller: A.to_owned(),
            group_chat: None,
        };
        let mut report = ScenarioReport::new(ScenarioName::AbCallApp);
        assert!(
            drive(
                &mut session,
                (&call, B),
                options(CallStep::Answer),
                &mut report,
                &quick()
            )
            .await
            .is_none()
        );
        assert_eq!(report.failures.len(), 1, "{report:?}");
        assert!(report.failures[0].contains(if fault == Fault::Start {
            "initialization failed"
        } else {
            "doesn't click answer"
        }));
        assert!(page.commands().iter().all(|command| {
            !command["params"]["expression"]
                .as_str()
                .is_some_and(|expression| expression.contains("e.click()") && expression.contains("Accept with audio"))
        }));
        assert!(
            page.commands()
                .iter()
                .any(|command| command["method"] == "Page.captureScreenshot")
        );
    }
}

#[tokio::test]
async fn silent_microphones_and_missing_or_stuck_call_screens_are_reported_without_abandoning_cleanup() {
    for (step, ending, expected) in [
        (CallStep::Answer, Ending::Stuck, "a didn't hang up"),
        (CallStep::Place, Ending::Absent, "gone before b hung up"),
        (CallStep::Place, Ending::Stuck, "stayed after b hung up"),
    ] {
        let mut page = page(ending, Fault::None, false).await;
        let dir = tempfile::tempdir().unwrap();
        let mut session = page.session(dir.path(), true);
        socket(&page);
        let call = RunCall {
            caller: A.to_owned(),
            group_chat: None,
        };
        let mut report = ScenarioReport::new(ScenarioName::AbCallApp);
        let mut pauses = quick();
        pauses.microphone = Duration::from_millis(100);
        drive(&mut session, (&call, B), options(step), &mut report, &pauses).await;
        assert!(
            report.failures.iter().any(|failure| failure.contains(expected)),
            "{report:?}"
        );
        assert!(
            report
                .notes
                .iter()
                .any(|note| note.contains("microphone stayed silent through 3")),
            "{report:?}"
        );
        if ending == Ending::Stuck {
            assert!(
                report
                    .notes
                    .iter()
                    .any(|note| note.contains("hung up at the run's end"))
            );
        }
    }
}

#[tokio::test]
async fn microphone_and_call_setup_without_capture_refuse_instead_of_inventing_evidence() {
    let mut page = page(Ending::Stuck, Fault::None, false).await;
    let dir = tempfile::tempdir().unwrap();
    let mut session = page.session(dir.path(), false);
    let mut report = ScenarioReport::new(ScenarioName::AbCallApp);
    let error = sound_microphone(&mut session, (0, CallStep::Answer), &mut report, Duration::ZERO)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("records no capture"));
    let call = RunCall {
        caller: A.to_owned(),
        group_chat: None,
    };
    drive(
        &mut session,
        (&call, B),
        options(CallStep::Answer),
        &mut report,
        &quick(),
    )
    .await;
    assert!(report.failures[0].contains("records no capture to see its calling socket"));
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("hung up at the run's end"))
    );
}

#[tokio::test]
async fn stale_loud_statistics_and_fresh_quiet_samples_require_all_microphone_retries() {
    let mut page = page(Ending::Local, Fault::None, false).await;
    let dir = tempfile::tempdir().unwrap();
    let mut session = page.session(dir.path(), true);
    std::fs::write(
        session.capture_dir().unwrap().join(EVENTS_FILE),
        format!("{}\n", json!({"type":"rtc","payload":stats(true)})),
    )
    .unwrap();
    let mut report = ScenarioReport::new(ScenarioName::AbCallApp);
    sound_microphone(
        &mut session,
        (0, CallStep::Place),
        &mut report,
        Duration::from_millis(100),
    )
    .await
    .unwrap();
    assert!(report.failures.is_empty());
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("microphone stayed silent through 3"))
    );
    assert!(
        report
            .notes
            .iter()
            .all(|note| !note.contains("microphone sounds after"))
    );
    let labels = page
        .commands()
        .iter()
        .filter(|command| {
            command["params"]["expression"].as_str().is_some_and(|expression| {
                expression.contains("#microphone-button") && expression.contains("getAttribute('aria-label')")
            })
        })
        .count();
    assert_eq!(labels, MIC_TRIES + 1);
}

#[test]
fn judgments_require_a_permitted_start_or_a_bound_invitation_and_attach() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(EVENTS_FILE);
    let call = RunCall {
        caller: A.to_owned(),
        group_chat: None,
    };
    let mut report = ScenarioReport::new(ScenarioName::AbCallApp);
    judge(CallStep::Place, (&call, B, None), dir.path(), &mut report);
    assert!(report.failures[0].contains("allowed no call start"));
    let start = json!({"type":"verdict","channel":"http","method":"POST","url":super::super::echo_call::CALL_START_URL,"verdict":"allow"});
    std::fs::write(&file, format!("{start}\n")).unwrap();
    let mut report = ScenarioReport::new(ScenarioName::AbCallApp);
    judge(CallStep::Place, (&call, B, None), dir.path(), &mut report);
    assert!(report.failures.is_empty());
    let attach = json!({"type":"verdict","channel":"http","method":"POST","url":"https://api.flightproxy.teams.microsoft.com/api/v2/ep/synthetic/cc/v1/forked/synthetic/attach","verdict":"allow"});
    let ring = json!({"type":"cdp","ts_ms":10,"method":"Network.webSocketFrameReceived","params":invitation(A)});
    std::fs::write(&file, format!("{ring}\n{attach}\n")).unwrap();
    let mut report = ScenarioReport::new(ScenarioName::AbCallApp);
    judge(CallStep::Answer, (&call, B, Some(1)), dir.path(), &mut report);
    assert!(report.failures.is_empty(), "{report:?}");
    let mut report = ScenarioReport::new(ScenarioName::AbCallApp);
    judge(CallStep::Answer, (&call, B, Some(100)), dir.path(), &mut report);
    assert!(report.failures[0].contains("no invitation"));
}
