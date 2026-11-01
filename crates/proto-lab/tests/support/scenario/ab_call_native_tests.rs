//! Native process management with generated synthetic shell stand-ins, never teams-headless or a tenant.

use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::process::ExitStatusExt as _;

use serde_json::json;

use super::*;
use crate::browser::Display;
use crate::scenario::ab_call::CallStep;
use crate::scenario::test_page::{Page, evaluated};

fn script(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("synthetic-native");
    fs::write(
        &path,
        format!("#!/bin/sh\n# Synthetic offline native-process stand-in\n{body}\n"),
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

fn run(binary: PathBuf, dir: &Path) -> Run {
    Run {
        binary,
        dir: dir.to_path_buf(),
        call: RunCall {
            caller: "8:orgid:synthetic-a".to_owned(),
            group_chat: None,
        },
        mri_a: "8:orgid:synthetic-a".to_owned(),
        mri_b: "8:orgid:synthetic-b".to_owned(),
        options: RunOptions {
            display: Display::Headless,
            load_timeout: Duration::from_millis(10),
            call: CallStep::Answer,
        },
        natives: Vec::new(),
        first_ring: None,
    }
}

#[test]
fn native_arguments_preserve_tone_target_hold_and_optional_video() {
    assert_eq!(tone_args(), ["--tone", "440", "--bursts", "--hear", "440"]);
    assert_eq!(video_args(), ["--video", "--camera-after", "5"]);
    let audio = call_args(Duration::from_secs(7), false);
    assert_eq!(
        audio,
        [
            "call",
            "b",
            "--tone",
            "440",
            "--bursts",
            "--hear",
            "440",
            "--seconds",
            "7",
            "--wait",
            "90"
        ]
    );
    let video = call_args(Duration::from_secs(8), true);
    assert!(video.windows(3).any(|args| args == ["--video", "--camera-after", "5"]));
    assert!(video.windows(2).any(|args| args == ["--seconds", "8"]));
    assert_eq!(
        watch_args("--calls", false),
        ["watch", "--calls", "--tone", "440", "--bursts", "--hear", "440"]
    );
    assert!(
        watch_args("--answer-lab-calls", true)
            .windows(3)
            .any(|args| args == ["--video", "--camera-after", "5"])
    );
}

#[tokio::test]
async fn a_synthetic_native_receives_its_alias_and_arguments_and_its_output_is_summarized() {
    let dir = tempfile::tempdir().unwrap();
    let binary = script(
        dir.path(),
        "printf 'args %s %s\\n' \"$1\" \"$2\"\nprintf 'call_summary synthetic\\n'\nprintf 'synthetic stderr\\n' >&2",
    );
    let mut page = Page::new(|_| Err("no CDP action is needed to reap this child".to_owned())).await;
    let mut session = page.session(dir.path(), false);
    let mut native = spawn_native((&binary, dir.path()), Alias::B, "synthetic", &["watch".to_owned()]).unwrap();
    let status = native.wait(&mut session, Duration::from_secs(2)).await;
    assert!(status.is_some_and(|status| status.success()));
    assert!(native.output().contains("args b watch"));
    assert!(native.output().contains("synthetic stderr"));
    let mut report = ScenarioReport::new(ScenarioName::AbCallNative);
    let finished = native.finished(status, &mut report);
    assert_eq!(finished.what, "synthetic");
    assert!(finished.output.contains("call_summary synthetic"));
    assert!(report.notes[0].contains("call_summary synthetic"));
    assert!(report.notes[1].contains("exited Some(Some(0))"));
    assert!(page.commands().is_empty());
}

#[tokio::test]
async fn a_native_that_exceeds_its_deadline_is_killed_and_a_running_one_can_be_interrupted() {
    let dir = tempfile::tempdir().unwrap();
    let binary = script(dir.path(), "exec /bin/sleep 60");
    let mut page = Page::new(|_| Err("no CDP action is needed to stop this child".to_owned())).await;
    let mut session = page.session(dir.path(), false);
    let mut native = spawn_native((&binary, dir.path()), Alias::A, "deadline", &[]).unwrap();
    assert!(native.wait(&mut session, Duration::ZERO).await.is_none());
    assert!(native.child.id().is_none());
    let native = spawn_native((&binary, dir.path()), Alias::A, "interrupt", &[]).unwrap();
    let mut report = ScenarioReport::new(ScenarioName::AbCallNative);
    let finished = stop_native(native, &mut session, &mut report).await;
    assert!(finished.status.is_some());
    assert!(report.notes.last().unwrap().contains("interrupt: exited"));
}

#[tokio::test]
async fn run_tracking_reports_missing_children_waits_for_output_and_removes_reaped_children() {
    let dir = tempfile::tempdir().unwrap();
    let binary = script(dir.path(), "printf 'calling_registration registered=true\\n'");
    let mut run = run(binary, dir.path());
    assert!(run.native("missing").is_err());
    assert!(run.take("missing").is_err());
    run.spawn(Alias::A, "first", &[]).unwrap();
    let mut page = Page::new(|_| Err("no CDP command is needed for synthetic output".to_owned())).await;
    let mut session = page.session(dir.path(), false);
    run.wait_output("first", &mut session, "registered=true", Duration::from_secs(2))
        .await
        .unwrap();
    let error = run
        .wait_output("first", &mut session, "missing-marker", Duration::ZERO)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no missing-marker"));
    let mut report = ScenarioReport::new(ScenarioName::AbCallNative);
    let finished = run
        .finish("first", Duration::ZERO, &mut session, &mut report)
        .await
        .unwrap();
    assert!(finished.status.is_some_and(|status| status.success()));
    assert!(run.natives.is_empty());
    run.spawn(Alias::B, "second", &[]).unwrap();
    let _ = run.stop("second", &mut session, &mut report).await.unwrap();
    assert!(run.natives.is_empty());
    run.spawn(Alias::B, "last", &[]).unwrap();
    run.stop_natives(&mut session, &mut report).await;
    assert!(run.natives.is_empty());
    assert!(report.notes.last().unwrap().contains("last: stopped at the run's end"));
}

#[tokio::test]
async fn spawn_errors_name_the_output_or_binary_without_starting_a_native() {
    let dir = tempfile::tempdir().unwrap();
    let error = spawn_native((&dir.path().join("absent"), dir.path()), Alias::A, "absent", &[])
        .err()
        .unwrap();
    assert!(error.to_string().contains("can't start teams-headless"));
    let error = spawn_native(
        (Path::new("/bin/true"), &dir.path().join("absent")),
        Alias::A,
        "output",
        &[],
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("can't create"));
}

#[test]
fn native_success_requires_exit_zero_heard_tone_camera_and_enough_video() {
    for (status, output, expected) in [
        (
            Some(ExitStatus::from_raw(0)),
            "tone hz=440 heard=true\ncamera_on call=1\nvideo received=1000",
            0,
        ),
        (Some(ExitStatus::from_raw(256)), "", 4),
        (None, "camera_on call=1\nvideo received=0", 3),
    ] {
        let native = Finished {
            what: "synthetic",
            status,
            output: output.to_owned(),
        };
        let mut report = ScenarioReport::new(ScenarioName::AbCallNative);
        judge_native(&native, &mut report);
        judge_native_video(&native, &mut report);
        assert_eq!(report.failures.len(), expected, "{:?}", report.failures);
    }
}

#[test]
fn reading_a_phase_clips_its_start_and_handles_missing_or_non_utf8_output() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("synthetic-output");
    assert_eq!(read_from(&file, 0), "");
    fs::write(&file, b"old\nnew\xff").unwrap();
    assert_eq!(read_from(&file, 4), "new\u{fffd}");
    assert_eq!(read_from(&file, u64::MAX), "");
    assert!(now_ms() > 0);
}

#[tokio::test]
async fn video_success_reads_only_this_phases_capture_and_missing_capture_is_refused() {
    for record in [true, false] {
        let mut page = Page::new(|command| {
            assert_eq!(command["method"], "Runtime.evaluate");
            assert!(
                command["params"]["expression"]
                    .as_str()
                    .unwrap()
                    .contains("#video-button")
            );
            Ok(evaluated(json!(true)))
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let mut session = page.session(dir.path(), record);
        let mut report = ScenarioReport::new(ScenarioName::AbCallNative);
        if let Some(capture) = session.capture_dir() {
            let stats = json!({"type": "rtc", "payload": {"event": "stats", "data": [{"type": "inbound-rtp", "kind": "video", "framesDecoded": PAGE_FRAMES}]}});
            fs::write(capture.join(EVENTS_FILE), stats.to_string()).unwrap();
            assert!(capture_len(&session) > 0);
        } else {
            assert_eq!(capture_len(&session), 0);
        }
        let outcome = see_on_page(&mut session, 0, &mut report).await;
        assert_eq!(outcome.is_ok(), record);
        assert!(report.failures.is_empty());
        assert_eq!(report.notes.len(), if record { 2 } else { 1 });
        assert_eq!(page.commands().len(), 1);
    }
}

fn invitation() -> String {
    use base64::Engine as _;
    let payload = json!({"callNotification": {"from": {"id": "8:orgid:synthetic-a"}, "to": {"id": "8:orgid:synthetic-b"}},
        "conversationInvitation": {"conversationController": "https://synthetic.invalid/conv/call"}});
    let body =
        json!({"evt": 107, "gp": base64::engine::general_purpose::STANDARD.encode(payload.to_string())}).to_string();
    let message = json!({"id": 1, "method": "POST", "url": "/v4/f/synthetic/", "body": body});
    json!({"type": "cdp", "ts_ms": now_ms() + 60_000, "method": "Network.webSocketFrameReceived",
        "params": {"response": {"payloadData": format!("3:::{message}")}}})
    .to_string()
}

fn media() -> String {
    let stats = json!({"type": "rtc", "payload": {"event": "stats", "data": [
        {"type": "outbound-rtp", "kind": "audio", "packetsSent": 40},
        {"type": "inbound-rtp", "kind": "audio", "packetsReceived": 30, "totalAudioEnergy": 0.5},
        {"type": "inbound-rtp", "kind": "video", "framesDecoded": PAGE_FRAMES},
    ]}})
    .to_string();
    let reading = json!({"type": "rtc", "session": "synthetic", "context": 1, "payload": {
        "event": "audio", "pc": 1, "data": {"track": "synthetic-track", "rms": 0.25, "tone": 0.99},
    }})
    .to_string();
    format!("{stats}\n{}", format!("{reading}\n").repeat(10))
}

type CaptureTarget = std::sync::Arc<std::sync::Mutex<Option<PathBuf>>>;

async fn media_page() -> (Page, CaptureTarget) {
    use std::io::Write as _;
    let output: CaptureTarget = std::sync::Arc::default();
    let target = std::sync::Arc::clone(&output);
    let page = Page::new(move |command| {
        assert_eq!(command["method"], "Runtime.evaluate");
        let expression = command["params"]["expression"].as_str().unwrap();
        if expression.contains("background-suppression-switch") {
            let target = target.lock().unwrap();
            let mut file = fs::OpenOptions::new()
                .append(true)
                .open(target.as_ref().unwrap())
                .unwrap();
            writeln!(file, "{}", media()).unwrap();
        }
        Ok(evaluated(json!(true)))
    })
    .await;
    (page, output)
}

#[tokio::test]
async fn the_native_to_page_phase_binds_the_answer_and_requires_both_sides_media() {
    let dir = tempfile::tempdir().unwrap();
    let binary = script(
        dir.path(),
        "printf 'call_summary synthetic\\ntone hz=440 heard=true\\ncamera_on call=1\\nvideo received=100\\n'",
    );
    let mut run = run(binary, dir.path());
    let (mut page, capture) = media_page().await;
    let mut session = page.session(dir.path(), true);
    let file = session.capture_dir().unwrap().join(EVENTS_FILE);
    fs::write(&file, format!("{}\n", invitation())).unwrap();
    capture.lock().unwrap().replace(file);
    let mut report = ScenarioReport::new(ScenarioName::AbCallNative);
    run.native_calls_page(&mut session, &mut report).await.unwrap();
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert!(run.first_ring.is_some());
    assert!(run.natives.is_empty());
    assert!(report.notes.iter().any(|note| note.contains("b: clicked answer")));
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("b: heard a's native client's tone"))
    );
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("decoded 30 video frame(s)"))
    );
    assert!(report.notes.iter().any(|note| note.contains("call screen went")));
    let commands = page.commands();
    let check = commands
        .iter()
        .position(|command| {
            command["params"]["expression"]
                .as_str()
                .unwrap()
                .contains(".length === 1")
        })
        .unwrap();
    let click = commands
        .iter()
        .position(|command| {
            command["params"]["expression"].as_str().unwrap().contains("e.click()")
                && command["params"]["expression"]
                    .as_str()
                    .unwrap()
                    .contains("Accept with audio")
        })
        .unwrap();
    assert!(check < click);
}

