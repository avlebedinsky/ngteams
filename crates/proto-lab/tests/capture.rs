//! The capture recorder: where captures go and how entries are written (offline), the WebRTC hooks' place in the
//! arming, and, with Chrome installed, a recorded run under the armed fence on local pages and a local echo server (its
//! `Network` events, response bodies, frames and the fence's verdicts), and the WebRTC hooks on a loopback call between
//! two `RTCPeerConnection`s in one page.
//!
//! The Chrome tests run headless on throwaway user-data-dirs against local servers; see `common` for how Chrome is
//! found.

mod common;
#[path = "common/page.rs"]
mod page;

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt as _, StreamExt as _};
use page::evaluate;
use proto_lab::arming::{ArmOptions, ArmedBrowser, ArmedTarget, Step, TargetKind};
use proto_lab::browser::{Browser, LaunchOptions};
use proto_lab::capture::{Capture, CaptureError, EVENTS_FILE, RTC_BINDING, Recorder, RtcHooks, capture_dir_name};
use proto_lab::cdp::{Cdp, SessionId};
use proto_lab::classify::{Classifier, OwnKind};
use proto_lab::fence_http::HttpFence;
use proto_lab::fence_ws::{FrameClassifier, FrameRule, GuardScript, WsFence};
use serde_json::{Value, json};
use teams_fence::{FailClosed, Fence, FenceLog, Roster};
use tokio_tungstenite::tungstenite::Message;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// How long one step of a live test may take.
const STEP: Duration = Duration::from_secs(20);

// --- where captures go ---

fn at(seconds: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(seconds)
}

#[test]
fn a_capture_dir_is_named_by_its_utc_time() {
    assert_eq!(capture_dir_name(at(0)), "19700101T000000Z");
    assert_eq!(capture_dir_name(at(1_729_351_234)), "20241019T152034Z");
    assert_eq!(capture_dir_name(at(1_709_168_461)), "20240229T010101Z");
    assert_eq!(capture_dir_name(at(4_102_444_799)), "20991231T235959Z");
}

fn mode(path: &Path) -> TestResult<u32> {
    Ok(std::fs::metadata(path)?.permissions().mode() & 0o777)
}

#[test]
fn a_capture_goes_to_its_scenario_and_time_private_to_the_user() {
    let root = tempfile::tempdir().unwrap();
    let captures = root.path().join("captures");
    let first = Recorder::create(&captures, "load").unwrap();
    let second = Recorder::create(&captures, "load").unwrap();
    assert_eq!(first.dir().parent().unwrap(), captures.join("load"));
    let name = first.dir().file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(name.len(), "20261019T225704Z".len(), "{name}");
    assert!(name.ends_with('Z') && name.as_bytes()[8] == b'T', "{name}");
    // a second capture in the same second gets a dir of its own
    assert_ne!(first.dir(), second.dir());
    assert_eq!(first.events_file(), first.dir().join(EVENTS_FILE));
    assert!(first.events_file().is_file());
    assert_eq!(mode(&captures).unwrap(), 0o700);
    assert_eq!(mode(&captures.join("load")).unwrap(), 0o700);
    assert_eq!(mode(first.dir()).unwrap(), 0o700);
    assert_eq!(mode(&first.events_file()).unwrap(), 0o600);
}

#[test]
fn a_scenario_name_is_a_plain_lowercase_name() {
    let root = tempfile::tempdir().unwrap();
    for bad in ["", "../x", "a/b", "Load", "-x", ".x", "a b", &"x".repeat(65)] {
        let error = Recorder::create(root.path(), bad).err();
        assert!(matches!(error, Some(CaptureError::BadScenario(_))), "{bad:?}");
    }
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0, "nothing made");
    Recorder::create(root.path(), "echo-call_2").unwrap();
}

