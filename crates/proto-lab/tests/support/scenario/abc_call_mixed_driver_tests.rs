//! Mixed-call phases with actual armed sessions and generated synthetic native-process output.

use base64::Engine as _;
use serde_json::json;

use super::*;
use crate::browser::Display;
use crate::scenario::ab_call::CallStep;
use crate::scenario::test_page::{Page as Peer, evaluated};

const CHAT: &str = "19:synthetic-group@thread.v2";
const A: &str = "8:orgid:synthetic-a";
const B: &str = "8:orgid:synthetic-b";
const C: &str = "8:orgid:synthetic-c";

fn mixed(dir: &Path) -> Mixed {
    Mixed {
        binary: "/bin/true".into(),
        dir: dir.into(),
        chat: CHAT.into(),
        call: RunCall {
            caller: A.into(),
            group_chat: Some(CHAT.into()),
        },
        native: None,
        placed: Some(100),
        observe_within: NATIVE_SHOWS,
        options: RunOptions {
            display: Display::Headless,
            load_timeout: Duration::from_secs(1),
            call: CallStep::Answer,
        },
    }
}

fn invitation(callee: &str) -> String {
    let payload = json!({"callNotification":{"from":{"id":A},"to":{"id":callee}},
        "conversationInvitation":{"conversationController":"https://synthetic.invalid/conv/call"},"groupChat":{"threadId":CHAT}});
    let gp = base64::engine::general_purpose::STANDARD.encode(payload.to_string());
    let body = json!({"evt":107,"gp":gp}).to_string();
    let frame = json!({"id":1,"method":"POST","url":"/v4/f/synthetic/","body":body});
    json!({"type":"cdp","ts_ms":101,"method":"Network.webSocketFrameReceived","params":{"response":{"payloadData":format!("3:::{frame}")}}}).to_string()
}

fn native_output(who: &[&str], frames: u64) -> String {
    let participants: Vec<Value> = who
        .iter()
        .enumerate()
        .map(|(i, name)| json!({"who":name,"line":i+1,"video_on":true}))
        .collect();
    format!(
        "1 call_participants participants={participants} speaking=[]\n{} video_lines received=1:{frames},2:{frames}\ntone hz=440 heard=true\ncamera_on call=1\nvideo received=1000\nvideo line=1 received=1000\nvideo line=2 received=1000\n",
        u64::MAX,
        participants = serde_json::to_string(&participants).unwrap()
    )
}

async fn native(run: &mut Mixed, session: &mut LabSession, who: &[&str], frames: u64) -> PathBuf {
    let mut native = spawn_native((&run.binary, &run.dir), Alias::A, CALLER, &[]).unwrap();
    assert!(
        native
            .wait(session, Duration::from_secs(2))
            .await
            .is_some_and(|status| status.success())
    );
    let file = run.dir.join(format!("native-{CALLER}.txt"));
    fs::write(&file, native_output(who, frames)).unwrap();
    assert!(native.output().contains("call_participants"));
    run.native = Some(native);
    file
}

async fn peer() -> Peer {
    Peer::new(|command| {
        if command["method"] == "Runtime.evaluate" {
            let expression = command["params"]["expression"].as_str().unwrap();
            if expression.contains("r.left + r.width / 2") {
                return Ok(evaluated(json!([10, 20])));
            }
            if expression.contains("return JSON.stringify({ sends:") {
                return Ok(evaluated(json!(
                    json!({"sends":1,"ours":1,"joins":1,"where":[]}).to_string()
                )));
            }
            return Ok(evaluated(json!(true)));
        }
        Ok(json!({}))
    })
    .await
}

#[tokio::test]
async fn native_observation_steps_need_a_running_client_and_use_its_last_camera_lines() {
    let dir = tempfile::tempdir().unwrap();
    let mut peer = peer().await;
    let mut session = peer.session(dir.path(), false);
    let mut run = mixed(dir.path());
    let mut report = ScenarioReport::new(ScenarioName::AbcCallMixed);
    assert!(run.wait_shown(&mut session, &["b"], &mut report).await.is_err());
    assert!(run.wait_counts_after(&mut session, 100, &mut report).await.is_err());
    assert!(
        run.wait_frames_since(&mut session, "b", &[], &mut report)
            .await
            .is_err()
    );
    native(&mut run, &mut session, &["b", "c"], 1000).await;
    run.wait_shown(&mut session, &["b", "c"], &mut report).await.unwrap();
    let counts = run.wait_counts_after(&mut session, 100, &mut report).await.unwrap();
    assert_eq!(counts, [(1, 1000), (2, 1000)]);
    run.wait_frames_since(&mut session, "c", &[(1, 0), (2, 0)], &mut report)
        .await
        .unwrap();
    assert_eq!(report.notes.len(), 3);
    assert!(report.failures.is_empty());
    run.native
        .take()
        .unwrap()
        .finished(Some(std::os::unix::process::ExitStatusExt::from_raw(0)), &mut report);
}