#[tokio::test]
async fn the_answered_elsewhere_phase_preserves_the_callers_hold_and_the_observers_silence() {
    let dir = tempfile::tempdir().unwrap();
    let binary = script(
        dir.path(),
        "printf 'calling_registration registered=true\\nincoming_call synthetic\\n0 ring_ended reason=\"accepted by another fork\"\\n'",
    );
    let mut run = run(binary, dir.path());
    let (mut page, _) = media_page().await;
    let mut session = page.session(dir.path(), true);
    fs::write(
        session.capture_dir().unwrap().join(EVENTS_FILE),
        format!("{}\n", invitation()),
    )
    .unwrap();
    let mut report = ScenarioReport::new(ScenarioName::AbCallNative);
    run.answered_elsewhere(&mut session, &mut report).await.unwrap();
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert!(run.natives.is_empty());
    assert!(report.notes.iter().any(|note| note.contains("b-watch: 0 ring_ended")));
    assert!(!report.notes.iter().any(|note| note.contains("call_answer")));
}

#[tokio::test]
async fn the_page_to_native_phase_waits_for_registration_answer_media_and_the_remote_end() {
    let dir = tempfile::tempdir().unwrap();
    let binary = script(
        dir.path(),
        "printf 'calling_registration registered=true\\ncall_answer call=1\\nstatus=\"connected\"\\nstatus=\"ended\"\\ntone hz=440 heard=true\\ncamera_on call=1\\nvideo received=100\\n'",
    );
    let mut run = run(binary, dir.path());
    let (mut page, capture) = media_page().await;
    let mut session = page.session(dir.path(), true);
    capture
        .lock()
        .unwrap()
        .replace(session.capture_dir().unwrap().join(EVENTS_FILE));
    let mut report = ScenarioReport::new(ScenarioName::AbCallNative);
    run.page_calls_native(&mut session, &mut report).await.unwrap();
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert!(run.natives.is_empty());
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("in the call a's native client answered"))
    );
    assert!(report.notes.iter().any(|note| note.contains("b: hung up")));
    assert!(report.notes.iter().any(|note| note.contains("a-watch: call_answer")));
    let commands = page.commands();
    let started = commands
        .iter()
        .position(|command| {
            command["params"]["expression"].as_str().unwrap().contains("e.click()")
                && command["params"]["expression"]
                    .as_str()
                    .unwrap()
                    .contains("chat-call-audio-button")
        })
        .unwrap();
    let hung_up = commands
        .iter()
        .position(|command| {
            command["params"]["expression"].as_str().unwrap().contains("e.click()")
                && command["params"]["expression"].as_str().unwrap().contains("hangup")
        })
        .unwrap();
    assert!(started < hung_up);
}