fn read_entries(file: &Path) -> TestResult<Vec<Value>> {
    let text = std::fs::read_to_string(file)?;
    Ok(text.lines().map(serde_json::from_str).collect::<Result<_, _>>()?)
}

#[test]
fn entries_are_numbered_timestamped_and_written_at_once() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::create(root.path(), "unit").unwrap();
    let before = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis();
    recorder.record(json!({"type": "cdp", "method": "Network.x", "params": {"a": 1}}));
    // readable before finish: nothing is buffered
    assert_eq!(read_entries(&recorder.events_file()).unwrap().len(), 1);
    recorder.record(json!("not an object"));
    recorder.finish().unwrap();
    let entries = read_entries(&recorder.events_file()).unwrap();
    assert_eq!(recorder.entries(), 2);
    assert_eq!(entries[0]["seq"], 1);
    assert_eq!(entries[0]["type"], "cdp");
    assert_eq!(entries[0]["params"]["a"], 1);
    assert!(u128::from(entries[0]["ts_ms"].as_u64().unwrap()) >= before);
    assert_eq!(entries[1]["seq"], 2);
    assert_eq!(entries[1]["value"], "not an object");
}

// --- the hooks in the arming ---

#[test]
fn the_hooks_render_with_their_binding_and_interval() {
    let hooks = RtcHooks::new(Duration::from_millis(250));
    assert!(hooks.source().contains(&format!("'{RTC_BINDING}'")));
    assert!(hooks.source().contains("STATS_INTERVAL_MS = 250;"));
    assert!(!hooks.source().contains("{{"), "every placeholder filled");
    assert!(RtcHooks::default().source().contains("STATS_INTERVAL_MS = 1000;"));
}

#[test]
fn pages_and_iframes_get_the_hooks_after_the_guard_and_before_they_run() {
    for kind in [TargetKind::Page, TargetKind::Iframe] {
        let steps = kind.steps();
        let position = |wanted| steps.iter().position(|step| *step == wanted).unwrap();
        assert!(position(Step::GuardNewDocuments) < position(Step::CaptureBinding));
        assert!(position(Step::CaptureBinding) < position(Step::CaptureHooks));
        assert!(position(Step::CaptureHooks) < position(Step::Resume));
        assert!(
            position(Step::Page) < position(Step::CaptureHooks),
            "new-document scripts need Page on"
        );
        assert!(
            position(Step::Runtime) < position(Step::CaptureBinding),
            "bindings need Runtime on"
        );
    }
    // no RTCPeerConnection in a worker
    for kind in [TargetKind::Worker, TargetKind::SharedWorker, TargetKind::ServiceWorker] {
        assert!(!kind.steps().contains(&Step::CaptureHooks), "{kind:?}");
        assert!(!kind.steps().contains(&Step::CaptureBinding), "{kind:?}");
    }
}

// --- live runs ---

const PAGES: [(&str, &str, &str); 4] = [
    ("capture.html", include_str!("fixtures/pages/capture.html"), "text/html"),
    ("rtc.html", include_str!("fixtures/pages/rtc.html"), "text/html"),
    ("data.json", r#"{"hello":"lab"}"#, "application/json"),
    ("favicon.ico", "", "image/x-icon"),
];

async fn serve_pages() -> MockServer {
    let server = MockServer::start().await;
    for (name, body, content_type) in PAGES {
        Mock::given(method("GET"))
            .and(path(format!("/pages/{name}")))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, content_type))
            .mount(&server)
            .await;
    }
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    server
}

/// A local WebSocket server that echoes every text message and records it.
async fn echo_server() -> TestResult<(String, Arc<Mutex<Vec<String>>>)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("ws://{}", listener.local_addr()?);
    let received = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&received);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(echo(stream, Arc::clone(&recorded)));
        }
    });
    Ok((origin, received))
}