#[tokio::test]
async fn a_join_is_bound_to_the_runs_invitation_before_the_visible_chat_is_examined() {
    let dir = tempfile::tempdir().unwrap();
    let mut unrecorded = peer().await;
    let mut run = mixed(dir.path());
    let mut report = ScenarioReport::new(ScenarioName::AbcCallMixed);
    let mut page = Page {
        alias: Alias::C,
        mri: C.into(),
        session: unrecorded.session(dir.path(), false),
    };
    run.placed = None;
    assert!(
        run.check_join(&page, &mut report)
            .await
            .unwrap_err()
            .to_string()
            .contains("placed no call")
    );
    run.placed = Some(100);
    assert!(
        run.check_join(&page, &mut report)
            .await
            .unwrap_err()
            .to_string()
            .contains("records no capture")
    );
    drop(page);
    let mut recorded = peer().await;
    page = Page {
        alias: Alias::C,
        mri: C.into(),
        session: recorded.session(dir.path(), true),
    };
    let capture = page.session.capture_dir().unwrap().join(EVENTS_FILE);
    fs::write(&capture, "").unwrap();
    assert!(
        run.check_join(&page, &mut report)
            .await
            .unwrap_err()
            .to_string()
            .contains("no invitation")
    );
    fs::write(&capture, invitation("8:orgid:synthetic-outsider")).unwrap();
    assert!(run.check_join(&page, &mut report).await.is_err());
    assert!(recorded.commands().is_empty());
    fs::write(&capture, invitation(C)).unwrap();
    run.check_join(&page, &mut report).await.unwrap();
    assert!(report.notes[0].contains("run's chat and one join"));
    assert_eq!(recorded.commands().len(), 1);
    fs::remove_file(&capture).unwrap();
    assert!(
        run.check_join(&page, &mut report)
            .await
            .unwrap_err()
            .to_string()
            .contains("can't read the capture")
    );
}

#[tokio::test]
async fn leaving_and_rejoining_require_new_frames_after_the_observed_absence_then_finish() {
    let dir = tempfile::tempdir().unwrap();
    let mut b = peer().await;
    let (mut c, capture) = late_peer(dir.path()).await;
    let mut pages = [
        Page {
            alias: Alias::B,
            mri: B.into(),
            session: b.session(dir.path(), false),
        },
        Page {
            alias: Alias::C,
            mri: C.into(),
            session: c.session(dir.path(), true),
        },
    ];
    let file = pages[1].session.capture_dir().unwrap().join(EVENTS_FILE);
    capture.lock().unwrap().replace(file.clone());
    fs::write(&file, format!("{}\n", invitation(C))).unwrap();
    let mut run = mixed(dir.path());
    native(&mut run, &mut pages[0].session, &["b"], 1000).await;
    let mut report = ScenarioReport::new(ScenarioName::AbcCallMixed);
    assert!(run.c_leaves(&mut [], &mut report).await.is_err());
    assert!(run.c_joins_late(&mut [], 100, &mut report).await.is_err());
    let absent = run.c_leaves(&mut pages, &mut report).await.unwrap();
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        run.c_joins_late(&mut pages, absent, &mut report),
    )
    .await;
    assert!(
        outcome.is_ok(),
        "{report:?}; commands={:?}; capture={}",
        c.commands(),
        fs::read_to_string(&file).unwrap()
    );
    outcome.unwrap().unwrap();
    run.finish(&mut pages, &mut report).await.unwrap();
    assert!(run.native.is_none());
    assert!(report.failures.is_empty(), "{report:?}");
    assert!(report.notes.iter().any(|note| note.contains("frames since the join")));
}

type CaptureFile = std::sync::Arc<std::sync::Mutex<Option<PathBuf>>>;

