//! The lab browser: launch arguments per profile, the debugging endpoint, the pinned Chrome for Testing install and its
//! checksum, the CDP message codec, and (with Chrome installed) a new worker target held paused until we resume it.
//!
//! The Chrome test runs headless on a throwaway user-data-dir and a local page. It finds Chrome at
//! `$PROTO_LAB_CHROME`, else at the pinned build in the lab's chrome dir, found as `proto-lab chrome install` finds it
//! (lab.toml's `data_dir` when set); without either it says so on stderr and passes, unless
//! `PROTO_LAB_REQUIRE_CHROME=1`.

mod common;
#[path = "common/process.rs"]
mod process;

use std::ffi::OsString;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::StreamExt as _;
use proto_lab::browser::{BASE_ARGS, Browser, BrowserError, DevToolsEndpoint, Display, LaunchOptions, Media};
use proto_lab::cdp::{Cdp, CdpError, Event, Events, Incoming, RemoteError, SessionId, encode_command, parse_incoming};
use proto_lab::commands::chrome::{self, ChecksumError, ChromeBuild, InstallError, Installed};
use proto_lab::config::{Alias, LabPaths};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Helpers return errors; the tests unwrap them.
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
/// The same, for what a spawned task returns.
type SendResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn strings(args: &[OsString]) -> Vec<String> {
    args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect()
}

// --- launch arguments ---

#[test]
fn a_profile_launches_headless_on_an_automatic_port_in_its_own_user_data_dir() {
    let paths = LabPaths::new(PathBuf::from("/lab/data"));
    let options = LaunchOptions::for_profile(&paths, Alias::B);
    let args = strings(&options.args());

    assert_eq!(options.user_data_dir, PathBuf::from("/lab/data/profiles/b"));
    assert!(args.contains(&"--user-data-dir=/lab/data/profiles/b".to_owned()));
    assert!(args.contains(&"--remote-debugging-port=0".to_owned()));
    assert!(args.contains(&"--headless=new".to_owned()));
    assert!(!args.iter().any(|arg| arg.contains("fake")));
    for base in BASE_ARGS {
        assert!(args.contains(&(*base).to_owned()), "{base} missing");
    }
    assert_eq!(args.last().map(String::as_str), Some("about:blank"));
}

#[test]
fn every_lab_browser_keeps_off_the_keyring_and_google_background_traffic() {
    for flag in [
        "--password-store=basic",
        "--disable-background-networking",
        "--disable-sync",
        "--no-first-run",
    ] {
        assert!(BASE_ARGS.contains(&flag), "{flag} missing");
    }
}

#[test]
fn every_lab_browser_has_report_uploads_off_in_its_only_disable_features() {
    let options = LaunchOptions::new(PathBuf::from("/lab/p"));
    let disabled: Vec<String> = strings(&options.args())
        .into_iter()
        .filter(|arg| arg.starts_with("--disable-features="))
        .collect();
    // Chrome keeps only the last --disable-features, so there must be exactly one
    assert_eq!(disabled, ["--disable-features=Reporting,NetworkErrorLogging"]);
}

#[test]
fn headed_with_fake_media_still_leaves_the_port_to_chrome() {
    let options = LaunchOptions {
        user_data_dir: PathBuf::from("/tmp/profile with space"),
        display: Display::Headed,
        media: Media::Fake { audio: None },
        user_agent: None,
    };
    let args = strings(&options.args());

    assert!(args.contains(&"--user-data-dir=/tmp/profile with space".to_owned()));
    // Chrome writes DevToolsActivePort only for port 0
    assert!(args.contains(&"--remote-debugging-port=0".to_owned()));
    assert!(!args.iter().any(|arg| arg.starts_with("--headless")));
    assert!(args.contains(&"--use-fake-ui-for-media-stream".to_owned()));
    assert!(args.contains(&"--use-fake-device-for-media-stream".to_owned()));
    assert!(
        !args
            .iter()
            .any(|arg| arg.starts_with("--use-file-for-fake-audio-capture"))
    );
}

#[test]
fn fake_media_can_play_a_wav_file_as_the_microphone() {
    let mut options = LaunchOptions::new(PathBuf::from("/p"));
    options.media = Media::Fake {
        audio: Some(PathBuf::from("/lab/tone.wav")),
    };
    let args = strings(&options.args());

    assert!(args.contains(&"--use-fake-device-for-media-stream".to_owned()));
    assert!(args.contains(&"--use-file-for-fake-audio-capture=/lab/tone.wav".to_owned()));
}