async fn echo(stream: tokio::net::TcpStream, recorded: Arc<Mutex<Vec<String>>>) {
    let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };
    while let Some(Ok(Message::Text(text))) = socket.next().await {
        recorded
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(text.as_str().to_owned());
        if socket.send(Message::Text(text)).await.is_err() {
            return;
        }
    }
}

/// Polls the capture until `wanted` holds for its entries, and gives them.
async fn entries_until(file: &Path, wanted: impl Fn(&[Value]) -> bool) -> TestResult<Vec<Value>> {
    let poll = async {
        loop {
            let entries = read_entries(file)?;
            if wanted(&entries) {
                return Ok(entries);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    tokio::time::timeout(STEP, poll)
        .await
        .map_err(|_| format!("the capture never got what the test waits for within {STEP:?}"))?
}

/// [`page::evaluate_until`], for up to [`STEP`].
async fn evaluate_until(cdp: &Cdp, session: &SessionId, expression: &str) -> TestResult<Value> {
    page::evaluate_until(cdp, session, expression, STEP).await
}

/// [`page::navigate`]s the tab and waits up to [`STEP`] for the page to load.
async fn navigate(cdp: &Cdp, tab: &ArmedTarget, url: &str) -> TestResult {
    page::navigate(cdp, &tab.session, url, STEP).await
}

/// One recorded run, armed on a throwaway profile; `echo` (when given) stands in for an own-state socket for
/// `allow:` frames.
struct Run {
    armed: ArmedBrowser,
    recorder: Arc<Recorder>,
    page: ArmedTarget,
    _dirs: tempfile::TempDir,
}

async fn start_run(pages: &MockServer, echo: Option<&str>, stats: Duration) -> TestResult<Option<Run>> {
    let Some(chrome) = common::chrome_or_skip() else {
        return Ok(None);
    };
    let dirs = tempfile::tempdir()?;
    let browser = Browser::launch(&chrome, &LaunchOptions::new(dirs.path().join("profile")), STEP).await?;
    let fence = Arc::new(Fence::new(
        Roster::new([], []),
        FenceLog::open(&dirs.path().join("fence.log"))?,
    ));
    let http = HttpFence::new(Classifier::seeded(), Arc::clone(&fence), Arc::new(FailClosed));
    let mut frames = FrameClassifier::seeded();
    if let Some(origin) = echo {
        frames = frames.with_own(FrameRule::new(OwnKind::Authz, origin, "/*", "allow:"));
    }
    let ws = WsFence::new(frames, fence, Arc::new(FailClosed), GuardScript::generate()?);
    let recorder = Arc::new(Recorder::create(&dirs.path().join("captures"), "test")?);
    let capture = Capture::new(Arc::clone(&recorder), RtcHooks::new(stats));
    let options = ArmOptions {
        service_worker_origins: vec![pages.uri()],
    };
    let armed = ArmedBrowser::arm_capturing(browser, http, ws, &options, capture).await?;
    let page = armed.open_page(STEP).await?;
    Ok(Some(Run {
        armed,
        recorder,
        page,
        _dirs: dirs,
    }))
}

/// Runs `body` on a recorded run, then closes the browser whatever `body` returned. Without Chrome it does nothing.
async fn with_run<F>(pages: &MockServer, echo: Option<&str>, stats: Duration, body: F) -> TestResult
where
    F: AsyncFnOnce(&Run) -> TestResult,
{
    let Some(run) = start_run(pages, echo, stats).await? else {
        return Ok(());
    };
    let outcome = tokio::time::timeout(Duration::from_secs(90), body(&run)).await;
    run.armed.close(Duration::from_secs(10)).await?;
    run.recorder.finish()?;
    outcome.map_err(|_| "the run took over 90 s")?
}

fn of_type<'a>(entries: &'a [Value], kind: &str) -> impl Iterator<Item = &'a Value> + 'a {
    let kind = kind.to_owned();
    entries.iter().filter(move |entry| entry["type"] == kind.as_str())
}

fn cdp_events<'a>(entries: &'a [Value], method: &str) -> Vec<&'a Value> {
    of_type(entries, "cdp")
        .filter(|entry| entry["method"] == method)
        .collect()
}