#[tokio::test]
async fn a_driver_failure_stops_every_native_then_closes_and_judges_the_armed_page() {
    let dir = tempfile::tempdir().unwrap();
    let binary = script(dir.path(), "exec /bin/sleep 60");
    let mut page = Page::new(|_| Err("synthetic browser disappeared".to_owned())).await;
    let session = page.session(dir.path(), true);
    let mut run = run(binary, dir.path());
    run.spawn(Alias::A, "synthetic-caller", &[]).unwrap();
    run.spawn(Alias::B, "synthetic-observer", &[]).unwrap();
    let report = finish_run(run, session, ScenarioReport::new(ScenarioName::AbCallNative)).await;
    assert_eq!(report.accounts.len(), 1);
    assert!(
        report.failures[0].contains("synthetic browser disappeared"),
        "{report:?}"
    );
    assert!(
        report
            .failures
            .iter()
            .any(|failure| failure.contains("didn't confirm closing"))
    );
    assert!(
        report
            .failures
            .iter()
            .any(|failure| failure.contains("fence allowed no call start"))
    );
    for child in ["synthetic-caller", "synthetic-observer"] {
        assert!(
            report
                .notes
                .iter()
                .any(|note| note.starts_with(child) && note.contains("stopped at the run's end")),
            "{report:?}"
        );
    }
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("couldn't hang up after the failure"))
    );
    assert!(report.notes.iter().any(|note| note.starts_with("b: recorded into")));
    let commands = page.commands();
    assert!(
        commands
            .iter()
            .any(|command| command["method"] == "Page.captureScreenshot")
    );
    assert!(commands.iter().any(|command| command["method"] == "Browser.close"));
}