#[test]
fn a_user_agent_replaces_the_browser_s_own() {
    let mut options = LaunchOptions::new(PathBuf::from("/p"));
    assert!(
        !strings(&options.args())
            .iter()
            .any(|arg| arg.starts_with("--user-agent"))
    );
    options.user_agent = Some("Mozilla/5.0 (X11; Linux x86_64) Chrome/1.0".to_owned());
    let args = strings(&options.args());

    assert!(args.contains(&"--user-agent=Mozilla/5.0 (X11; Linux x86_64) Chrome/1.0".to_owned()));
    // the starting page stays last
    assert_eq!(args.last().map(String::as_str), Some("about:blank"));
}

// --- DevToolsActivePort ---

#[test]
fn the_active_port_file_gives_the_browser_websocket() {
    let endpoint = DevToolsEndpoint::parse("40123\n/devtools/browser/0b7c-11\n").unwrap();

    assert_eq!(endpoint.port, 40123);
    assert_eq!(endpoint.ws_url(), "ws://127.0.0.1:40123/devtools/browser/0b7c-11");
}

#[test]
fn a_partial_or_malformed_active_port_file_is_refused() {
    for text in [
        "",
        "40123",
        "40123\n",
        "0\n/devtools/browser/x",
        "port\n/devtools/browser/x",
        "1\n/json",
    ] {
        assert!(
            matches!(DevToolsEndpoint::parse(text), Err(BrowserError::BadActivePort(_))),
            "{text:?} accepted"
        );
    }
}

#[tokio::test]
async fn a_missing_executable_is_reported() {
    let profile = tempfile::tempdir().unwrap();
    let missing = profile.path().join("no-chrome");
    let result = Browser::launch(
        &missing,
        &LaunchOptions::new(profile.path().join("p")),
        Duration::from_secs(5),
    )
    .await;

    assert!(matches!(result, Err(BrowserError::Spawn { .. })));
}

#[tokio::test]
async fn a_browser_that_exits_early_is_reported() {
    let profile = tempfile::tempdir().unwrap();
    let fake = profile.path().join("fake-chrome");
    std::fs::write(&fake, "#!/bin/sh\nexit 3\n").unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

    let result = Browser::launch(
        &fake,
        &LaunchOptions::new(profile.path().join("p")),
        Duration::from_secs(10),
    )
    .await;

    assert!(matches!(result, Err(BrowserError::Exited(status)) if status.code() == Some(3)));
}

#[tokio::test]
async fn a_stale_active_port_file_is_not_trusted() {
    let profile = tempfile::tempdir().unwrap();
    let dir = profile.path().join("p");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("DevToolsActivePort"), "40123\n/devtools/browser/stale\n").unwrap();
    let fake = profile.path().join("silent-chrome");
    std::fs::write(&fake, "#!/bin/sh\nexec sleep 30\n").unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

    let result = Browser::launch(&fake, &LaunchOptions::new(dir), Duration::from_millis(500)).await;

    assert!(matches!(result, Err(BrowserError::Timeout(_))));
}

/// A stand-in browser: starts a helper process (as Chrome starts its crash handler), writes `DevToolsActivePort`,
/// then waits.
const FAKE_BROWSER: &str = r#"#!/bin/sh
for arg; do case "$arg" in --user-data-dir=*) dir="${arg#--user-data-dir=}";; esac; done
sleep 300 &
echo $! > "$dir/helper.pid"
printf '40123\n/devtools/browser/fake\n' > "$dir/DevToolsActivePort"
exec sleep 300
"#;

async fn launch_fake(root: &Path) -> TestResult<(Browser, u32)> {
    let fake = root.join("fake-chrome");
    std::fs::write(&fake, FAKE_BROWSER)?;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755))?;
    let dir = root.join("p");
    let browser = Browser::launch(&fake, &LaunchOptions::new(dir.clone()), Duration::from_secs(10)).await?;
    let helper = std::fs::read_to_string(dir.join("helper.pid"))?.trim().parse()?;
    Ok((browser, helper))
}

