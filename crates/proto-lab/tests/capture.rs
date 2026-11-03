//! The capture recorder: where captures go and how entries are written (offline), the WebRTC hooks' place in the
//! setup, and, with Chrome installed, a recorded run on local pages and a local echo server (its
//! `Network` events, response bodies, frames and capture holds), and the WebRTC hooks on a loopback call between
//! two `RTCPeerConnection`s in one page, their audio tap reading the tones such a call receives.
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
use proto_lab::arming::{ArmedBrowser, ArmedTarget, Step, TargetKind};
use proto_lab::browser::{Browser, LaunchOptions, Media};
use proto_lab::capture::{
    Capture, CaptureError, EVENTS_FILE, RTC_BINDING, Recorder, RtcHooks, TAP_INTERVAL, TAP_TONE_HZ, TAP_WINDOW,
    capture_dir_name,
};
use proto_lab::capture_hold::{CaptureHold, Hold};
use proto_lab::cdp::{Cdp, SessionId};
use proto_lab::frame_hold::FrameHold;
use proto_lab::scenario::echo_call::tone_wav;
use serde_json::{Value, json};
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
    assert!(
        hooks
            .source()
            .contains(&format!("TAP_INTERVAL_MS = {};", TAP_INTERVAL.as_millis()))
    );
    assert!(hooks.source().contains(&format!("TAP_WINDOW = {TAP_WINDOW};")));
    assert!(hooks.source().contains(&format!("TONE_HZ = {TAP_TONE_HZ};")));
    assert!(!hooks.source().contains("{{"), "every placeholder filled");
    assert!(RtcHooks::default().source().contains("STATS_INTERVAL_MS = 1000;"));
}