struct LatePage {
    output: PathBuf,
    capture: CaptureFile,
}

impl LatePage {
    fn clicked(&self, expression: &str) {
        use std::io::Write as _;
        match selected_control(expression) {
            "join" => fs::write(&self.output, native_output(&["b", "c"], 2000)).unwrap(),
            "camera" => {
                let path = self.capture.lock().unwrap();
                let mut file = fs::OpenOptions::new()
                    .append(true)
                    .open(path.as_ref().unwrap())
                    .unwrap();
                writeln!(file, "{}", json!({"type":"rtc","payload":{"event":"stats","data":[{"type":"inbound-rtp","kind":"video","framesDecoded":2000}]}})).unwrap();
            }
            _ => {}
        }
    }

    fn answer(&self, command: &Value) -> Value {
        if command["method"] != "Runtime.evaluate" {
            return json!({});
        }
        let expression = command["params"]["expression"].as_str().unwrap();
        if expression.contains("e.click()") {
            self.clicked(expression);
        }
        if expression.contains("return JSON.stringify({ sends:") {
            return evaluated(json!(json!({"sends":1,"ours":1,"joins":1}).to_string()));
        }
        evaluated(json!(true))
    }
}

async fn late_peer(dir: &Path) -> (Peer, CaptureFile) {
    let capture: CaptureFile = std::sync::Arc::new(std::sync::Mutex::new(None));
    let context = LatePage {
        output: dir.join(format!("native-{CALLER}.txt")),
        capture: std::sync::Arc::clone(&capture),
    };
    let page = Peer::new(move |command| Ok(context.answer(command))).await;
    (page, capture)
}

fn selected_control(expression: &str) -> &'static str {
    if expression.contains("chat-calling-meeting-buttons") {
        "join"
    } else if expression.contains("video-button") {
        "camera"
    } else {
        ""
    }
}

#[tokio::test]
async fn observation_deadlines_refuse_stale_counts_missing_people_and_old_video() {
    let dir = tempfile::tempdir().unwrap();
    let mut peer = peer().await;
    let mut session = peer.session(dir.path(), false);
    let mut run = mixed(dir.path());
    assert_eq!(run.observe_within, Duration::from_secs(45));
    run.observe_within = Duration::ZERO;
    let output = native(&mut run, &mut session, &["b"], 100).await;
    let mut report = ScenarioReport::new(ScenarioName::AbcCallMixed);
    assert!(
        run.wait_shown(&mut session, &["b", "c"], &mut report)
            .await
            .unwrap_err()
            .to_string()
            .contains("didn't show")
    );
    fs::write(&output, native_output(&["b"], 100).replace(&u64::MAX.to_string(), "99")).unwrap();
    assert!(
        run.wait_counts_after(&mut session, 100, &mut report)
            .await
            .unwrap_err()
            .to_string()
            .contains("no video counts after")
    );
    assert!(
        run.wait_frames_since(&mut session, "c", &[], &mut report)
            .await
            .unwrap_err()
            .to_string()
            .contains("frames since the join")
    );
    assert!(
        run.wait_frames_since(&mut session, "b", &[(1, 100)], &mut report)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn answering_and_media_steps_require_the_runs_invitation_and_recorded_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let mut unrecorded = peer().await;
    let mut page = Page {
        alias: Alias::B,
        mri: B.into(),
        session: unrecorded.session(dir.path(), false),
    };
    let run = mixed(dir.path());
    let mut report = ScenarioReport::new(ScenarioName::AbcCallMixed);
    assert!(
        answer(&mut page, &run.call, 100, &mut report)
            .await
            .unwrap_err()
            .to_string()
            .contains("capture")
    );
    assert!(
        see(&mut page, 0, &mut report)
            .await
            .unwrap_err()
            .to_string()
            .contains("no capture")
    );
    assert!(
        hear(&mut page, 0, &mut report)
            .await
            .unwrap_err()
            .to_string()
            .contains("no capture")
    );
    drop(page);
    let mut recorded = peer().await;
    let mut page = Page {
        alias: Alias::B,
        mri: B.into(),
        session: recorded.session(dir.path(), true),
    };
    let file = page.session.capture_dir().unwrap().join(EVENTS_FILE);
    fs::write(&file, format!("{}\n", invitation(B))).unwrap();
    answer(&mut page, &run.call, 100, &mut report).await.unwrap();
    assert!(report.notes.iter().any(|note| note.contains("clicked answer")));
    assert!(report.notes.iter().any(|note| note.contains("in the call")));
    fs::OpenOptions::new().append(true).open(&file).unwrap();
    let mut text = fs::read_to_string(&file).unwrap();
    text.push_str(&json!({"type":"rtc","payload":{"event":"stats","data":[{"type":"inbound-rtp","kind":"video","framesDecoded":2000}]}}).to_string());
    fs::write(&file, text).unwrap();
    see(&mut page, 0, &mut report).await.unwrap();
    assert!(report.notes.iter().any(|note| note.contains("decoded 2000 video")));
}

fn argument_script(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let file = dir.join("synthetic-native-arguments");
    fs::write(
        &file,
        "#!/bin/sh\n# Synthetic offline argument recorder\nprintf '%s\\n' \"$@\"\n",
    )
    .unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o700)).unwrap();
    file
}