fn calling_socket_capture() -> String {
    let connected = json!({"name": "trouter.connected", "args": []});
    [
        json!({"type":"cdp","method":"Network.webSocketCreated","params":{"requestId":"synthetic-calling","url":"wss://go.trouter.teams.microsoft.com/v4/c?tc=%7B%22ua%22:%22SkypeSpaces%22%7D"}}),
        json!({"type":"cdp","method":"Network.webSocketFrameReceived","params":{"requestId":"synthetic-calling","response":{"payloadData":format!("5:1::{connected}")}}}),
    ]
    .map(|event| event.to_string())
    .join("\n")
}

#[tokio::test]
async fn the_driver_loads_registers_and_observes_the_socket_before_refusing_a_foreign_ring() {
    let dir = tempfile::tempdir().unwrap();
    let binary = script(dir.path(), "exec /bin/sleep 60");
    let mut page = Page::new(|command| {
        Ok(if command["method"] == "Runtime.evaluate" {
            evaluated(json!(true))
        } else {
            json!({})
        })
    })
    .await;
    let mut session = page.loaded_session(dir.path(), true).await;
    let file = session.capture_dir().unwrap().join(EVENTS_FILE);
    fs::write(&file, format!("{}\n{}\n", calling_socket_capture(), invitation())).unwrap();
    let mut run = run(binary, dir.path());
    run.mri_b = "8:orgid:synthetic-outsider".into();
    let mut report = ScenarioReport::new(ScenarioName::AbCallNative);
    let error = run
        .drive_with(
            &mut session,
            &mut report,
            CallPauses {
                page_steady: Duration::ZERO,
                calling_warm: Duration::ZERO,
                ..CallPauses::default()
            },
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("isn't the run's call"), "{error}");
    assert!(report.notes.iter().any(|note| note == "b: the calling socket is up"));
    assert!(report.notes.iter().any(|note| note == "b: back at the chat"));
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("the Calls app started the calling stack"))
    );
    let commands = page.commands();
    assert!(commands.iter().any(|command| command["method"] == "Page.navigate"));
    assert!(!commands.iter().any(|command| {
        let expression = command["params"]["expression"].as_str().unwrap_or_default();
        expression.contains(ACCEPT_AUDIO_CSS) && expression.contains("e.click()")
    }));
    run.stop_natives(&mut session, &mut report).await;
    assert!(run.natives.is_empty());
}