#[tokio::test]
async fn the_endpoint_is_read_from_the_profile_and_dropping_the_browser_kills_its_whole_group() {
    let root = tempfile::tempdir().unwrap();
    let (browser, helper) = launch_fake(root.path()).await.unwrap();

    assert_eq!(
        browser.endpoint().ws_url(),
        "ws://127.0.0.1:40123/devtools/browser/fake"
    );
    assert!(process::running(helper));
    drop(browser);

    assert!(process::gone(helper).await, "the helper outlived the browser");
}

#[tokio::test]
async fn a_browser_that_does_not_exit_gets_sigterm_and_its_group_goes_with_it() {
    use std::os::unix::process::ExitStatusExt as _;

    let root = tempfile::tempdir().unwrap();
    let (browser, helper) = launch_fake(root.path()).await.unwrap();

    let status = browser.wait_or_kill(Duration::from_millis(100)).await.unwrap();

    assert_eq!(status.signal(), Some(15));
    assert!(process::gone(helper).await, "the helper outlived the browser");
}

// --- checksum and install ---

const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> TestResult<PathBuf> {
    let path = dir.join(name);
    std::fs::write(&path, bytes)?;
    Ok(path)
}

fn sha256_of(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    use std::fmt::Write as _;
    sha2::Sha256::digest(bytes).iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    })
}

#[test]
fn a_matching_checksum_passes_in_any_case() {
    let dir = tempfile::tempdir().unwrap();
    let empty = write_file(dir.path(), "empty", b"").unwrap();
    let abc = write_file(dir.path(), "abc", b"abc").unwrap();

    chrome::verify_sha256(&empty, EMPTY_SHA256).unwrap();
    chrome::verify_sha256(&abc, "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD").unwrap();
}

#[test]
fn a_mismatched_checksum_names_both_hashes() {
    let dir = tempfile::tempdir().unwrap();
    let abc = write_file(dir.path(), "abc", b"abc").unwrap();

    let error = chrome::verify_sha256(&abc, EMPTY_SHA256).unwrap_err();

    let ChecksumError::Mismatch { expected, actual } = &error else {
        panic!("{error}");
    };
    assert_eq!(expected, EMPTY_SHA256);
    assert_eq!(
        actual,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert!(error.to_string().contains(EMPTY_SHA256));
}

#[test]
fn a_malformed_expected_hash_or_missing_file_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let abc = write_file(dir.path(), "abc", b"abc").unwrap();

    for bad in ["", "abc", &"g".repeat(64), &"a".repeat(63)] {
        assert!(matches!(
            chrome::verify_sha256(&abc, bad),
            Err(ChecksumError::BadExpected(_))
        ));
    }
    assert!(matches!(
        chrome::verify_sha256(&dir.path().join("missing"), EMPTY_SHA256),
        Err(ChecksumError::Read { .. })
    ));
}

#[test]
fn the_pinned_build_is_a_chrome_for_testing_linux64_zip_no_older_than_the_profiles() {
    let build = ChromeBuild::pinned();
    let chrome_dir = Path::new("/lab/data/chrome");

    assert!(
        build
            .url
            .starts_with("https://storage.googleapis.com/chrome-for-testing-public/")
    );
    assert!(
        build
            .url
            .ends_with(&format!("/{}/linux64/chrome-linux64.zip", build.version))
    );
    assert_eq!(build.sha256.len(), 64);
    let major: u32 = build.version.split('.').next().unwrap().parse().unwrap();
    // the lab profiles were created by Chrome for Testing 151 (Playwright 1.62.1)
    assert!(major >= 151);
    assert_eq!(
        build.executable(chrome_dir),
        chrome_dir.join(&build.version).join("chrome-linux64/chrome")
    );
}

/// A zip shaped like Chrome for Testing's: `chrome-linux64/chrome` (executable) and one data file.
fn fake_chrome_zip(with_binary: bool) -> TestResult<Vec<u8>> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut bytes);
    let executable = zip::write::SimpleFileOptions::default().unix_permissions(0o755);
    let data = zip::write::SimpleFileOptions::default().unix_permissions(0o644);
    if with_binary {
        zip.start_file("chrome-linux64/chrome", executable)?;
        zip.write_all(b"#!/bin/sh\necho fake chrome\n")?;
    }
    zip.start_file("chrome-linux64/resources.pak", data)?;
    zip.write_all(b"data")?;
    zip.finish()?;
    Ok(bytes.into_inner())
}