#[test]
fn pages_and_iframes_get_the_hooks_before_they_run() {
    for kind in [TargetKind::Page, TargetKind::Iframe] {
        let steps = kind.steps();
        let position = |wanted| steps.iter().position(|step| *step == wanted).unwrap();
        assert_eq!(position(Step::BypassServiceWorker), position(Step::Network) + 1);
        assert!(position(Step::BypassServiceWorker) < position(Step::Resume));
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

const PAGES: [(&str, &str, &str); 7] = [
    ("blank.html", include_str!("fixtures/pages/blank.html"), "text/html"),
    ("capture.html", include_str!("fixtures/pages/capture.html"), "text/html"),
    ("rtc.html", include_str!("fixtures/pages/rtc.html"), "text/html"),
    (
        "rtc-audio.html",
        include_str!("fixtures/pages/rtc-audio.html"),
        "text/html",
    ),
    ("rtc-mic.html", include_str!("fixtures/pages/rtc-mic.html"), "text/html"),
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

/// One recorded run, armed on a throwaway profile under the lab rule, which lets the local pages' requests and frames
/// through: a `POST` to the pages' `/sink` and a frame holding `deny:` are force-denied.
struct Run {
    armed: ArmedBrowser,
    recorder: Arc<Recorder>,
    page: ArmedTarget,
    _dirs: tempfile::TempDir,
}

/// How a recorded run records: the WebRTC hooks' stats interval, and whether request bodies are kept.
#[derive(Clone)]
struct Recording {
    stats: Duration,
    request_bodies: bool,
    /// The browser's media: with fake devices it is the browser a call scenario runs (audio allowed to start without a
    /// click), the microphone playing the file it names.
    media: Media,
}

impl Recording {
    fn with_bodies(stats: Duration) -> Self {
        Self {
            stats,
            request_bodies: true,
            media: Media::System,
        }
    }
}

async fn start_run(pages: &MockServer, echo: Option<&str>, recording: Recording) -> TestResult<Option<Run>> {
    let Some(chrome) = common::chrome_or_skip() else {
        return Ok(None);
    };
    let dirs = tempfile::tempdir()?;
    let mut launch = LaunchOptions::new(dirs.path().join("profile"));
    launch.media = recording.media.clone();
    let browser = Browser::launch(&chrome, &launch, STEP).await?;

    let http = CaptureHold::new();
    http.hold(Hold::new("POST", &format!("^{}/sink$", regex::escape(&pages.uri())))?);

    let recorder = Arc::new(Recorder::create(&dirs.path().join("captures"), "test")?);
    let capture = Capture::new(Arc::clone(&recorder), RtcHooks::new(recording.stats));
    let capture = if recording.request_bodies {
        capture
    } else {
        capture.without_request_bodies()
    };
    let armed = ArmedBrowser::arm_capturing(browser, http, capture).await?;
    let page = armed.open_page(STEP).await?;
    if let Some(origin) = echo {
        armed
            .hold_frames(FrameHold::new(&format!("^{}/", regex::escape(origin)))?.with_text_containing("deny:"))
            .await?;
    }
    Ok(Some(Run {
        armed,
        recorder,
        page,
        _dirs: dirs,
    }))
}

/// Runs `body` on a recorded run, then closes the browser whatever `body` returned. Without Chrome it does nothing.
async fn with_run<F>(pages: &MockServer, echo: Option<&str>, recording: Recording, body: F) -> TestResult
where
    F: AsyncFnOnce(&Run) -> TestResult,
{
    let Some(run) = start_run(pages, echo, recording).await? else {
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
async fn a_recorded_run_keeps_network_frames_bodies_and_selective_holds() {
    let pages = serve_pages().await;
    let (echo, received) = echo_server().await.unwrap();
    with_run(
        &pages,
        Some(&echo),
        Recording::with_bodies(Duration::from_secs(1)),
        async |run| {
            let cdp = run.armed.cdp();
            let url = format!("{}/pages/capture.html?ws={echo}", pages.uri());
            navigate(cdp, &run.page, &url).await?;
            let expression = "results.get && results.post && results.echo.length === 2 ? results : null";
            let results = evaluate_until(cdp, &run.page.session, expression).await?;
            assert_eq!(results["get"], "lab");
            assert_eq!(
                results["post"], "error TypeError",
                "the page sees the hold as a network error"
            );
            assert_eq!(results["echo"], json!(["allow:1", "allow:2"]));
            let file = run.recorder.events_file();
            let entries = entries_until(&file, |entries| has_body(entries, "/pages/data.json")).await?;
            check_recorded_run(&entries)?;
            assert_eq!(
                *received.lock().unwrap_or_else(PoisonError::into_inner),
                ["allow:1", "allow:2"]
            );
            assert_eq!(run.armed.held_requests().len(), 1);
            assert!(run.armed.status().failures().is_empty());
            Ok(())
        },
    )
    .await
    .unwrap();
    let posted = pages.received_requests().await.unwrap_or_default();
    assert!(
        posted.iter().all(|request| request.method.as_str() == "GET"),
        "a held POST got out"
    );
}

/// Posts from the page: a string and a `Blob` to `/own`, which reaches nobody (Chrome leaves a `Blob` out of its
/// request event, so the capture would fetch it), and a short one to `/sink`, which the capture holds.
const POSTS: &str = r"(async () => {
  const post = (path, body) => fetch(path, { method: 'POST', body }).then((r) => r.status, (e) => 'error ' + e.name);
  return {
    short: await post('/own', 'lab-secret-short'),
    blob: await post('/own', new Blob(['lab-secret-blob'.repeat(20000)])),
    denied: await post('/sink', 'lab-secret-denied'),
  };
})()";

#[tokio::test]
async fn a_capture_without_request_bodies_keeps_the_requests_and_none_of_their_bodies() {
    let pages = serve_pages().await;
    let recording = Recording {
        stats: Duration::from_secs(1),
        request_bodies: false,
        media: Media::System,
    };
    with_run(&pages, None, recording, async |run| {
        let cdp = run.armed.cdp();
        navigate(cdp, &run.page, &format!("{}/pages/blank.html", pages.uri())).await?;
        let results = evaluate(cdp, &run.page.session, POSTS).await?;
        assert_eq!(results, json!({"short": 200, "blob": 200, "denied": "error TypeError"}));
        let file = run.recorder.events_file();
        // both posts to `/own` have their answers, so whatever their requests would have fetched was asked for before
        let entries = entries_until(&file, |entries| {
            let answered = cdp_events(entries, "Network.responseReceived");
            let posts = posts_to(entries, "/own");
            let answered = posts
                .iter()
                .filter(|id| answered.iter().any(|entry| entry["params"]["requestId"] == id.as_str()))
                .count();
            answered == 2 && of_type(entries, "held_request").next().is_some()
        })
        .await?;
        let posts: Vec<&Value> = cdp_events(&entries, "Network.requestWillBeSent")
            .into_iter()
            .filter(|entry| entry["params"]["request"]["method"] == "POST")
            .collect();
        assert_eq!(posts.len(), 3, "every post is recorded: {posts:?}");
        assert!(
            posts
                .iter()
                .all(|entry| entry["params"]["request"]["hasPostData"] == true),
            "and says it had a body"
        );
        let denied: Vec<&Value> = of_type(&entries, "held_request").collect();
        assert_eq!(denied.len(), 1, "{denied:?}");
        assert_eq!(denied[0]["request"]["method"], "POST");
        assert_eq!(of_type(&entries, "request_body").count(), 0);
        // nothing of any body, in any form, anywhere in the file
        let raw = std::fs::read_to_string(&file)?;
        for needle in ["lab-secret", "\"postData\"", "postDataEntries", "bGFiLXNlY3JldC"] {
            assert!(!raw.contains(needle), "{needle} is in the capture");
        }
        Ok(())
    })
    .await
    .unwrap();
}

/// The `Network` request ids of the `POST`s to `path`.
fn posts_to(entries: &[Value], path: &str) -> Vec<String> {
    cdp_events(entries, "Network.requestWillBeSent")
        .into_iter()
        .filter(|entry| {
            let request = &entry["params"]["request"];
            request["method"] == "POST" && request["url"].as_str().is_some_and(|url| url.ends_with(path))
        })
        .filter_map(|entry| entry["params"]["requestId"].as_str().map(str::to_owned))
        .collect()
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
    check_http_holds(entries);
    check_recorded_frames(entries);
    Ok(())
}

fn check_http_holds(entries: &[Value]) {
    let held: Vec<_> = of_type(entries, "held_request").collect();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0]["method"], "POST");
    assert!(held[0]["url"].as_str().is_some_and(|url| url.ends_with("/sink")));
    assert_eq!(held[0]["request"]["postData"], "lab-post");
    assert!(
        of_type(entries, "verdict").next().is_none(),
        "capture has no authorization verdicts"
    );
}
fn check_recorded_frames(entries: &[Value]) {
    let held: Vec<_> = of_type(entries, "held_frame").collect();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0]["payload"]["data"], "deny:1");
    let sent: Vec<_> = cdp_events(entries, "Network.webSocketFrameSent")
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
    with_run(
        &pages,
        None,
        Recording::with_bodies(Duration::from_millis(200)),
        async |run| {
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
        },
    )
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
    check_data_channel(&reports);
}