#[tokio::test]
async fn a_native_running_call_conflict_ends_the_ring_wait_before_any_answer_is_clicked() {
    let dir = tempfile::tempdir().unwrap();
    let mut page = Page::new(|_| Err("synthetic ring is absent".to_owned())).await;
    let mut session = page.session(dir.path(), true);
    let mut run = run(PathBuf::from("/bin/true"), dir.path());
    run.spawn(Alias::A, "synthetic-conflict", &[]).unwrap();
    run.natives[0].wait(&mut session, Duration::from_secs(2)).await.unwrap();
    fs::write(
        dir.path().join("native-synthetic-conflict.txt"),
        "end=\"remote 409 chat has a running call https://synthetic.invalid/conv/old\"\n",
    )
    .unwrap();
    let mut report = ScenarioReport::new(ScenarioName::AbCallNative);
    let error = run
        .answer_on_page(&mut session, ("synthetic-conflict", now_ms()), &mut report)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("https://synthetic.invalid/conv/old"));
    assert!(error.to_string().contains("409, 5704"));
    assert_eq!(page.commands().len(), 1);
    assert!(report.notes.is_empty());
    run.stop_natives(&mut session, &mut report).await;
}

#[tokio::test]
async fn answered_elsewhere_reports_each_premature_end_unexpected_answer_and_bad_exit() {
    let dir = tempfile::tempdir().unwrap();
    let binary = script(
        dir.path(),
        r#"case "$1" in
  a) printf 'status="ended"\n'; touch "$0.ready" ;;
  b) printf 'calling_registration registered=true\nincoming_call synthetic\n'
     while [ ! -e "$0.ready" ]; do /bin/sleep 0.001; done
     printf '0 ring_ended reason="caller cancelled"\ncall_answer call=1\n' ;;