async fn serve_zip(body: Vec<u8>, expected_downloads: u64) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/cft/chrome-linux64.zip"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
        .expect(expected_downloads)
        .mount(&server)
        .await;
    server
}

fn build_at(server: &MockServer, sha256: String) -> ChromeBuild {
    ChromeBuild {
        version: "999.0.0.1".to_owned(),
        url: format!("{}/cft/chrome-linux64.zip", server.uri()),
        sha256,
    }
}

fn entries(dir: &Path) -> TestResult<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        names.push(entry?.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    Ok(names)
}

#[tokio::test]
async fn install_downloads_verifies_and_unpacks_once() {
    let zip = fake_chrome_zip(true).unwrap();
    let server = serve_zip(zip.clone(), 1).await;
    let build = build_at(&server, sha256_of(&zip));
    let lab = tempfile::tempdir().unwrap();
    let chrome_dir = lab.path().join("chrome");
    let client = reqwest::Client::new();

    let first = chrome::install(&build, &chrome_dir, &client).await.unwrap();
    let second = chrome::install(&build, &chrome_dir, &client).await.unwrap();

    let executable = build.executable(&chrome_dir);
    assert_eq!(first, Installed::Fresh(executable.clone()));
    assert_eq!(second, Installed::Present(executable.clone()));
    let mode = std::fs::metadata(&executable).unwrap().permissions().mode();
    assert_eq!(mode & 0o111, 0o111, "the browser binary is executable");
    // nothing but the finished install is left behind
    assert_eq!(entries(&chrome_dir).unwrap(), vec!["999.0.0.1".to_owned()]);
}

#[tokio::test]
async fn a_download_with_the_wrong_checksum_installs_nothing() {
    let server = serve_zip(fake_chrome_zip(true).unwrap(), 1).await;
    let build = build_at(&server, EMPTY_SHA256.to_owned());
    let lab = tempfile::tempdir().unwrap();
    let chrome_dir = lab.path().join("chrome");

    let error = chrome::install(&build, &chrome_dir, &reqwest::Client::new())
        .await
        .unwrap_err();

    assert!(
        matches!(error, InstallError::Checksum(ChecksumError::Mismatch { .. })),
        "{error}"
    );
    assert!(
        entries(&chrome_dir).unwrap().is_empty(),
        "left behind: {:?}",
        entries(&chrome_dir).unwrap()
    );
}

#[tokio::test]
async fn a_zip_without_the_browser_binary_installs_nothing() {
    let zip = fake_chrome_zip(false).unwrap();
    let server = serve_zip(zip.clone(), 1).await;
    let build = build_at(&server, sha256_of(&zip));
    let lab = tempfile::tempdir().unwrap();
    let chrome_dir = lab.path().join("chrome");

    let error = chrome::install(&build, &chrome_dir, &reqwest::Client::new())
        .await
        .unwrap_err();

    assert!(matches!(error, InstallError::MissingExecutable(_)), "{error}");
    assert!(
        entries(&chrome_dir).unwrap().is_empty(),
        "left behind: {:?}",
        entries(&chrome_dir).unwrap()
    );
}

#[tokio::test]
async fn an_http_error_installs_nothing() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let build = build_at(&server, EMPTY_SHA256.to_owned());
    let lab = tempfile::tempdir().unwrap();
    let chrome_dir = lab.path().join("chrome");

    let error = chrome::install(&build, &chrome_dir, &reqwest::Client::new())
        .await
        .unwrap_err();

    assert!(matches!(error, InstallError::Download(_)), "{error}");
    assert!(entries(&chrome_dir).unwrap().is_empty());
}

#[tokio::test]
async fn an_unfinished_install_is_replaced() {
    let zip = fake_chrome_zip(true).unwrap();
    let server = serve_zip(zip.clone(), 1).await;
    let build = build_at(&server, sha256_of(&zip));
    let lab = tempfile::tempdir().unwrap();
    let chrome_dir = lab.path().join("chrome");
    // an earlier run died after unpacking, before the marker
    std::fs::create_dir_all(build.install_dir(&chrome_dir).join("chrome-linux64")).unwrap();
    std::fs::write(build.executable(&chrome_dir), "truncated").unwrap();

    let installed = chrome::install(&build, &chrome_dir, &reqwest::Client::new())
        .await
        .unwrap();

    assert!(matches!(installed, Installed::Fresh(_)));
    let binary = std::fs::read_to_string(build.executable(&chrome_dir)).unwrap();
    assert!(binary.contains("fake chrome"));
}