#[tokio::test]
async fn initial_group_call_starts_the_native_with_video_and_preserves_the_first_page_failure() {
    for with_page in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut observer = peer().await;
        let mut session = observer.session(dir.path(), false);
        let mut page_peer = peer().await;
        let mut pages = if with_page {
            vec![Page {
                alias: Alias::B,
                mri: B.into(),
                session: page_peer.session(dir.path(), false),
            }]
        } else {
            Vec::new()
        };
        let mut run = mixed(dir.path());
        run.binary = argument_script(dir.path());
        run.placed = None;
        let mut report = ScenarioReport::new(ScenarioName::AbcCallMixed);
        let error = run.everyone_in(&mut pages, &mut report).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains(if with_page { "capture" } else { "no page" })
        );
        assert!(run.placed.is_some_and(|placed| placed >= 100));
        let mut native = run.native.take().unwrap();
        let status = native.wait(&mut session, Duration::from_secs(2)).await;
        let finished = native.finished(status, &mut report);
        assert!(finished.status.is_some_and(|status| status.success()));
        let args: Vec<_> = finished.output.lines().collect();
        assert_eq!(
            args,
            [
                "a",
                "call",
                "abc",
                "--tone",
                "440",
                "--bursts",
                "--hear",
                "440",
                "--video",
                "--camera-after",
                "5",
                "--seconds",
                "200",
                "--wait",
                "90"
            ]
        );
        assert!(report.notes[0].contains("phase 1"));
    }
}