esac
exit 1"#,
    );
    let mut run = run(binary, dir.path());
    let (mut page, _) = media_page().await;
    let mut session = page.session(dir.path(), true);
    fs::write(
        session.capture_dir().unwrap().join(EVENTS_FILE),
        format!("{}\n", invitation()),
    )
    .unwrap();
    let mut report = ScenarioReport::new(ScenarioName::AbCallNative);
    run.answered_elsewhere(&mut session, &mut report).await.unwrap();
    for reason in [
        "didn't end as answered elsewhere",
        "ended before",
        "a's native client",
        "b's native client answered",
        "b's native client didn't exit 0",
    ] {
        assert!(
            report.failures.iter().any(|failure| failure.contains(reason)),
            "{report:?}"
        );
    }
    assert_eq!(report.failures.len(), 5, "{report:?}");
    assert!(run.natives.is_empty());
}

#[tokio::test]
async fn native_setup_keeps_its_pinned_page_media_and_settles_the_page_when_initial_loading_fails() {
    for record in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let page_dir = tempfile::tempdir().unwrap();
        let accounts = ["a", "b", "c"].map(|alias| {
            format!("[[accounts]]\nalias = '{alias}'\nupn = 'synthetic-{alias}@example.test'\nmri = '8:orgid:synthetic-{alias}'\n")
        });
        let config = LabConfig::parse(&accounts.join("\n"), root.path()).unwrap();
        let mut peer = Page::new(|command| {
            if command["method"] == "Browser.close" {
                Ok(json!({}))
            } else {
                Err("synthetic native page failed to load".to_owned())
            }
        })
        .await;
        let (session, pid) = peer.managed_session(page_dir.path(), record);
        let options = run(PathBuf::from("/bin/true"), root.path()).options;
        let report = run_with(
            "/bin/true".into(),
            &LabPaths::new(root.path().to_path_buf()),
            &config,
            options,
            async |options| {
                assert!(options.live_members && options.capture_request_bodies);
                assert_eq!(options.capture.as_deref(), Some("ab-call-native"));
                assert_eq!(options.user_agent.as_deref(), Some(USER_AGENT));
                assert_eq!(options.display, Display::Headless);
                assert!(matches!(options.media, Media::Fake { audio: Some(ref file) } if file.is_file()));
                Ok(session)
            },
        )
        .await
        .unwrap();
        assert_eq!(report.accounts.len(), 1);
        assert!(
            report.failures[0].contains("synthetic native page failed to load"),
            "{report:?}"
        );
        assert!(
            report
                .failures
                .iter()
                .all(|reason| !reason.contains("didn't confirm closing"))
        );
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
        assert!(
            peer.commands()
                .iter()
                .any(|command| command["method"] == "Browser.close")
        );
    }
}