// --- CDP codec ---

#[test]
fn a_command_carries_its_session_and_empty_params_become_an_object() {
    let session = SessionId::new("S1");
    let browser: Value = serde_json::from_str(&encode_command(1, None, "Target.getTargets", Value::Null)).unwrap();
    let target: Value = serde_json::from_str(&encode_command(
        2,
        Some(&session),
        "Fetch.enable",
        json!({"patterns": []}),
    ))
    .unwrap();

    assert_eq!(browser, json!({"id": 1, "method": "Target.getTargets", "params": {}}));
    assert_eq!(
        target,
        json!({"id": 2, "method": "Fetch.enable", "params": {"patterns": []}, "sessionId": "S1"})
    );
}

#[test]
fn responses_errors_and_session_events_are_decoded() {
    let ok = parse_incoming(r#"{"id":3,"result":{"targetId":"T"}}"#).unwrap();
    let failed = parse_incoming(r#"{"id":4,"error":{"code":-32601,"message":"'X.y' wasn't found"}}"#).unwrap();
    let event =
        parse_incoming(r#"{"method":"Target.attachedToTarget","params":{"waitingForDebugger":true},"sessionId":"S2"}"#)
            .unwrap();
    let browser_event = parse_incoming(r#"{"method":"Target.targetCreated","params":{}}"#).unwrap();

    assert_eq!(
        ok,
        Incoming::Response {
            id: 3,
            outcome: Ok(json!({"targetId": "T"}))
        }
    );
    assert_eq!(
        failed,
        Incoming::Response {
            id: 4,
            outcome: Err(RemoteError {
                code: -32601,
                message: "'X.y' wasn't found".to_owned()
            })
        }
    );
    assert_eq!(
        event,
        Incoming::Event(Event {
            session_id: Some(SessionId::new("S2")),
            method: "Target.attachedToTarget".to_owned(),
            params: json!({"waitingForDebugger": true}),
        })
    );
    assert!(matches!(browser_event, Incoming::Event(Event { session_id: None, .. })));
}

#[test]
fn a_message_that_is_neither_response_nor_event_is_malformed() {
    for text in ["", "[]", "{}", r#"{"params":{}}"#, r#"{"id":"x"}"#] {
        assert!(
            matches!(parse_incoming(text), Err(CdpError::Malformed(_))),
            "{text:?} accepted"
        );
    }
}

#[tokio::test]
async fn connecting_to_nothing_fails() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let result = Cdp::connect(&format!("ws://127.0.0.1:{port}/devtools/browser/x")).await;

    assert!(matches!(result, Err(CdpError::Connect(_))));
}

/// A one-client websocket server that reads until the client goes away; true when the client sent a close frame.
async fn close_watching_server() -> TestResult<(String, tokio::task::JoinHandle<SendResult<bool>>)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("ws://{}/devtools/browser/x", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = tokio_tungstenite::accept_async(stream).await?;
        let mut closed = false;
        // reading on after the close frame sends the server's answer, which ends the client's reader
        while let Some(message) = socket.next().await {
            closed |= message?.is_close();
        }
        Ok(closed)
    });
    Ok((url, server))
}

#[tokio::test]
async fn dropping_every_handle_closes_the_connection() {
    let (url, server) = close_watching_server().await.unwrap();
    let (cdp, events) = Cdp::connect(&url).await.unwrap();
    let session = cdp.session(SessionId::new("S1"));

    drop((cdp, session, events));

    let closed = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("the connection stayed open after every handle was dropped");
    assert!(closed.unwrap().unwrap(), "no close frame");
}

#[tokio::test]
async fn events_end_once_the_last_command_handle_is_dropped() {
    let (url, server) = close_watching_server().await.unwrap();
    let (cdp, mut events) = Cdp::connect(&url).await.unwrap();

    drop(cdp);

    let end = tokio::time::timeout(Duration::from_secs(5), events.recv()).await;
    assert!(matches!(end, Ok(None)), "the events went on");
    assert!(server.await.unwrap().unwrap(), "no close frame");
}

// --- a real browser: a new worker target is held until we resume it ---

const PAGE: &str = r"<!doctype html>
<title>worker</title>
<script>
  window.workerSaid = [];
  const worker = new Worker('/worker.js');
  worker.onmessage = (event) => { window.workerSaid.push(event.data); };
</script>";

// The first line is something Fetch can't hold: if the worker ran before we resumed it, the page would hear 'started'.
const WORKER: &str = r"postMessage('started');
fetch('/from-worker')
  .then((response) => response.text())
  .then((text) => postMessage('ran:' + text), () => postMessage('fetch failed'));";

const HOLD: Duration = Duration::from_millis(700);

const STEP: Duration = Duration::from_secs(15);

fn is_worker_request(event: &Event) -> bool {
    event.method == "Fetch.requestPaused"
        && event.params["request"]["url"]
            .as_str()
            .is_some_and(|url| url.ends_with("/from-worker"))
}

/// Takes every event for `window`, letting paused requests through, and fails if the worker's request turns up: a held
/// worker sends nothing.
async fn hold_for(cdp: &Cdp, events: &mut Events, window: Duration) -> TestResult {
    let deadline = tokio::time::Instant::now() + window;
    loop {
        let Ok(next) = tokio::time::timeout_at(deadline, events.recv()).await else {
            return Ok(());
        };
        let event = next.ok_or("the connection closed while the worker was held")?;
        if is_worker_request(&event) {
            return Err("the worker's request went out before runIfWaitingForDebugger".into());
        }
        if event.method == "Fetch.requestPaused" {
            let request = json!({"requestId": event.params["requestId"]});
            cdp.call(event.session_id.as_ref(), "Fetch.continueRequest", request)
                .await?;
        }
    }
}

/// Waits for an event, letting through every paused request it passes over (Fetch is on for the page).
async fn next_event(cdp: &Cdp, events: &mut Events, what: &str, wanted: impl Fn(&Event) -> bool) -> TestResult<Event> {
    let wait = async {
        loop {
            let Some(event) = events.recv().await else {
                return Err(format!("the connection closed while waiting for {what}").into());
            };
            if wanted(&event) {
                return Ok(event);
            }
            if event.method == "Fetch.requestPaused" {
                let request = json!({"requestId": event.params["requestId"]});
                cdp.call(event.session_id.as_ref(), "Fetch.continueRequest", request)
                    .await?;
            }
        }
    };
    tokio::time::timeout(STEP, wait)
        .await
        .map_err(|_| format!("no {what} within {STEP:?}"))?
}

async fn page_value(cdp: &Cdp, page: &SessionId, expression: &str) -> TestResult<Value> {
    let params = json!({"expression": expression, "returnByValue": true});
    let result = cdp.call(Some(page), "Runtime.evaluate", params).await?;
    Ok(result["result"]["value"].clone())
}

fn session_of(value: &Value) -> TestResult<SessionId> {
    let id = value["sessionId"].as_str().ok_or("no sessionId")?;
    Ok(SessionId::new(id))
}

async fn served(server: &MockServer, wanted: &str) -> usize {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|request| request.url.path() == wanted)
        .count()
}