/// The `Network` request id of the request to `path`.
fn request_id(entries: &[Value], path: &str) -> Option<String> {
    cdp_events(entries, "Network.requestWillBeSent")
        .into_iter()
        .find(|entry| {
            entry["params"]["request"]["url"]
                .as_str()
                .is_some_and(|url| url.ends_with(path))
        })
        .and_then(|entry| entry["params"]["requestId"].as_str().map(str::to_owned))
}

fn has_body(entries: &[Value], path: &str) -> bool {
    let Some(id) = request_id(entries, path) else {
        return false;
    };
    of_type(entries, "response_body").any(|entry| entry["request_id"] == id.as_str())
}

#[tokio::test]
async fn a_recorded_run_holds_the_network_the_frames_the_bodies_and_every_verdict() {
    let pages = serve_pages().await;
    let (echo, received) = echo_server().await.unwrap();
    with_run(&pages, Some(&echo), Duration::from_secs(1), async |run| {
        let cdp = run.armed.cdp();
        let url = format!("{}/pages/capture.html?ws={echo}", pages.uri());
        navigate(cdp, &run.page, &url).await?;
        let expression = "results.get && results.post && results.echo.length === 2 ? results : null";
        let results = evaluate_until(cdp, &run.page.session, expression).await?;
        assert_eq!(results["get"], "lab");
        assert_eq!(
            results["post"], "error TypeError",
            "the page sees the deny as a network error"
        );
        assert_eq!(results["echo"], json!(["allow:1", "allow:2"]));
        let file = run.recorder.events_file();
        let entries = entries_until(&file, |entries| has_body(entries, "/pages/data.json")).await?;
        check_recorded_run(&entries)?;
        assert_eq!(
            *received.lock().unwrap_or_else(PoisonError::into_inner),
            ["allow:1", "allow:2"]
        );
        // the run fails on the denied request and frame, as any run with a deny does
        let failed = run.armed.verdict().unwrap_err();
        assert_eq!((failed.denials.len(), failed.frame_denials.len()), (1, 1), "{failed:?}");
        Ok(())
    })
    .await
    .unwrap();
    let posted = pages.received_requests().await.unwrap_or_default();
    assert!(
        posted.iter().all(|request| request.method.as_str() == "GET"),
        "a denied POST got out"
    );
}