fn check_data_channel(reports: &[(u64, String, Value)]) {
    // the offerer's channel, made by it and given to the answerer; each message reported as sent by one side and as
    // received by the other: text as it is, binary as hex with its length
    assert_eq!(
        reported(reports, 1, "createDataChannel"),
        [&json!({"label": "lab", "id": null})]
    );
    assert_eq!(reported(reports, 2, "datachannel").len(), 1);
    assert_eq!(reported(reports, 2, "datachannel")[0]["label"], "lab");
    let text = json!({"text": "lab text"});
    let binary = json!({"length": 4, "hex": "0102feff"});
    let sent: Vec<&Value> = reported(reports, 1, "dc:send")
        .iter()
        .map(|data| &data["data"])
        .collect();
    assert_eq!(sent, [&text, &binary]);
    let received: Vec<&Value> = reported(reports, 2, "dc:message")
        .iter()
        .map(|data| &data["data"])
        .collect();
    assert_eq!(received, [&text, &binary]);
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

/// The audio tap's measurements of received track `track`: (`rms`, `tone`).
fn tap_readings(entries: &[Value], track: &str) -> Vec<(f64, f64)> {
    rtc_reports(entries)
        .into_iter()
        .filter(|(_, event, data)| event == "audio" && data["track"] == track)
        .map(|(_, _, data)| {
            (
                data["rms"].as_f64().unwrap_or(0.0),
                data["tone"].as_f64().unwrap_or(0.0),
            )
        })
        .collect()
}

/// Whether the last `count` readings of `track` all hold `wanted`.
fn steady(entries: &[Value], track: &str, count: usize, wanted: impl Fn(f64, f64) -> bool) -> bool {
    let readings = tap_readings(entries, track);
    readings.len() >= count
        && readings[readings.len() - count..]
            .iter()
            .all(|(rms, tone)| wanted(*rms, *tone))
}

/// How many of `tracks` the tap has reported ended.
fn ended_taps(entries: &[Value], tracks: &[&str]) -> usize {
    rtc_reports(entries)
        .into_iter()
        .filter(|(_, event, data)| event == "audio:ended" && tracks.iter().any(|track| data["track"] == *track))
        .count()
}

#[tokio::test]
async fn the_audio_tap_reads_each_received_tone_in_the_page() {
    let pages = serve_pages().await;
    let recording = Recording {
        stats: Duration::from_millis(500),
        request_bodies: true,
        media: Media::Fake { audio: None },
    };
    with_run(&pages, None, recording, async |run| {
        let cdp = run.armed.cdp();
        navigate(cdp, &run.page, &format!("{}/pages/rtc-audio.html", pages.uri())).await?;
        assert_eq!(
            evaluate_until(cdp, &run.page.session, "window.outcome || null").await?,
            "negotiated"
        );
        let tracks: Value = serde_json::from_str(
            evaluate_until(cdp, &run.page.session, "window.receivedTracks()")
                .await?
                .as_str()
                .ok_or("no received tracks")?,
        )?;
        let tone = tracks["440"].as_str().ok_or("no 440 Hz track")?.to_owned();
        let other = tracks["1000"].as_str().ok_or("no 1000 Hz track")?.to_owned();
        let file = run.recorder.events_file();
        // the 440 Hz track is nearly all tone, the 1000 Hz one nearly none of it, both clearly audible
        let entries = entries_until(&file, |entries| {
            steady(entries, &tone, 3, |rms, share| rms > 0.1 && share > 0.9)
                && steady(entries, &other, 3, |rms, share| rms > 0.1 && share < 0.05)
        })
        .await?;
        for entry in of_type(&entries, "rtc").filter(|entry| entry["payload"]["event"] == "audio") {
            let data = &entry["payload"]["data"];
            assert_eq!(entry["session"], run.page.session.as_str());
            assert!(data["rate"].as_f64().is_some_and(|rate| rate >= 8_000.0), "{data}");
            assert!(data["state"].is_string(), "{data}");
        }
        assert!(
            rtc_reports(&entries).iter().all(|(_, event, _)| event != "audio:error"),
            "the tap ran into nothing"
        );
        assert_eq!(evaluate(cdp, &run.page.session, "closeAll()").await?, "closed");
        // a closed connection ends its tracks, and the tap stops with them
        entries_until(&file, |entries| ended_taps(entries, &[&tone, &other]) == 2).await?;
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_call_scenario_s_fake_microphone_sends_its_tone_file() {
    let pages = serve_pages().await;
    let media = tempfile::tempdir().unwrap();
    let tone = media.path().join("tone.wav");
    std::fs::write(&tone, tone_wav(TAP_TONE_HZ, 2, 48_000)).unwrap();
    let recording = Recording {
        stats: Duration::from_millis(500),
        request_bodies: true,
        media: Media::Fake { audio: Some(tone) },
    };
    with_run(&pages, None, recording, async |run| {
        let cdp = run.armed.cdp();
        navigate(cdp, &run.page, &format!("{}/pages/rtc-mic.html", pages.uri())).await?;
        assert_eq!(
            evaluate_until(cdp, &run.page.session, "window.outcome || null").await?,
            "negotiated"
        );
        let track = evaluate_until(cdp, &run.page.session, "window.receivedTrack()").await?;
        let track = track.as_str().ok_or("no received track")?.to_owned();
        // what the microphone sent comes back as the tone
        entries_until(&run.recorder.events_file(), |entries| {
            steady(entries, &track, 3, |rms, share| rms > 0.01 && share > 0.5)
        })
        .await?;
        assert_eq!(evaluate(cdp, &run.page.session, "closeAll()").await?, "closed");
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn page_fetches_bypass_an_active_service_worker_and_keep_their_network_ids() {
    let pages = serve_pages().await;
    let worker = r"self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (event) => event.waitUntil(self.clients.claim()));
self.addEventListener('fetch', (event) => {
  if (new URL(event.request.url).pathname.endsWith('/data.json')) {
    event.respondWith(new Response('intercepted by worker'));
  }
});";
    Mock::given(method("GET"))
        .and(path("/pages/sw.js"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(worker, "text/javascript"))
        .mount(&pages)
        .await;
    with_run(&pages, None, Recording::with_bodies(Duration::from_secs(1)), async |run| {
        let cdp = run.armed.cdp();
        navigate(cdp, &run.page, &format!("{}/pages/blank.html", pages.uri())).await?;
        let controlled = r"(async () => {
            await navigator.serviceWorker.register('/pages/sw.js');
            await navigator.serviceWorker.ready;
            if (!navigator.serviceWorker.controller) {
                await new Promise((resolve) => navigator.serviceWorker.addEventListener('controllerchange', resolve, { once: true }));
            }
            return !!navigator.serviceWorker.controller;
        })()";
        assert_eq!(tokio::time::timeout(STEP, evaluate(cdp, &run.page.session, controlled)).await??, true);
        assert_eq!(evaluate(cdp, &run.page.session, "fetch('/pages/data.json').then(r => r.json()).then(r => r.hello)").await?, "lab");
        let entries = entries_until(&run.recorder.events_file(), |entries| has_body(entries, "/pages/data.json")).await?;
        let id = request_id(&entries, "/pages/data.json").ok_or("page fetch has no Network id")?;
        assert!(cdp_events(&entries, "Network.responseReceived").iter().any(|entry| entry["params"]["requestId"] == id && entry["params"]["response"]["fromServiceWorker"] == false));
        // The same active worker intercepts this request when bypass is disabled.
        cdp.call(Some(&run.page.session), "Network.setBypassServiceWorker", json!({"bypass":false})).await?;
        assert_eq!(evaluate(cdp, &run.page.session, "fetch('/pages/data.json').then(r => r.text())").await?, "intercepted by worker");
        Ok(())
    }).await.unwrap();
}