async fn serve_worker_page() -> MockServer {
    let server = MockServer::start().await;
    for (route, body, content_type) in [
        ("/worker.html", PAGE, "text/html"),
        ("/worker.js", WORKER, "text/javascript"),
        ("/from-worker", "hello", "text/plain"),
    ] {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, content_type))
            .mount(&server)
            .await;
    }
    server
}

/// Opens a blank page target in flat mode, with auto-attach holding every new child target.
async fn open_held_page(cdp: &Cdp) -> TestResult<SessionId> {
    let target = cdp
        .call(None, "Target.createTarget", json!({"url": "about:blank"}))
        .await?;
    let attach = json!({"targetId": target["targetId"], "flatten": true});
    let page = session_of(&cdp.call(None, "Target.attachToTarget", attach).await?)?;
    let hold = json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true});
    cdp.call(Some(&page), "Target.setAutoAttach", hold).await?;
    Ok(page)
}

/// Arms the page (Fetch on every request), loads it, and returns the session of the worker it starts, still held.
async fn load_with_held_worker(
    cdp: &Cdp,
    events: &mut Events,
    page: &SessionId,
    server: &MockServer,
) -> TestResult<SessionId> {
    // armed as the fence will be: Fetch on the page before anything loads
    let intercept_all = json!({"patterns": [{"urlPattern": "*"}]});
    cdp.call(Some(page), "Fetch.enable", intercept_all).await?;
    // Page.navigate answers once the navigation commits, and the page request waits on Fetch: drive events meanwhile
    let navigate = tokio::spawn({
        let (cdp, page, url) = (cdp.clone(), page.clone(), format!("{}/worker.html", server.uri()));
        async move { cdp.call(Some(&page), "Page.navigate", json!({ "url": url })).await }
    });
    let attached = next_event(cdp, events, "the worker's attach", |event| {
        event.method == "Target.attachedToTarget"
            && event.session_id.as_ref() == Some(page)
            && event.params["targetInfo"]["type"] == "worker"
    })
    .await?;
    navigate.await??;
    assert_eq!(attached.params["waitingForDebugger"], json!(true));
    session_of(&attached.params)
}