fn check_recorded_run(entries: &[Value]) -> TestResult {
    for (index, entry) in entries.iter().enumerate() {
        assert_eq!(entry["seq"], index + 1, "entries are numbered in file order");
        assert!(entry["ts_ms"].is_u64());
    }
    assert!(!cdp_events(entries, "Target.attachedToTarget").is_empty());
    // the GET: its request, response and body, and the fence's allow
    let get = request_id(entries, "/pages/data.json").ok_or("the GET wasn't recorded")?;
    let body = of_type(entries, "response_body")
        .find(|entry| entry["request_id"] == get.as_str())
        .ok_or("its body wasn't recorded")?;
    assert_eq!(body["body"], r#"{"hello":"lab"}"#);
    assert!(
        cdp_events(entries, "Network.responseReceived")
            .iter()
            .any(|entry| entry["params"]["requestId"] == get.as_str())
    );
    let verdicts: Vec<&Value> = of_type(entries, "verdict").collect();
    check_http_verdicts(entries, &verdicts, &get);
    check_recorded_frames(entries, &verdicts);
    Ok(())
}

fn check_http_verdicts(entries: &[Value], verdicts: &[&Value], get: &str) {
    let on = |path: &str, verdict: &str| -> Vec<&Value> {
        verdicts
            .iter()
            .copied()
            .filter(|entry| entry["url"].as_str().is_some_and(|url| url.ends_with(path)) && entry["verdict"] == verdict)
            .collect()
    };
    // allowed by the page's layer, then by the browser's
    let allowed = on("/pages/data.json", "allow");
    assert_eq!(allowed.len(), 2, "{allowed:?}");
    assert!(
        allowed
            .iter()
            .all(|entry| entry["network_id"] == get && entry["method"] == "GET")
    );
    let layers = allowed.iter().filter(|entry| entry["session"].is_null()).count();
    assert_eq!(
        layers, 1,
        "one allow on the page's session, one on the browser's: {allowed:?}"
    );
    // the POST: denied by the page's layer (so the browser's never sees it), with the whole request kept for mapping
    let denied = on("/sink", "deny");
    assert_eq!(denied.len(), 1, "{denied:?}");
    assert_eq!(denied[0]["channel"], "http");
    assert_eq!(denied[0]["method"], "POST");
    assert_eq!(denied[0]["cause"], "unclassified");
    assert_eq!(denied[0]["request"]["postData"], "lab-post");
    assert!(denied[0]["session"].is_string());
    assert!(
        cdp_events(entries, "Network.requestWillBeSent")
            .iter()
            .any(|entry| entry["params"]["request"]["url"]
                .as_str()
                .is_some_and(|url| url.ends_with("/sink"))),
        "the denied request's own network event is there too"
    );
}

fn check_recorded_frames(entries: &[Value], verdicts: &[&Value]) {
    let frames: Vec<(String, String)> = verdicts
        .iter()
        .filter(|entry| entry["channel"] == "web_socket")
        .map(|entry| {
            let data = entry["frame"]["data"].as_str().unwrap_or_default().to_owned();
            (data, entry["verdict"].as_str().unwrap_or_default().to_owned())
        })
        .collect();
    let expected = [("allow:1", "allow"), ("deny:1", "deny"), ("allow:2", "allow")];
    let expected: Vec<(String, String)> = expected
        .iter()
        .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
        .collect();
    assert_eq!(frames, expected);
    let sent: Vec<&str> = cdp_events(entries, "Network.webSocketFrameSent")
        .iter()
        .filter_map(|entry| entry["params"]["response"]["payloadData"].as_str())
        .collect();
    assert_eq!(sent, ["allow:1", "allow:2"]);
    assert_eq!(cdp_events(entries, "Network.webSocketFrameReceived").len(), 2);
    assert_eq!(cdp_events(entries, "Network.webSocketCreated").len(), 1);
}

/// The hooks' reports, as (connection, event, data).
fn rtc_reports(entries: &[Value]) -> Vec<(u64, String, Value)> {
    of_type(entries, "rtc")
        .map(|entry| {
            let payload = &entry["payload"];
            (
                payload["pc"].as_u64().unwrap_or_default(),
                payload["event"].as_str().unwrap_or_default().to_owned(),
                payload["data"].clone(),
            )
        })
        .collect()
}

fn reported<'a>(reports: &'a [(u64, String, Value)], pc: u64, event: &str) -> Vec<&'a Value> {
    reports
        .iter()
        .filter(|(id, name, _)| *id == pc && name == event)
        .map(|(_, _, data)| data)
        .collect()
}

/// Whether connection `pc` has reported stats with a succeeded candidate pair.
fn connected_stats(reports: &[(u64, String, Value)], pc: u64) -> bool {
    reported(reports, pc, "stats").iter().any(|stats| {
        stats.as_array().is_some_and(|stats| {
            stats
                .iter()
                .any(|entry| entry["type"] == "candidate-pair" && entry["state"] == "succeeded")
        })
    })
}