#[tokio::test]
async fn a_driver_failure_attempts_each_cleanup_stops_the_native_and_settles_both_pages() {
    let dir = tempfile::tempdir().unwrap();
    let mut b = Peer::new(|_| Err("synthetic page disappeared".to_owned())).await;
    let mut c = Peer::new(|_| Err("synthetic page disappeared".to_owned())).await;
    fs::create_dir_all(dir.path().join("b")).unwrap();
    fs::create_dir_all(dir.path().join("c")).unwrap();
    let mut pages = vec![
        Page {
            alias: Alias::B,
            mri: B.into(),
            session: b.session(&dir.path().join("b"), true),
        },
        Page {
            alias: Alias::C,
            mri: C.into(),
            session: c.session(&dir.path().join("c"), true),
        },
    ];
    let mut run = mixed(dir.path());
    native(&mut run, &mut pages[0].session, &["b", "c"], 1000).await;
    let report = finish_run(run, pages, ScenarioReport::new(ScenarioName::AbcCallMixed)).await;
    assert_eq!(report.accounts.len(), 2);
    assert!(report.failures[0].contains("synthetic page disappeared"), "{report:?}");
    assert_eq!(
        report
            .failures
            .iter()
            .filter(|failure| failure.contains("didn't confirm closing"))
            .count(),
        2,
        "{report:?}"
    );
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("stopped at the run's end"))
    );
    assert_eq!(
        report
            .notes
            .iter()
            .filter(|note| note.contains("couldn't hang up after the failure"))
            .count(),
        2
    );
    for commands in [b.commands(), c.commands()] {
        assert!(commands.iter().any(|command| command["method"] == "Browser.close"));
        assert!(
            commands
                .iter()
                .any(|command| command["method"] == "Page.captureScreenshot")
        );
    }
    assert!(report.notes.iter().any(|note| note.starts_with("b: recorded into")));
    assert!(report.notes.iter().any(|note| note.starts_with("c: recorded into")));
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
async fn the_driver_loads_each_calling_stack_before_starting_and_refuses_a_foreign_ring() {
    let b_dir = tempfile::tempdir().unwrap();
    let c_dir = tempfile::tempdir().unwrap();
    let mut b = peer().await;
    let mut c = peer().await;
    let b_session = b.loaded_session(b_dir.path(), true).await;
    let c_session = c.loaded_session(c_dir.path(), true).await;
    for session in [&b_session, &c_session] {
        let file = session.capture_dir().unwrap().join(EVENTS_FILE);
        fs::write(&file, format!("{}\n{}\n", calling_socket_capture(), invitation(B))).unwrap();
    }
    let mut pages = [
        Page {
            alias: Alias::B,
            mri: "8:orgid:synthetic-outsider".into(),
            session: b_session,
        },
        Page {
            alias: Alias::C,
            mri: C.into(),
            session: c_session,
        },
    ];
    let mut run = mixed(b_dir.path());
    let mut report = ScenarioReport::new(ScenarioName::AbcCallMixed);
    let error = run
        .drive_with(
            &mut pages,
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
    for alias in ["b", "c"] {
        assert!(
            report
                .notes
                .iter()
                .any(|note| note == &format!("{alias}: the calling socket is up"))
        );
        assert!(
            report
                .notes
                .iter()
                .any(|note| note == &format!("{alias}: the Calls app started the calling stack"))
        );
    }
    for commands in [b.commands(), c.commands()] {
        assert!(commands.iter().any(|command| command["method"] == "Page.navigate"));
        assert!(!commands.iter().any(|command| {
            let expression = command["params"]["expression"].as_str().unwrap_or_default();
            expression.contains(ACCEPT_AUDIO_CSS) && expression.contains("e.click()")
        }));
    }
    let native = run.native.take().unwrap();
    let finished = stop_native(native, &mut pages[0].session, &mut report).await;
    assert!(finished.status.is_some());
}

fn audible_capture() -> String {
    let stats = json!({"type":"rtc","payload":{"event":"stats","data":[
        {"type":"outbound-rtp","kind":"audio","packetsSent":40},
        {"type":"inbound-rtp","kind":"audio","packetsReceived":30,"totalAudioEnergy":0.5}
    ]}});
    let tone = json!({"type":"rtc","session":"synthetic","context":1,"payload":{"event":"audio","pc":1,"data":{"track":"synthetic-track","rms":0.25,"tone":0.99}}});
    format!("{stats}\n{}", format!("{tone}\n").repeat(10))
}

#[tokio::test]
async fn media_evidence_distinguishes_audible_duplex_audio_from_silence_and_missing_video() {
    for audible in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut peer = peer().await;
        let mut page = Page {
            alias: Alias::B,
            mri: B.into(),
            session: peer.session(dir.path(), true),
        };
        let file = page.session.capture_dir().unwrap().join(EVENTS_FILE);
        fs::write(&file, if audible { audible_capture() } else { String::new() }).unwrap();
        let mut report = ScenarioReport::new(ScenarioName::AbcCallMixed);
        hear_within(
            &mut page,
            0,
            &mut report,
            if audible {
                Duration::from_secs(1)
            } else {
                Duration::ZERO
            },
        )
        .await
        .unwrap();
        if audible {
            assert!(report.failures.is_empty(), "{report:?}");
            assert!(report.notes.iter().any(|note| note.contains("heard the tone")));
        } else {
            assert_eq!(report.failures.len(), 2, "{report:?}");
            assert!(report.failures[0].contains("didn't hear the tone"));
            assert!(report.failures[1].contains("didn't go both ways"));
        }
        see_within(&mut page, 0, &mut report, Duration::ZERO).await.unwrap();
        assert!(report.failures.last().unwrap().contains("decoded 0 video frames"));
    }
}