/// Resumes the held worker; its request pauses on the page's session and goes on only when we continue it.
async fn release_worker(
    cdp: &Cdp,
    events: &mut Events,
    page: &SessionId,
    worker: &SessionId,
    server: &MockServer,
) -> TestResult {
    // a dedicated worker has no Fetch domain of its own: the page's Fetch covers it
    let no_fetch = cdp
        .call(Some(worker), "Fetch.enable", json!({"patterns": [{"urlPattern": "*"}]}))
        .await;
    assert!(matches!(no_fetch, Err(CdpError::Remote { .. })), "{no_fetch:?}");
    cdp.call(Some(worker), "Runtime.runIfWaitingForDebugger", Value::Null)
        .await?;
    let paused = next_event(cdp, events, "the worker's paused request", is_worker_request).await?;
    assert_eq!(paused.session_id.as_ref(), Some(page), "paused on the page's session");
    // the worker runs now, and its request is held by Fetch alone
    assert_eq!(worker_messages(cdp, page, 1).await?, json!(["started"]));
    assert_eq!(served(server, "/from-worker").await, 0, "held by Fetch");
    let request = json!({"requestId": paused.params["requestId"]});
    cdp.call(Some(page), "Fetch.continueRequest", request).await?;
    Ok(())
}

/// What the worker posted to the page, once it has posted at least `count` messages.
async fn worker_messages(cdp: &Cdp, page: &SessionId, count: usize) -> TestResult<Value> {
    let poll = async {
        loop {
            let said = page_value(cdp, page, "window.workerSaid").await?;
            if said.as_array().is_some_and(|messages| messages.len() >= count) {
                return Ok(said);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    tokio::time::timeout(STEP, poll)
        .await
        .map_err(|_| format!("the worker posted under {count} messages within {STEP:?}"))?
}

async fn held_worker(browser: &Browser, server: &MockServer) -> TestResult {
    let (cdp, mut events) = Cdp::connect(&browser.endpoint().ws_url()).await?;
    let page = open_held_page(&cdp).await?;
    let worker = load_with_held_worker(&cdp, &mut events, &page, server).await?;

    // held: the page is up, the worker's code hasn't run, not even its first line
    hold_for(&cdp, &mut events, HOLD).await?;
    assert_eq!(page_value(&cdp, &page, "window.workerSaid").await?, json!([]));
    assert_eq!(served(server, "/from-worker").await, 0);

    release_worker(&cdp, &mut events, &page, &worker, server).await?;
    assert_eq!(worker_messages(&cdp, &page, 2).await?, json!(["started", "ran:hello"]));
    assert_eq!(served(server, "/from-worker").await, 1);

    // closing is best effort: the connection may drop before the reply
    let _ = cdp.call(None, "Browser.close", Value::Null).await;
    Ok(())
}

#[tokio::test]
async fn a_new_worker_target_is_held_until_run_if_waiting_for_debugger() {
    let Some(chrome) = common::chrome_or_skip() else {
        return;
    };
    let profile = tempfile::tempdir().unwrap();
    let server = serve_worker_page().await;
    let browser = Browser::launch(&chrome, &LaunchOptions::new(profile.path().join("p")), STEP)
        .await
        .unwrap();

    let outcome = tokio::time::timeout(Duration::from_secs(90), held_worker(&browser, &server)).await;
    browser.wait_or_kill(Duration::from_secs(10)).await.unwrap();

    outcome.expect("the held-worker run took over 90 s").unwrap();
}