#[tokio::test]
async fn the_rtc_hooks_record_a_loopback_call_from_its_first_line() {
    let pages = serve_pages().await;
    with_run(&pages, None, Duration::from_millis(200), async |run| {
        let cdp = run.armed.cdp();
        navigate(cdp, &run.page, &format!("{}/pages/rtc.html", pages.uri())).await?;
        let outcome = evaluate_until(cdp, &run.page.session, "window.outcome || null").await?;
        assert_eq!(outcome, "open");
        assert_eq!(evaluate(cdp, &run.page.session, "bindingVisible").await?, "undefined");
        assert_eq!(evaluate(cdp, &run.page.session, "constructorKept").await?, true);
        let file = run.recorder.events_file();
        entries_until(&file, |entries| connected_stats(&rtc_reports(entries), 1)).await?;
        assert_eq!(evaluate(cdp, &run.page.session, "closeAll()").await?, "closed");
        let entries = entries_until(&file, |entries| {
            let reports = rtc_reports(entries);
            !reported(&reports, 1, "close").is_empty() && !reported(&reports, 2, "close").is_empty()
        })
        .await?;
        check_rtc_reports(&entries, &run.page.session);
        Ok(())
    })
    .await
    .unwrap();
}

fn sdp_of(data: &Value) -> &str {
    data["sdp"].as_str().unwrap_or_default()
}

fn check_rtc_reports(entries: &[Value], page: &SessionId) {
    for entry in of_type(entries, "rtc") {
        assert_eq!(entry["session"], page.as_str(), "reported from the page's session");
        assert!(entry["context"].is_u64());
    }
    let reports = rtc_reports(entries);
    check_descriptions(&reports);
    check_candidates(&reports);
}

fn check_descriptions(reports: &[(u64, String, Value)]) {
    // both connections, seen from their constructor (the page's first line): only the constructor knows the
    // configuration
    assert_eq!(reported(reports, 1, "create"), [&json!({"bundlePolicy": "max-bundle"})]);
    assert_eq!(reported(reports, 2, "create"), [&Value::Null]);
    let offer = reported(reports, 1, "createOffer");
    assert_eq!(offer.len(), 1);
    assert_eq!(offer[0]["type"], "offer");
    assert!(sdp_of(offer[0]).contains("m=application"), "{}", offer[0]);
    let local = reported(reports, 1, "setLocalDescription");
    assert_eq!(sdp_of(local[0]), sdp_of(offer[0]));
    assert_eq!(
        sdp_of(reported(reports, 2, "setRemoteDescription")[0]),
        sdp_of(offer[0])
    );
    // the implicit setLocalDescription() reports the answer it made, and the offerer applies that answer
    let answer = reported(reports, 2, "setLocalDescription");
    assert_eq!(answer[0]["type"], "answer");
    assert!(sdp_of(answer[0]).contains("a=fingerprint:"), "{}", answer[0]);
    let applied = reported(reports, 1, "setRemoteDescription");
    assert_eq!(
        (applied[0]["type"].clone(), sdp_of(applied[0])),
        (json!("answer"), sdp_of(answer[0]))
    );
}

fn check_candidates(reports: &[(u64, String, Value)]) {
    // candidates gathered (ending with null) and handed over
    for pc in [1, 2] {
        let gathered = reported(reports, pc, "icecandidate");
        assert!(
            gathered.iter().any(|candidate| sdp_candidate(candidate)),
            "pc {pc}: {gathered:?}"
        );
        assert_eq!(
            gathered.last(),
            Some(&&Value::Null),
            "pc {pc}: gathering ends with null"
        );
        let given = reported(reports, pc, "addIceCandidate");
        assert!(
            given.iter().any(|candidate| sdp_candidate(candidate)),
            "pc {pc}: {given:?}"
        );
        assert!(reported(reports, pc, "connectionstatechange").contains(&&json!("connected")));
        assert!(connected_stats(reports, pc) || pc == 2 && !reported(reports, 2, "stats").is_empty());
    }
}

fn sdp_candidate(candidate: &Value) -> bool {
    candidate["candidate"]
        .as_str()
        .is_some_and(|line| line.starts_with("candidate:"))
}