#[tokio::test]
async fn opening_the_pages_preserves_media_capture_options_and_arms_probe_denies_before_use() {
    use crate::scenario::ab_chat_full::tests::page;
    let root = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let c_dir = tempfile::tempdir().unwrap();
    let mut b = page(false, false).await;
    let mut c = page(false, false).await;
    let (b_session, b_pid) = b.managed_session(b_dir.path(), true);
    let (c_session, c_pid) = c.managed_session(c_dir.path(), true);
    let mut sessions = std::collections::VecDeque::from([b_session, c_session]);
    let mut opened = Vec::new();
    let paths = LabPaths::new(root.path().to_path_buf());
    let options = mixed(root.path()).options;
    let mut pages = open_pages_with(
        &paths,
        options,
        [(Alias::B, B.into()), (Alias::C, C.into())],
        async |alias, options| {
            opened.push(alias);
            assert!(options.live_members && options.capture_request_bodies);
            assert_eq!(options.capture.as_deref(), Some("abc-call-mixed"));
            assert_eq!(options.user_agent.as_deref(), Some(USER_AGENT));
            assert_eq!(options.display, Display::Headless);
            assert!(matches!(options.media, Media::Fake { audio: Some(ref file) } if file.is_file()));
            Ok(sessions.pop_front().unwrap())
        },
    )
    .await
    .unwrap();
    assert_eq!(opened, [Alias::B, Alias::C]);
    for page in &mut pages {
        let text = crate::scenario::ab_chat::probe_text("synthetic-opening-probe");
        crate::scenario::ab_chat::send(&mut page.session.driver(), "synthetic probe", &text)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while page.session.denials().is_empty() {
                assert!(page.session.pump(Duration::from_millis(10)).await);
            }
        })
        .await
        .unwrap();
        assert_eq!(page.session.denials()[0].label.as_deref(), Some(EARLIER_RUN_PROBE));
    }
    for page in pages {
        page.session.close().await.unwrap();
    }
    for pid in [b_pid, c_pid] {
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }
}

#[tokio::test]
async fn failure_to_open_the_second_page_closes_and_reaps_the_first_armed_session() {
    let root = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let mut b = Peer::new(|_| Ok(json!({}))).await;
    let (first, pid) = b.managed_session(b_dir.path(), true);
    let mut first = Some(first);
    let mut aliases = Vec::new();
    let result = open_pages_with(
        &LabPaths::new(root.path().to_path_buf()),
        mixed(root.path()).options,
        [(Alias::B, B.into()), (Alias::C, C.into())],
        async |alias, _| {
            aliases.push(alias);
            first
                .take()
                .ok_or_else(|| SessionError::NoChrome("synthetic-second-page".into()))
        },
    )
    .await;
    assert!(result.err().unwrap().to_string().contains("synthetic-second-page"));
    assert_eq!(aliases, [Alias::B, Alias::C]);
    assert!(b.commands().iter().any(|command| command["method"] == "Browser.close"));
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
}

#[tokio::test]
async fn finishing_refuses_missing_state_and_reports_only_one_proven_video_line_then_attempts_hangup() {
    let dir = tempfile::tempdir().unwrap();
    let mut peer = Peer::new(|_| Err("synthetic call screen vanished".to_owned())).await;
    let mut pages = [Page {
        alias: Alias::B,
        mri: B.into(),
        session: peer.session(dir.path(), false),
    }];
    let mut run = mixed(dir.path());
    let mut report = ScenarioReport::new(ScenarioName::AbcCallMixed);
    assert!(
        run.finish(&mut [], &mut report)
            .await
            .unwrap_err()
            .to_string()
            .contains("no page")
    );
    assert!(
        run.finish(&mut pages, &mut report)
            .await
            .unwrap_err()
            .to_string()
            .contains("no native client")
    );
    let file = native(&mut run, &mut pages[0].session, &["b"], 1000).await;
    fs::write(&file, "tone hz=440 heard=true\ncamera_on call=1\nvideo received=1000\nvideo line=1 received=1000\nvideo line=2 received=1\n").unwrap();
    run.finish(&mut pages, &mut report).await.unwrap();
    assert!(run.native.is_none());
    assert_eq!(report.failures.len(), 1, "{report:?}");
    assert!(report.failures[0].contains("frames on 1 line(s), not 2"));
    let commands = peer.commands();
    assert_eq!(commands.len(), 2);
    assert!(commands.iter().all(|command| command["method"] == "Runtime.evaluate"));
    assert!(
        commands[0]["params"]["expression"]
            .as_str()
            .unwrap()
            .contains("=== null")
    );
    assert!(
        commands[1]["params"]["expression"]
            .as_str()
            .unwrap()
            .contains("!== null")
    );
}
