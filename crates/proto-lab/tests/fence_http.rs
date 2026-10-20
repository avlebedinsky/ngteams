//! The browser fence's HTTP layer: the classifier, the decision on one paused request, the arming of the browser and
//! of every target (against a scripted fake browser), and, with Chrome installed, enforcement on local pages: a denied
//! request never reaches the mock server and the page sees a network error, wherever it comes from.
//!
//! The Chrome tests run headless on throwaway user-data-dirs against a local mock server; see `common` for how Chrome
//! is found.

mod common;
#[path = "common/process.rs"]
mod process;

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use proto_lab::arming::{ArmOptions, ArmedBrowser, ArmedTarget, Step, TEAMS_ORIGINS, TargetKind};
use proto_lab::browser::{Browser, BrowserError, LaunchOptions};
use proto_lab::cdp::{Cdp, SessionId};
use proto_lab::classify::{Classification, Classifier, OwnKind, OwnRule, classify, is_mutating};
use proto_lab::config::Alias;
use proto_lab::fence_http::{DenyCause, HttpFence, MalformedPause, PausedRequest, Verdict};
use proto_lab::fence_ws::{FrameClassifier, GuardScript, WsFence};
use proto_lab::scenario::{AccountRun, DenyPolicy, ScenarioName, ScenarioReport};
use proto_lab::session::FenceReport;
use serde_json::{Value, json};
use teams_fence::{
    DenyReason, Fence, FenceLog, Membership, MembershipFuture, MembershipSource, Mri, Roster, Scope, Target, WriteKind,
};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt as _, AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _, BufReader,
};
use tokio::sync::mpsc;
use tokio_rustls::rustls::pki_types::PrivatePkcs8KeyDer;
use tokio_tungstenite::tungstenite::Message;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Helpers return errors; the tests unwrap them.
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
/// The same, for what a spawned task returns.
type SendResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const TOKEN: &str = "https://login.microsoftonline.com/0f0e0d0c-0b0a-0908-0706-050403020100/oauth2/v2.0/token";
const CHAT: &str = "https://emea.ng.msg.teams.microsoft.com/v1/users/ME/conversations/19:lab@thread.v2/messages";

// --- the classifier ---

#[test]
fn reading_methods_are_reads_whatever_the_url() {
    for method in ["GET", "HEAD", "OPTIONS", "get", "Options"] {
        assert_eq!(classify(method, CHAT, None), Classification::Read, "{method}");
        assert_eq!(classify(method, "not a url", None), Classification::Read, "{method}");
        assert!(!is_mutating(method));
    }
}

#[test]
fn every_other_method_is_mutating_and_unknown_outside_the_own_list() {
    for method in [
        "POST", "PUT", "PATCH", "DELETE", "post", "PROPFIND", "CONNECT", "TRACE", "",
    ] {
        assert!(is_mutating(method), "{method}");
        assert_eq!(classify(method, CHAT, Some(b"{}")), Classification::Unknown, "{method}");
    }
}

#[test]
fn the_entra_token_endpoint_of_any_tenant_is_own() {
    for tenant in [
        "0f0e0d0c-0b0a-0908-0706-050403020100",
        "common",
        "organizations",
        "contoso.example",
    ] {
        let url = format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token");
        assert_eq!(
            classify("POST", &url, None),
            Classification::Own(OwnKind::EntraToken),
            "{tenant}"
        );
    }
    let with_query = format!("{TOKEN}?client-request-id=1");
    assert_eq!(
        classify("post", &with_query, None),
        Classification::Own(OwnKind::EntraToken)
    );
    assert_eq!(
        classify(
            "POST",
            "https://login.microsoftonline.com:443/common/oauth2/v2.0/token",
            None
        ),
        Classification::Own(OwnKind::EntraToken),
        "the default port is the same origin"
    );
}

#[test]
fn the_authz_endpoints_are_own() {
    for url in [
        "https://teams.microsoft.com/api/authsvc/v1.0/authz",
        "https://teams.microsoft.com/api/authsvc/v1.0/authzcore",
        "https://authsvc.teams.microsoft.com/v1.0/authz",
        "https://authsvc.teams.microsoft.com/v1.0/authzcore",
    ] {
        assert_eq!(
            classify("POST", url, None),
            Classification::Own(OwnKind::Authz),
            "{url}"
        );
    }
}

#[test]
fn lookalikes_of_own_endpoints_are_unknown() {
    let lookalikes = [
        ("PUT", TOKEN),
        ("DELETE", "https://teams.microsoft.com/api/authsvc/v1.0/authz"),
        ("POST", "http://login.microsoftonline.com/common/oauth2/v2.0/token"),
        (
            "POST",
            "https://login.microsoftonline.com:8443/common/oauth2/v2.0/token",
        ),
        (
            "POST",
            "https://login.microsoftonline.com.attacker.example/common/oauth2/v2.0/token",
        ),
        (
            "POST",
            "https://attacker.example/login.microsoftonline.com/common/oauth2/v2.0/token",
        ),
        ("POST", "https://login.microsoftonline.com//oauth2/v2.0/token"),
        ("POST", "https://login.microsoftonline.com/oauth2/v2.0/token"),
        ("POST", "https://login.microsoftonline.com/common/oauth2/v2.0/token/"),
        (
            "POST",
            "https://login.microsoftonline.com/common/oauth2/v2.0/token/more",
        ),
        ("POST", "https://login.microsoftonline.com/a/b/oauth2/v2.0/token"),
        ("POST", "https://login.microsoftonline.com/common/oauth2/token"),
        ("POST", "https://login.microsoftonline.com/common/oauth2/v2.0/%74oken"),
        ("POST", "https://teams.microsoft.com/api/authsvc/v1.0/authz/extra"),
        ("POST", "https://teams.microsoft.com/api/authsvc/v1.0/users/region"),
        ("POST", "https://teams.live.com/api/auth/v2.0/authz/consumer"),
        ("POST", "not a url"),
        ("POST", ""),
    ];
    for (method, url) in lookalikes {
        assert_eq!(classify(method, url, None), Classification::Unknown, "{method} {url}");
    }
}

#[test]
fn a_local_rule_stands_in_for_an_own_endpoint_without_widening_the_seeded_list() {
    let classifier = Classifier::seeded().with_own(OwnRule::new(
        OwnKind::Authz,
        "post",
        "http://127.0.0.1:4000",
        "/own/authz",
    ));
    assert_eq!(
        classifier.classify("POST", "http://127.0.0.1:4000/own/authz", None),
        Classification::Own(OwnKind::Authz)
    );
    assert_eq!(
        classifier.classify("POST", "http://127.0.0.1:4001/own/authz", None),
        Classification::Unknown
    );
    assert_eq!(
        classify("POST", "http://127.0.0.1:4000/own/authz", None),
        Classification::Unknown,
        "the seeded classifier doesn't know it"
    );
    assert_eq!(
        classifier.classify("POST", TOKEN, None),
        Classification::Own(OwnKind::EntraToken)
    );
}

#[test]
fn own_kinds_are_named_for_the_log() {
    assert_eq!(OwnKind::EntraToken.as_str(), "entra_token");
    assert_eq!(OwnKind::Authz.as_str(), "authz");
}

// --- the decision on one paused request ---

const LAB_A: &str = "8:orgid:lab-a";
const LAB_B: &str = "8:orgid:lab-b";
const COWORKER: &str = "8:orgid:coworker";

/// A membership source that answers the same every time.
struct Fixed(Membership);

impl MembershipSource for Fixed {
    fn members<'a>(&'a self, _scope: &'a Scope) -> MembershipFuture<'a> {
        let membership = self.0.clone();
        Box::pin(async move { membership })
    }
}

struct Lab {
    _dir: tempfile::TempDir,
    log_path: PathBuf,
    fence: Arc<Fence>,
}

fn lab() -> TestResult<Lab> {
    let dir = tempfile::tempdir()?;
    let log_path = dir.path().join("fence.log");
    let roster = Roster::new([Mri::new(LAB_A), Mri::new(LAB_B)], []);
    let fence = Arc::new(Fence::new(roster, FenceLog::open(&log_path)?));
    Ok(Lab {
        _dir: dir,
        log_path,
        fence,
    })
}

fn http_fence(lab: &Lab, classifier: Classifier, members: &[&str]) -> HttpFence {
    let members = Membership::Complete(members.iter().map(|&mri| Mri::new(mri)).collect());
    HttpFence::new(classifier, Arc::clone(&lab.fence), Arc::new(Fixed(members)))
}

/// The WebSocket layer with the seeded classifier: every frame is denied.
fn ws_fence(lab: &Lab) -> TestResult<WsFence> {
    let members = Arc::new(Fixed(Membership::Complete(Vec::new())));
    Ok(WsFence::new(
        FrameClassifier::seeded(),
        Arc::clone(&lab.fence),
        members,
        GuardScript::generate()?,
    ))
}

fn log_entries(path: &Path) -> TestResult<Vec<Value>> {
    let text = std::fs::read_to_string(path)?;
    Ok(text
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<Vec<Value>, _>>()?)
}

fn paused(method: &str, url: &str) -> PausedRequest {
    PausedRequest {
        request_id: "interception-job-1.0".to_owned(),
        method: method.to_owned(),
        url: url.to_owned(),
        body: None,
        network_id: None,
    }
}

#[tokio::test]
async fn an_unclassified_post_is_denied_kept_and_logged_without_its_query() {
    let lab = lab().unwrap();
    let fence = http_fence(&lab, Classifier::seeded(), &[LAB_A]);

    let verdict = fence.decide(&paused("POST", &format!("{CHAT}?secret=planted"))).await;

    assert_eq!(verdict, Verdict::Deny(DenyCause::Unclassified));
    let denials = fence.denials();
    assert_eq!(denials.len(), 1);
    assert_eq!(denials[0].method, "POST");
    assert_eq!(denials[0].url, CHAT, "no query");
    let entries = log_entries(&lab.log_path).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["verdict"], "deny");
    assert_eq!(entries[0]["reason"], "unclassified");
    assert_eq!(entries[0]["url"], CHAT);
    assert!(!std::fs::read_to_string(&lab.log_path).unwrap().contains("planted"));
}

#[tokio::test]
async fn reads_pass_unlogged_and_own_requests_pass_logged() {
    let lab = lab().unwrap();
    let fence = http_fence(&lab, Classifier::seeded(), &[LAB_A]);

    assert_eq!(fence.decide(&paused("GET", CHAT)).await, Verdict::Allow);
    assert_eq!(fence.decide(&paused("POST", TOKEN)).await, Verdict::Allow);

    assert!(fence.denials().is_empty());
    let entries = log_entries(&lab.log_path).unwrap();
    assert_eq!(entries.len(), 1, "only the own request: {entries:?}");
    assert_eq!(entries[0]["verdict"], "allow");
    assert_eq!(entries[0]["own"], "entra_token");
}

#[tokio::test]
async fn an_own_request_that_cannot_be_logged_is_denied() {
    struct Broken;
    impl std::io::Write for Broken {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("disk full"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let fence = Arc::new(Fence::new(Roster::default(), FenceLog::from_writer(Broken)));
    let http = HttpFence::new(
        Classifier::seeded(),
        fence,
        Arc::new(Fixed(Membership::Complete(Vec::new()))),
    );

    let verdict = http.decide(&paused("POST", TOKEN)).await;

    assert_eq!(verdict, Verdict::Deny(DenyCause::LogUnavailable));
    assert_eq!(http.denials().len(), 1);
}

#[tokio::test]
async fn a_mapped_write_goes_through_the_fence_with_a_fresh_membership() {
    let lab = lab().unwrap();
    let target = || {
        Target::new(
            WriteKind::Message,
            Scope::Conversation {
                id: "19:lab@thread.v2".to_owned(),
            },
        )
    };

    let lab_only = http_fence(&lab, Classifier::seeded(), &[LAB_A, LAB_B]);
    let verdict = lab_only.judge(Classification::Write(target()), "POST", CHAT).await;
    assert_eq!(verdict, Verdict::Allow);

    let with_coworker = http_fence(&lab, Classifier::seeded(), &[LAB_A, COWORKER]);
    let verdict = with_coworker.judge(Classification::Write(target()), "POST", CHAT).await;
    assert_eq!(
        verdict,
        Verdict::Deny(DenyCause::Fenced(DenyReason::Outsiders { count: 1 }))
    );
    assert_eq!(with_coworker.denials().len(), 1);

    let entries = log_entries(&lab.log_path).unwrap();
    let verdicts: Vec<&Value> = entries.iter().map(|entry| &entry["verdict"]).collect();
    assert_eq!(verdicts, [&json!("allow"), &json!("deny")], "the fence logs both");
}

#[test]
fn a_paused_event_gives_the_request_and_its_inline_body() {
    let params = json!({
        "requestId": "interception-job-7.0",
        "request": {"method": "POST", "url": CHAT, "postData": "{\"content\":\"hi\"}", "headers": {}},
        "resourceType": "Fetch",
    });
    let request = PausedRequest::from_params(&params).unwrap();
    assert_eq!(request.request_id, "interception-job-7.0");
    assert_eq!(request.method, "POST");
    assert_eq!(request.url, CHAT);
    assert_eq!(request.body.as_deref(), Some(&b"{\"content\":\"hi\"}"[..]));

    let without_body = json!({"requestId": "r", "request": {"method": "GET", "url": CHAT}});
    assert_eq!(PausedRequest::from_params(&without_body).unwrap().body, None);

    for (field, params) in [
        ("requestId", json!({"request": {"method": "POST", "url": CHAT}})),
        ("request.method", json!({"requestId": "r", "request": {"url": CHAT}})),
        ("request.url", json!({"requestId": "r", "request": {"method": "POST"}})),
    ] {
        assert_eq!(PausedRequest::from_params(&params), Err(MalformedPause(field)));
    }
}

// --- arming, against a scripted fake browser ---

#[test]
fn every_kind_of_target_is_resumed_last_and_only_dedicated_workers_skip_fetch() {
    let kinds = [
        TargetKind::Page,
        TargetKind::Iframe,
        TargetKind::Worker,
        TargetKind::SharedWorker,
        TargetKind::ServiceWorker,
        TargetKind::Other("browser_ui".to_owned()),
    ];
    for kind in kinds {
        let steps = kind.steps();
        assert_eq!(steps.last(), Some(&Step::Resume), "{kind:?}");
        assert_eq!(steps.iter().filter(|step| **step == Step::Resume).count(), 1);
        assert!(steps.contains(&Step::AutoAttach), "{kind:?} children must be held too");
        let fetch = steps.contains(&Step::Fetch);
        assert_eq!(fetch, kind != TargetKind::Worker, "{kind:?}");
        assert!(
            steps.contains(&Step::Network),
            "{kind:?}: the frame audit reads Network events"
        );
        let page_like = matches!(kind, TargetKind::Page | TargetKind::Iframe);
        assert_eq!(steps.contains(&Step::BypassServiceWorker), page_like, "{kind:?}");
        if page_like {
            let position = |wanted| steps.iter().position(|step| *step == wanted);
            assert!(
                position(Step::Network) < position(Step::BypassServiceWorker),
                "the bypass needs the Network domain on"
            );
        }
    }
    assert_eq!(TargetKind::from_type("service_worker"), TargetKind::ServiceWorker);
    assert_eq!(TargetKind::from_type("shared_worker"), TargetKind::SharedWorker);
    assert_eq!(
        TargetKind::from_type("auction_worklet"),
        TargetKind::Other("auction_worklet".to_owned())
    );
}

#[test]
fn the_default_run_neutralizes_the_teams_origins() {
    assert_eq!(
        ArmOptions::default().service_worker_origins,
        TEAMS_ORIGINS.map(str::to_owned).to_vec()
    );
}

/// A fake browser endpoint: it answers every command (with an error when `refuse` says so), records them, and sends
/// whatever events the test pushes. Its process is a shell script that points `DevToolsActivePort` at the endpoint,
/// starts a helper (as Chrome starts its crash handler) and waits, so the tests can see the browser killed.
struct FakeBrowser {
    port: u16,
    commands: Arc<Mutex<Vec<Value>>>,
    push: mpsc::UnboundedSender<Value>,
    root: tempfile::TempDir,
}

/// Hangs the fake browser up when pushed as an event, as a browser dropping the connection would.
const HANG_UP: Value = Value::Null;

impl FakeBrowser {
    async fn start(refuse: fn(&str, Option<&str>) -> bool) -> TestResult<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let commands = Arc::new(Mutex::new(Vec::new()));
        let (push, pushed) = mpsc::unbounded_channel();
        tokio::spawn(serve_fake(listener, Arc::clone(&commands), pushed, refuse));
        Ok(Self {
            port,
            commands,
            push,
            root: tempfile::tempdir()?,
        })
    }

    /// Starts the fake browser's process.
    async fn launch(&self) -> TestResult<Browser> {
        let script = format!(
            "#!/bin/sh\n\
             for arg; do case \"$arg\" in --user-data-dir=*) dir=\"${{arg#--user-data-dir=}}\";; esac; done\n\
             sleep 300 &\n\
             echo $! > \"$dir/helper.pid\"\n\
             printf '{}\\n/devtools/browser/fake\\n' > \"$dir/DevToolsActivePort\"\n\
             exec sleep 300\n",
            self.port
        );
        let executable = self.root.path().join("fake-chrome");
        std::fs::write(&executable, script)?;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))?;
        let options = LaunchOptions::new(self.root.path().join("p"));
        Ok(Browser::launch(&executable, &options, Duration::from_secs(10)).await?)
    }

    /// The helper the fake browser's process started: it lives as long as the browser's process group.
    fn helper(&self) -> TestResult<u32> {
        let pid = std::fs::read_to_string(self.root.path().join("p").join("helper.pid"))?;
        Ok(pid.trim().parse()?)
    }

    fn hang_up(&self) -> TestResult {
        self.push.send(HANG_UP).map_err(|_| "the fake browser is gone")?;
        Ok(())
    }

    fn event(&self, session: Option<&str>, method: &str, params: &Value) -> TestResult {
        let mut event = json!({"method": method, "params": params});
        if let Some(session) = session {
            event["sessionId"] = json!(session);
        }
        self.push.send(event).map_err(|_| "the fake browser is gone")?;
        Ok(())
    }

    fn commands(&self) -> Vec<Value> {
        self.commands.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The methods sent to one session (`None`: the browser), in order.
    fn methods_on(&self, session: Option<&str>) -> Vec<String> {
        self.commands()
            .iter()
            .filter(|command| command["sessionId"].as_str() == session)
            .filter_map(|command| command["method"].as_str().map(str::to_owned))
            .collect()
    }

    /// Waits until a command matching `wanted` was sent, and returns it.
    async fn sent(&self, what: &str, wanted: impl Fn(&Value) -> bool) -> TestResult<Value> {
        let poll = async {
            loop {
                if let Some(command) = self.commands().into_iter().find(|command| wanted(command)) {
                    return command;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        Ok(tokio::time::timeout(Duration::from_secs(5), poll)
            .await
            .map_err(|_| format!("no {what} was sent"))?)
    }
}

async fn serve_fake(
    listener: tokio::net::TcpListener,
    commands: Arc<Mutex<Vec<Value>>>,
    mut pushed: mpsc::UnboundedReceiver<Value>,
    refuse: fn(&str, Option<&str>) -> bool,
) -> SendResult<()> {
    let (stream, _) = listener.accept().await?;
    let mut socket = tokio_tungstenite::accept_async(stream).await?;
    loop {
        tokio::select! {
            message = socket.next() => {
                let Some(Ok(Message::Text(text))) = message else { return Ok(()) };
                let command: Value = serde_json::from_str(text.as_str())?;
                commands.lock().unwrap_or_else(PoisonError::into_inner).push(command.clone());
                // a refused Browser.close hangs up unanswered: the connection lost while the browser is still running
                if command["method"] == "Browser.close" && refuse("Browser.close", None) {
                    return Ok(());
                }
                answer_fake(&mut socket, &command, refuse).await?;
            }
            event = pushed.recv() => {
                let Some(event) = event.filter(|event| *event != HANG_UP) else { return Ok(()) };
                socket.send(Message::text(event.to_string())).await?;
            }
        }
    }
}

/// The session whose target detaches before its resume is answered: a short-lived worker, or a tab closed at once.
const SHORT_LIVED: &str = "S-short";

/// The session whose WebSocket guard reports that it has no binding to ask through.
const UNGUARDED: &str = "S-unguarded";

/// The target the browser won't close.
const STUCK: &str = "T-stuck";

async fn answer_fake(
    socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    command: &Value,
    refuse: fn(&str, Option<&str>) -> bool,
) -> SendResult<()> {
    let method = &command["method"];
    if method == "Runtime.runIfWaitingForDebugger" && command["sessionId"] == SHORT_LIVED {
        let event = json!({"method": "Target.detachedFromTarget", "params": {"sessionId": SHORT_LIVED}});
        socket.send(Message::text(event.to_string())).await?;
        // the detach is handled well before the resume's reply wakes the arming up
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    socket
        .send(Message::text(fake_reply(command, refuse).to_string()))
        .await?;
    if method == "Target.createTarget" {
        // a new tab attaches through the browser's auto-attach
        let event = json!({"method": "Target.attachedToTarget", "params": attached("S-new", "T-new", "page")});
        socket.send(Message::text(event.to_string())).await?;
    }
    Ok(())
}

fn fake_reply(command: &Value, refuse: fn(&str, Option<&str>) -> bool) -> Value {
    let method = command["method"].as_str().unwrap_or_default();
    let id = &command["id"];
    let session = command["sessionId"].as_str();
    if session == Some("S-gone") && method != "Fetch.enable" {
        return json!({"id": id, "error": {"code": -32001, "message": "Session with given id not found."}});
    }
    if refuse(method, session) {
        return json!({"id": id, "error": {"code": -32601, "message": format!("'{method}' wasn't found")}});
    }
    let result = match method {
        "Target.createTarget" => json!({"targetId": "T-new"}),
        // the browser won't close the stuck target
        "Target.closeTarget" if command["params"]["targetId"] == STUCK => json!({"success": false}),
        "Target.closeTarget" => json!({"success": true}),
        // the WebSocket guard reports itself armed, except where the binding is missing
        "Runtime.evaluate" if session == Some(UNGUARDED) => {
            json!({"result": {"type": "string", "value": "no binding"}})
        }
        "Runtime.evaluate" => json!({"result": {"type": "string", "value": "armed"}}),
        _ => json!({}),
    };
    json!({"id": id, "result": result})
}

fn attached(session: &str, target: &str, kind: &str) -> Value {
    json!({
        "sessionId": session,
        "targetInfo": {"targetId": target, "type": kind, "url": "http://127.0.0.1/", "attached": true},
        "waitingForDebugger": true,
    })
}

async fn arm_fake(fake: &FakeBrowser, origins: &[&str]) -> TestResult<(ArmedBrowser, Lab)> {
    let lab = lab()?;
    let options = ArmOptions {
        service_worker_origins: origins.iter().map(|&origin| origin.to_owned()).collect(),
    };
    let armed = ArmedBrowser::arm(
        fake.launch().await?,
        http_fence(&lab, Classifier::seeded(), &[]),
        ws_fence(&lab)?,
        &options,
    )
    .await?;
    Ok((armed, lab))
}

fn refuse_nothing(_method: &str, _session: Option<&str>) -> bool {
    false
}

#[tokio::test]
async fn the_browser_is_fenced_then_cleaned_through_a_blank_armed_tab() {
    let fake = FakeBrowser::start(refuse_nothing).await.unwrap();
    let (armed, _lab) = arm_fake(&fake, &["https://a.example", "https://b.example"])
        .await
        .unwrap();

    let browser: Vec<Value> = fake
        .commands()
        .into_iter()
        .filter(|command| command.get("sessionId").is_none())
        .collect();
    let methods: Vec<&str> = browser
        .iter()
        .filter_map(|command| command["method"].as_str())
        .collect();
    assert_eq!(
        methods,
        [
            "Fetch.enable",
            "Target.setAutoAttach",
            "Target.createTarget",
            "Target.closeTarget"
        ]
    );
    assert_eq!(browser[0]["params"], json!({"patterns": [{"urlPattern": "*"}]}));
    assert_eq!(
        browser[1]["params"],
        json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true})
    );
    assert_eq!(browser[3]["params"]["targetId"], "T-new", "the blank tab is closed");

    // the blank tab is armed like any page, and the origins are cleared through it once it runs
    assert_eq!(
        fake.methods_on(Some("S-new")),
        [
            "Fetch.enable",
            "Network.enable",
            "Network.setBypassServiceWorker",
            "Runtime.enable",
            "Page.enable",
            "Runtime.addBinding",
            "Page.addScriptToEvaluateOnNewDocument",
            "Target.setAutoAttach",
            "Runtime.runIfWaitingForDebugger",
            "Storage.clearDataForOrigin",
            "Storage.clearDataForOrigin"
        ]
    );
    let cleared: Vec<Value> = fake
        .commands()
        .into_iter()
        .filter(|command| command["method"] == "Storage.clearDataForOrigin")
        .map(|command| command["params"].clone())
        .collect();
    assert_eq!(
        cleared,
        [
            json!({"origin": "https://a.example", "storageTypes": "service_workers,cache_storage"}),
            json!({"origin": "https://b.example", "storageTypes": "service_workers,cache_storage"}),
        ]
    );
    let on_tab = |method: &str| {
        fake.commands()
            .into_iter()
            .find(|command| command["sessionId"] == "S-new" && command["method"] == method)
            .map(|command| command["params"].clone())
            .unwrap_or_default()
    };
    // its children are held too, but a service worker attaches through the browser only, once
    let held = on_tab("Target.setAutoAttach");
    assert_eq!(held["waitForDebuggerOnStart"], true);
    assert_eq!(held["filter"][0], json!({"type": "service_worker", "exclude": true}));
    // the WebSocket guard runs in its documents to come and in the ones there already
    let guard = on_tab("Page.addScriptToEvaluateOnNewDocument");
    assert_eq!(guard["runImmediately"], true);
    assert!(
        guard["source"]
            .as_str()
            .is_some_and(|source| source.contains("__labFenceWsVerdict"))
    );
    armed.verdict().unwrap();
}

#[tokio::test]
async fn every_attached_target_is_armed_on_its_own_session_before_it_runs() {
    let fake = FakeBrowser::start(refuse_nothing).await.unwrap();
    let (armed, _lab) = arm_fake(&fake, &[]).await.unwrap();
    let page_like = || {
        vec![
            "Fetch.enable",
            "Network.enable",
            "Network.setBypassServiceWorker",
            "Runtime.enable",
            "Page.enable",
            "Runtime.addBinding",
            "Page.addScriptToEvaluateOnNewDocument",
            "Target.setAutoAttach",
        ]
    };
    let guarded = ["Network.enable", "Runtime.addBinding", "Runtime.evaluate"];
    let worker_like = || [&["Fetch.enable"][..], &guarded, &["Target.setAutoAttach"]].concat();
    let cases = [
        (None, "S-page", "page", page_like()),
        (Some("S-page"), "S-frame", "iframe", page_like()),
        (
            Some("S-page"),
            "S-worker",
            "worker",
            [&guarded[..], &["Target.setAutoAttach"]].concat(),
        ),
        (None, "S-shared", "shared_worker", worker_like()),
        (Some("S-page"), "S-service", "service_worker", worker_like()),
        (
            None,
            "S-ui",
            "browser_ui",
            vec![
                "Fetch.enable",
                "Network.enable",
                "Runtime.enable",
                "Runtime.addBinding",
                "Runtime.evaluate",
                "Target.setAutoAttach",
            ],
        ),
    ];
    for (parent, session, kind, _) in &cases {
        fake.event(
            *parent,
            "Target.attachedToTarget",
            &attached(session, &format!("T-{session}"), kind),
        )
        .unwrap();
    }

    for (parent, session, kind, arming) in cases {
        fake.sent("the resume", |command| {
            command["method"] == "Runtime.runIfWaitingForDebugger" && command["sessionId"] == session
        })
        .await
        .unwrap();
        let mut expected = arming;
        expected.push("Runtime.runIfWaitingForDebugger");
        assert_eq!(fake.methods_on(Some(session)), expected, "{kind}");
        let target = armed_target(&armed, session).await.unwrap();
        assert_eq!(target.kind, TargetKind::from_type(kind));
        assert_eq!(target.parent, parent.map(SessionId::new));
    }
    assert!(armed.failures().is_empty());
    armed.verdict().unwrap();
}

/// Waits for a session to show up among the armed targets (it is recorded once its resume is answered).
async fn armed_target(armed: &ArmedBrowser, session: &str) -> TestResult<ArmedTarget> {
    let poll = async {
        loop {
            let found = armed
                .targets()
                .into_iter()
                .find(|target| target.session.as_str() == session);
            if let Some(target) = found {
                return target;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    Ok(tokio::time::timeout(Duration::from_secs(5), poll)
        .await
        .map_err(|_| format!("{session} wasn't armed"))?)
}

fn refuse_fetch_on_s9(method: &str, session: Option<&str>) -> bool {
    method == "Fetch.enable" && session == Some("S9")
}

#[tokio::test]
async fn a_target_that_cannot_be_armed_is_closed_and_never_resumed() {
    let fake = FakeBrowser::start(refuse_fetch_on_s9).await.unwrap();
    let (armed, _lab) = arm_fake(&fake, &[]).await.unwrap();

    fake.event(None, "Target.attachedToTarget", &attached("S9", "T9", "service_worker"))
        .unwrap();

    let close = fake
        .sent("Target.closeTarget", |command| {
            command["method"] == "Target.closeTarget" && command["params"]["targetId"] == "T9"
        })
        .await
        .unwrap();
    assert!(close.get("sessionId").is_none(), "closed through the browser");
    assert_eq!(
        fake.methods_on(Some("S9")),
        ["Fetch.enable"],
        "nothing after the failed step"
    );
    // the failure is recorded right after the close
    let failures = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let failures = armed.failures();
            if !failures.is_empty() {
                return failures;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].target_id.as_deref(), Some("T9"));
    assert_eq!(failures[0].kind, "service_worker");
    assert!(failures[0].closed);
    assert!(failures[0].error.contains("Fetch"), "{}", failures[0].error);
    assert!(armed.targets().iter().all(|target| target.session.as_str() != "S9"));
    let failed = armed.verdict().unwrap_err();
    assert_eq!(failed.failures.len(), 1);
}

/// `S-held`'s and `S-stuck`'s `Page.enable` and `S-ran`'s resume are refused (and [`STUCK`] isn't closed).
fn refuse_held_and_ran(method: &str, session: Option<&str>) -> bool {
    (method == "Page.enable" && matches!(session, Some("S-held" | "S-stuck")))
        || (method == "Runtime.runIfWaitingForDebugger" && session == Some("S-ran"))
}

/// Attaches an iframe through the page's session and waits until it has failed to arm; gives what the fence did.
async fn iframe_that_fails(fake: &FakeBrowser, armed: &ArmedBrowser, session: &str) -> TestResult<FenceReport> {
    let target = format!("T{}", session.trim_start_matches('S'));
    fake.event(
        Some("S-page"),
        "Target.attachedToTarget",
        &attached(session, &target, "iframe"),
    )?;
    let wait = async {
        loop {
            if !armed.failures().is_empty() {
                return FenceReport::of(&armed.decisions());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    Ok(tokio::time::timeout(Duration::from_secs(5), wait)
        .await
        .map_err(|_| format!("{session} didn't fail"))?)
}

#[tokio::test]
async fn a_target_closed_before_it_ran_is_a_warning_not_a_breach() {
    let fake = FakeBrowser::start(refuse_held_and_ran).await.unwrap();
    let (armed, _lab) = arm_fake(&fake, &[]).await.unwrap();
    let held = iframe_that_fails(&fake, &armed, "S-held").await.unwrap();

    assert!(
        !fake
            .methods_on(Some("S-held"))
            .contains(&"Runtime.runIfWaitingForDebugger".to_owned())
    );
    assert!(
        held.unarmed[0].closed && !held.unarmed[0].may_have_run,
        "{:?}",
        held.unarmed
    );
    assert!(!held.breached(), "{}", held.summary());
    assert_eq!(held.closed_unarmed().count(), 1);
    assert!(
        held.summary().contains("iframe target closed unarmed before it ran"),
        "{}",
        held.summary()
    );
    // the run's strict verdict still lists it
    assert_eq!(armed.verdict().unwrap_err().failures.len(), 1);
    // a scenario warns about it and doesn't fail on it
    let mut report = ScenarioReport::new(ScenarioName::AbChat);
    let run = AccountRun {
        alias: Alias::A,
        fence: held,
        denials: Vec::new(),
        frame_denials: Vec::new(),
        capture: None,
    };
    report.add_account(run, DenyPolicy::AllExpected);
    assert!(!report.failed(), "{:?}", report.failures);
    let warned = |note: &String| note.contains("warning") && note.contains("1 target(s)");
    assert!(report.notes.iter().any(warned), "{:?}", report.notes);
}

#[tokio::test]
async fn a_target_whose_resume_was_sent_may_have_run_and_is_a_breach() {
    let fake = FakeBrowser::start(refuse_held_and_ran).await.unwrap();
    let (armed, _lab) = arm_fake(&fake, &[]).await.unwrap();
    let ran = iframe_that_fails(&fake, &armed, "S-ran").await.unwrap();

    assert!(
        ran.unarmed[0].closed && ran.unarmed[0].may_have_run,
        "{:?}",
        ran.unarmed
    );
    assert!(ran.breached(), "{}", ran.summary());
    assert_eq!(ran.closed_unarmed().count(), 0);
    let mut report = ScenarioReport::new(ScenarioName::AbChat);
    let run = AccountRun {
        alias: Alias::A,
        fence: ran,
        denials: Vec::new(),
        frame_denials: Vec::new(),
        capture: None,
    };
    report.add_account(run, DenyPolicy::AllExpected);
    assert!(
        report.failures.iter().any(|failure| failure.contains("breached")),
        "{:?}",
        report.failures
    );
}

#[tokio::test]
async fn a_target_that_could_not_be_closed_is_a_breach() {
    let fake = FakeBrowser::start(refuse_held_and_ran).await.unwrap();
    let (armed, _lab) = arm_fake(&fake, &[]).await.unwrap();
    let stuck = iframe_that_fails(&fake, &armed, "S-stuck").await.unwrap();

    assert!(
        !stuck.unarmed[0].closed && !stuck.unarmed[0].may_have_run,
        "{:?}",
        stuck.unarmed
    );
    assert!(stuck.breached(), "{}", stuck.summary());
    assert_eq!(stuck.closed_unarmed().count(), 0);
}

#[tokio::test]
async fn a_worker_whose_websocket_guard_is_not_armed_is_closed_and_never_resumed() {
    let fake = FakeBrowser::start(refuse_nothing).await.unwrap();
    let (armed, _lab) = arm_fake(&fake, &[]).await.unwrap();

    fake.event(
        Some("S-page"),
        "Target.attachedToTarget",
        &attached(UNGUARDED, "T-unguarded", "worker"),
    )
    .unwrap();

    fake.sent("Target.closeTarget", |command| {
        command["method"] == "Target.closeTarget" && command["params"]["targetId"] == "T-unguarded"
    })
    .await
    .unwrap();
    let methods = fake.methods_on(Some(UNGUARDED));
    assert_eq!(
        methods.last().map(String::as_str),
        Some("Runtime.evaluate"),
        "{methods:?}"
    );
    assert!(!methods.contains(&"Runtime.runIfWaitingForDebugger".to_owned()));
    let failed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Err(failed) = armed.verdict() {
                return failed;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(failed.failures.len(), 1);
    assert!(
        failed.failures[0].error.contains("no binding"),
        "{}",
        failed.failures[0].error
    );
}

#[tokio::test]
async fn a_session_gone_before_it_is_armed_is_dropped_without_closing_its_target() {
    let fake = FakeBrowser::start(refuse_nothing).await.unwrap();
    let (armed, _lab) = arm_fake(&fake, &[]).await.unwrap();

    // a target attached through a page whose tab closes while it is being armed, and once more through the browser
    // (as a service worker was before target-level auto-attach left them out)
    fake.event(
        Some("S-page"),
        "Target.attachedToTarget",
        &attached("S-gone", "T-sw", "service_worker"),
    )
    .unwrap();
    fake.event(
        None,
        "Target.attachedToTarget",
        &attached("S-sw", "T-sw", "service_worker"),
    )
    .unwrap();

    armed_target(&armed, "S-sw").await.unwrap();
    fake.sent("the gone session's second step", |command| {
        command["sessionId"] == "S-gone" && command["method"] == "Network.enable"
    })
    .await
    .unwrap();
    assert_eq!(
        fake.methods_on(Some("S-gone")),
        ["Fetch.enable", "Network.enable"],
        "nothing after the step that found the session gone"
    );
    let closed: Vec<Value> = fake
        .commands()
        .into_iter()
        .filter(|command| command["method"] == "Target.closeTarget" && command["params"]["targetId"] == "T-sw")
        .collect();
    assert_eq!(
        closed,
        Vec::<Value>::new(),
        "the target lives on through its other, armed session"
    );
    assert!(armed.failures().is_empty(), "{:?}", armed.failures());
}

#[tokio::test]
async fn a_target_that_detaches_while_it_is_armed_is_never_listed() {
    let fake = FakeBrowser::start(refuse_nothing).await.unwrap();
    let (armed, _lab) = arm_fake(&fake, &[]).await.unwrap();

    fake.event(
        None,
        "Target.attachedToTarget",
        &attached(SHORT_LIVED, "T-short", "worker"),
    )
    .unwrap();
    fake.event(None, "Target.attachedToTarget", &attached("S-long", "T-long", "page"))
        .unwrap();

    armed_target(&armed, "S-long").await.unwrap();
    fake.sent("the short-lived target's resume", |command| {
        command["method"] == "Runtime.runIfWaitingForDebugger" && command["sessionId"] == SHORT_LIVED
    })
    .await
    .unwrap();
    // the resume is answered 200 ms after the detach; the arming finishes right after that
    tokio::time::sleep(Duration::from_millis(500)).await;
    let sessions: Vec<String> = armed
        .targets()
        .iter()
        .map(|target| target.session.to_string())
        .collect();
    assert!(!sessions.iter().any(|session| session == SHORT_LIVED), "{sessions:?}");
    assert!(sessions.iter().any(|session| session == "S-long"), "{sessions:?}");
    armed.verdict().unwrap();
}

#[tokio::test]
async fn dropping_the_fence_kills_the_browser() {
    let fake = FakeBrowser::start(refuse_nothing).await.unwrap();
    let (armed, _lab) = arm_fake(&fake, &[]).await.unwrap();
    let helper = fake.helper().unwrap();
    assert!(process::running(helper));

    drop(armed);

    assert!(process::gone(helper).await, "the browser outlived its fence");
}

fn refuse_browser_auto_attach(method: &str, session: Option<&str>) -> bool {
    method == "Target.setAutoAttach" && session.is_none()
}

#[tokio::test]
async fn a_browser_that_cannot_be_armed_is_killed() {
    let fake = FakeBrowser::start(refuse_browser_auto_attach).await.unwrap();

    let armed = arm_fake(&fake, &[]).await;

    assert!(armed.is_err(), "the browser refused auto-attach");
    assert!(
        process::gone(fake.helper().unwrap()).await,
        "the browser outlived a failed arming"
    );
}

#[tokio::test]
async fn closing_the_fence_asks_the_browser_to_close_then_stops_it() {
    let fake = FakeBrowser::start(refuse_nothing).await.unwrap();
    let (armed, _lab) = arm_fake(&fake, &[]).await.unwrap();
    let helper = fake.helper().unwrap();

    armed.close(Duration::from_millis(100)).await.unwrap();

    assert!(fake.methods_on(None).contains(&"Browser.close".to_owned()));
    assert!(process::gone(helper).await, "the browser outlived close");
}

/// A membership source that answers only after `delay`, as a slow live read does, and says when it is asked.
struct Slow {
    delay: Duration,
    membership: Membership,
    asked: Arc<tokio::sync::Notify>,
}

impl MembershipSource for Slow {
    fn members<'a>(&'a self, _scope: &'a Scope) -> MembershipFuture<'a> {
        self.asked.notify_one();
        Box::pin(async move {
            tokio::time::sleep(self.delay).await;
            self.membership.clone()
        })
    }
}

#[tokio::test]
async fn closing_the_fence_waits_for_the_decisions_still_running() {
    let fake = FakeBrowser::start(refuse_nothing).await.unwrap();
    let lab = lab().unwrap();
    let asked = Arc::new(tokio::sync::Notify::new());
    let members = Slow {
        delay: Duration::from_secs(2),
        membership: Membership::Complete(vec![Mri::new(LAB_A), Mri::new(COWORKER)]),
        asked: Arc::clone(&asked),
    };
    let http = HttpFence::new(Classifier::mapped(), Arc::clone(&lab.fence), Arc::new(members));
    let options = ArmOptions {
        service_worker_origins: Vec::new(),
    };
    let armed = ArmedBrowser::arm(fake.launch().await.unwrap(), http, ws_fence(&lab).unwrap(), &options)
        .await
        .unwrap();
    let decisions = armed.decisions();
    let body = r#"{"messagetype":"Text","content":"hi"}"#;
    assert!(matches!(
        Classifier::mapped().classify("POST", CHAT, Some(body.as_bytes())),
        Classification::Write(_)
    ));
    let pause = json!({
        "requestId": "r-write",
        "request": {"method": "POST", "url": CHAT, "postData": body, "headers": {}},
        "resourceType": "Fetch",
    });
    fake.event(Some("S1"), "Fetch.requestPaused", &pause).unwrap();
    // the write's membership read is running when the fence closes
    tokio::time::timeout(Duration::from_secs(5), asked.notified())
        .await
        .unwrap();
    assert!(decisions.denials().is_empty());

    armed.close(Duration::from_millis(100)).await.unwrap();

    let causes: Vec<DenyCause> = decisions.denials().iter().map(|denial| denial.cause).collect();
    assert_eq!(
        causes,
        [DenyCause::Fenced(DenyReason::Outsiders { count: 1 })],
        "the deny that landed after the browser went down is kept"
    );
    assert_eq!(decisions.undecided(), 0);
    // the connection ending after the close isn't a lost one
    fake.hang_up().unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let failed = decisions.verdict().unwrap_err();
    assert!(!failed.connection_lost, "{failed}");
}

fn hang_up_on_close(method: &str, session: Option<&str>) -> bool {
    method == "Browser.close" && session.is_none()
}

#[tokio::test]
async fn a_connection_lost_while_closing_kills_the_browser_at_once() {
    let fake = FakeBrowser::start(hang_up_on_close).await.unwrap();
    let (armed, _lab) = arm_fake(&fake, &[]).await.unwrap();
    let helper = fake.helper().unwrap();
    let started = std::time::Instant::now();

    let closed = armed.close(Duration::from_secs(30)).await;

    assert!(matches!(closed, Err(BrowserError::CloseUnconfirmed(_))), "{closed:?}");
    // not left running unfenced for the grace period
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    assert!(
        process::gone(helper).await,
        "the browser outlived its fence's connection"
    );
}

#[tokio::test]
async fn a_lost_connection_fails_the_run_and_kills_the_browser() {
    let fake = FakeBrowser::start(refuse_nothing).await.unwrap();
    let (armed, _lab) = arm_fake(&fake, &[]).await.unwrap();
    let helper = fake.helper().unwrap();
    armed.verdict().unwrap();

    fake.hang_up().unwrap();

    assert!(
        process::gone(helper).await,
        "the browser outlived its fence's connection"
    );
    let failed = armed.verdict().unwrap_err();
    assert!(failed.connection_lost);
    assert!(failed.denials.is_empty() && failed.failures.is_empty());
    assert!(failed.to_string().contains("lost its connection"), "{failed}");
    assert!(armed.open_page(Duration::from_secs(5)).await.is_err());
}

#[tokio::test]
async fn a_watch_ends_with_the_connection() {
    let fake = FakeBrowser::start(refuse_nothing).await.unwrap();
    let (armed, _lab) = arm_fake(&fake, &[]).await.unwrap();
    let mut events = armed.watch();
    fake.event(Some("S1"), "Page.frameStoppedLoading", &json!({"frameId": "F"}))
        .unwrap();

    fake.hang_up().unwrap();

    // the event that came before the loss is still delivered, then the channel ends, while the fence is still alive
    let wait = Duration::from_secs(5);
    let first = tokio::time::timeout(wait, events.recv()).await.unwrap().unwrap();
    assert_eq!(first.method, "Page.frameStoppedLoading");
    let end = tokio::time::timeout(wait, events.recv()).await;
    assert!(matches!(end, Ok(None)), "the watch outlived the connection");
    // and a watch asked for afterwards is closed from the start
    let mut late = armed.watch();
    assert!(late.recv().await.is_none());
    assert!(armed.verdict().unwrap_err().connection_lost);
}

#[tokio::test]
async fn a_paused_request_is_answered_on_the_session_it_paused_on() {
    let fake = FakeBrowser::start(refuse_nothing).await.unwrap();
    let (armed, lab) = arm_fake(&fake, &[]).await.unwrap();
    let pause = |id: &str, method: &str| json!({"requestId": id, "request": {"method": method, "url": CHAT, "headers": {}}, "resourceType": "Fetch"});

    fake.event(Some("S1"), "Fetch.requestPaused", &pause("r-post", "POST"))
        .unwrap();
    fake.event(Some("S1"), "Fetch.requestPaused", &pause("r-get", "GET"))
        .unwrap();
    fake.event(None, "Fetch.requestPaused", &pause("r-browser", "DELETE"))
        .unwrap();
    fake.event(
        Some("S1"),
        "Fetch.requestPaused",
        &json!({"requestId": "r-bad", "request": {}}),
    )
    .unwrap();

    let answer = |id: &'static str| move |command: &Value| command["params"]["requestId"] == id;
    let post = fake.sent("the POST's answer", answer("r-post")).await.unwrap();
    assert_eq!(post["method"], "Fetch.failRequest");
    assert_eq!(post["params"]["errorReason"], "BlockedByClient");
    assert_eq!(post["sessionId"], "S1");
    let get = fake.sent("the GET's answer", answer("r-get")).await.unwrap();
    assert_eq!(get["method"], "Fetch.continueRequest");
    assert_eq!(get["sessionId"], "S1");
    let browser = fake.sent("the browser's answer", answer("r-browser")).await.unwrap();
    assert_eq!(browser["method"], "Fetch.failRequest");
    assert!(browser.get("sessionId").is_none());
    let bad = fake.sent("the malformed one's answer", answer("r-bad")).await.unwrap();
    assert_eq!(bad["method"], "Fetch.failRequest");

    let causes: Vec<DenyCause> = armed.denials().iter().map(|denial| denial.cause).collect();
    assert_eq!(causes.len(), 3, "{causes:?}");
    assert_eq!(causes.iter().filter(|cause| **cause == DenyCause::Malformed).count(), 1);
    assert_eq!(log_entries(&lab.log_path).unwrap().len(), 3, "every deny is logged");
    assert!(armed.verdict().is_err());
}

// --- enforcement in a real browser ---

const STEP: Duration = Duration::from_secs(15);

/// The local pages, under `/pages/`.
const PAGES: [(&str, &str, &str); 10] = [
    ("fence.html", include_str!("fixtures/pages/fence.html"), "text/html"),
    (
        "dedicated-worker.js",
        include_str!("fixtures/pages/dedicated-worker.js"),
        "text/javascript",
    ),
    (
        "shared-worker.js",
        include_str!("fixtures/pages/shared-worker.js"),
        "text/javascript",
    ),
    (
        "service-worker.js",
        include_str!("fixtures/pages/service-worker.js"),
        "text/javascript",
    ),
    (
        "pagehide.html",
        include_str!("fixtures/pages/pagehide.html"),
        "text/html",
    ),
    ("blank.html", include_str!("fixtures/pages/blank.html"), "text/html"),
    ("own.html", include_str!("fixtures/pages/own.html"), "text/html"),
    (
        "answering-worker.js",
        include_str!("fixtures/pages/answering-worker.js"),
        "text/javascript",
    ),
    (
        "service-worker-probe.html",
        include_str!("fixtures/pages/service-worker-probe.html"),
        "text/html",
    ),
    ("favicon.ico", "", "image/x-icon"),
];

/// The mock server: the pages, a sink that answers any POST, a GET endpoint, a stand-in own endpoint, and `/probe`.
async fn serve_pages() -> MockServer {
    let server = MockServer::start().await;
    for (name, body, content_type) in PAGES {
        Mock::given(method("GET"))
            .and(path(format!("/pages/{name}")))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, content_type))
            .mount(&server)
            .await;
    }
    for (verb, route, body) in [
        ("POST", "^/sink/.*$", "sunk"),
        ("GET", "^/data$", "data"),
        ("POST", "^/own/.*$", "own"),
        ("GET", "^/probe$", "from-network"),
    ] {
        Mock::given(method(verb))
            .and(path_regex(route))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
    }
    server
}

/// The requests the mock server received for paths starting with `prefix`, as `METHOD path`.
async fn received(server: &MockServer, prefix: &str) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|request| request.url.path().starts_with(prefix))
        .map(|request| format!("{} {}", request.method, request.url.path()))
        .collect()
}

async fn evaluate(cdp: &Cdp, session: &SessionId, expression: &str) -> TestResult<Value> {
    let params = json!({"expression": expression, "returnByValue": true, "awaitPromise": true});
    let result = cdp.call(Some(session), "Runtime.evaluate", params).await?;
    if let Some(exception) = result.get("exceptionDetails") {
        return Err(format!("{expression} threw: {exception}").into());
    }
    Ok(result["result"]["value"].clone())
}

/// Polls `expression` until it gives something other than `null`.
async fn evaluate_until(cdp: &Cdp, session: &SessionId, expression: &str) -> TestResult<Value> {
    let poll = async {
        loop {
            let value = evaluate(cdp, session, expression).await?;
            if !value.is_null() {
                return Ok(value);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    tokio::time::timeout(STEP, poll)
        .await
        .map_err(|_| format!("{expression} stayed null for {STEP:?}"))?
}

async fn navigate(cdp: &Cdp, page: &ArmedTarget, url: &str) -> TestResult {
    let navigated = cdp
        .call(Some(&page.session), "Page.navigate", json!({ "url": url }))
        .await?;
    if let Some(error) = navigated.get("errorText") {
        return Err(format!("navigating to {url}: {error}").into());
    }
    Ok(())
}

/// Waits until `wanted` holds for the run's denials.
async fn denials_until(
    armed: &ArmedBrowser,
    what: &str,
    wanted: impl Fn(&[String]) -> bool,
) -> TestResult<Vec<String>> {
    let poll = async {
        loop {
            let denied: Vec<String> = armed
                .denials()
                .iter()
                .map(|denial| format!("{} {}", denial.method, denial.url))
                .collect();
            if wanted(&denied) {
                return denied;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    Ok(tokio::time::timeout(STEP, poll)
        .await
        .map_err(|_| format!("no {what} within {STEP:?}: {:?}", armed.denials()))?)
}

/// One armed run on a throwaway profile.
struct Run {
    armed: ArmedBrowser,
    lab: Lab,
    _profile: tempfile::TempDir,
}

async fn start_run(chrome: &Path, server: &MockServer, classifier: Classifier) -> TestResult<Run> {
    let profile = tempfile::tempdir()?;
    let browser = Browser::launch(chrome, &LaunchOptions::new(profile.path().join("p")), STEP).await?;
    let lab = lab()?;
    let options = ArmOptions {
        service_worker_origins: vec![server.uri()],
    };
    let armed = ArmedBrowser::arm(browser, http_fence(&lab, classifier, &[]), ws_fence(&lab)?, &options).await?;
    Ok(Run {
        armed,
        lab,
        _profile: profile,
    })
}

impl Run {
    async fn finish(self) -> TestResult {
        self.armed.close(Duration::from_secs(10)).await?;
        Ok(())
    }
}

/// Runs `body` on an armed browser, then closes the browser whatever `body` returned. Without Chrome it does nothing.
async fn with_run<F>(server: &MockServer, classifier: Classifier, body: F) -> TestResult
where
    F: AsyncFnOnce(&Run) -> TestResult,
{
    let Some(chrome) = common::chrome_or_skip() else {
        return Ok(());
    };
    let run = start_run(&chrome, server, classifier).await?;
    let outcome = tokio::time::timeout(Duration::from_secs(90), body(&run)).await;
    run.finish().await?;
    outcome.map_err(|_| "the run took over 90 s")?
}

const SOURCES: [&str; 4] = ["page", "dedicated-worker", "shared-worker", "service-worker"];

/// The page, a dedicated, a shared and a service worker each POST; all are denied and see a network error.
async fn posts_from_every_scope_are_denied(run: &Run, server: &MockServer) -> TestResult {
    let cdp = run.armed.cdp();
    let page = run.armed.open_page(STEP).await?;
    navigate(cdp, &page, &format!("{}/pages/fence.html", server.uri())).await?;
    let results = evaluate_until(
        cdp,
        &page.session,
        "Object.keys(window.results).length === 4 ? window.results : null",
    )
    .await?;
    for source in SOURCES {
        assert_eq!(results[source], "error TypeError", "{source}: {results}");
    }
    let kinds: BTreeSet<String> = run
        .armed
        .targets()
        .iter()
        .map(|target| format!("{:?}", target.kind))
        .collect();
    for kind in ["Page", "Worker", "SharedWorker", "ServiceWorker"] {
        assert!(kinds.contains(kind), "no armed {kind}: {kinds:?}");
    }
    Ok(())
}

/// What the pagehide scenarios post to: a beacon and a keepalive fetch, tagged by how the page goes away.
const PAGEHIDE_SINKS: [&str; 4] = [
    "beacon-navigate",
    "keepalive-navigate",
    "beacon-close",
    "keepalive-close",
];

/// A beacon and a keepalive fetch fired from `pagehide`, once when the page navigates away and once when its tab
/// closes: all denied. (A request paused on a tab that closes may be denied again by the browser-level layer, so
/// each is counted once at least.)
async fn pagehide_requests_are_denied(run: &Run, server: &MockServer) -> TestResult {
    let cdp = run.armed.cdp();
    let pagehide = format!("{}/pages/pagehide.html", server.uri());
    let leaving = run.armed.open_page(STEP).await?;
    navigate(cdp, &leaving, &format!("{pagehide}#navigate")).await?;
    evaluate_until(cdp, &leaving.session, "window.ready || null").await?;
    navigate(cdp, &leaving, &format!("{}/pages/blank.html", server.uri())).await?;

    let closing = run.armed.open_page(STEP).await?;
    navigate(cdp, &closing, &format!("{pagehide}#close")).await?;
    evaluate_until(cdp, &closing.session, "window.ready || null").await?;
    cdp.call(None, "Target.closeTarget", json!({"targetId": closing.target_id}))
        .await?;

    denials_until(&run.armed, "every pagehide request", |denied| {
        PAGEHIDE_SINKS
            .iter()
            .all(|sink| denied.iter().any(|line| line.ends_with(&format!("/sink/{sink}"))))
    })
    .await?;
    Ok(())
}

#[tokio::test]
async fn every_mutating_request_is_denied_before_it_leaves_the_browser() {
    let server = serve_pages().await;
    with_run(&server, Classifier::seeded(), async |run| {
        posts_from_every_scope_are_denied(run, &server).await?;
        pagehide_requests_are_denied(run, &server).await?;

        assert_eq!(
            received(&server, "/sink/").await,
            Vec::<String>::new(),
            "a denied request got out"
        );
        let denied: BTreeSet<String> = run
            .armed
            .denials()
            .iter()
            .inspect(|denial| assert_eq!(denial.cause, DenyCause::Unclassified, "{denial:?}"))
            .map(|denial| format!("{} {}", denial.method, denial.url))
            .collect();
        let sinks: Vec<&str> = SOURCES.iter().chain(&PAGEHIDE_SINKS).copied().collect();
        let expected: BTreeSet<String> = sinks
            .iter()
            .map(|sink| format!("POST {}/sink/{sink}", server.uri()))
            .collect();
        assert_eq!(denied, expected);

        let logged: BTreeSet<String> = log_entries(&run.lab.log_path)?
            .iter()
            .filter(|entry| entry["verdict"] == "deny")
            .filter_map(|entry| entry["url"].as_str().map(str::to_owned))
            .collect();
        let expected_urls: BTreeSet<String> = sinks
            .iter()
            .map(|sink| format!("{}/sink/{sink}", server.uri()))
            .collect();
        assert_eq!(logged, expected_urls, "every deny is in fence.log");
        assert!(run.armed.failures().is_empty(), "{:?}", run.armed.failures());
        assert!(run.armed.verdict().is_err(), "denies fail the run");
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_get_passes_and_an_own_post_reaches_the_server() {
    let server = serve_pages().await;
    let classifier = Classifier::seeded().with_own(OwnRule::new(OwnKind::Authz, "POST", &server.uri(), "/own/authz"));
    with_run(&server, classifier, async |run| {
        let cdp = run.armed.cdp();
        let page = run.armed.open_page(STEP).await?;
        navigate(cdp, &page, &format!("{}/pages/own.html", server.uri())).await?;
        let results = evaluate_until(cdp, &page.session, "window.results.other ?? null").await?;
        assert_eq!(results, "error TypeError");
        let results = evaluate(cdp, &page.session, "window.results").await?;
        assert_eq!(results["get"], "200 data");
        assert_eq!(results["own"], "200 own");

        assert_eq!(received(&server, "/data").await, ["GET /data"]);
        assert_eq!(
            received(&server, "/own/").await,
            ["POST /own/authz"],
            "only the own POST got out"
        );
        let denied: Vec<String> = run.armed.denials().iter().map(|denial| denial.url.clone()).collect();
        assert_eq!(denied, [format!("{}/own/other", server.uri())]);
        let own_entries = log_entries(&run.lab.log_path)?
            .into_iter()
            .filter(|entry| entry["own"] == "authz")
            .count();
        assert!(own_entries >= 1, "the own request is in fence.log");
        Ok(())
    })
    .await
    .unwrap();
}

/// What an earlier, unfenced session leaves behind on the origin: a service worker that answers `/probe` and a filled
/// cache. Done over a connection of its own, which is gone before the run arms.
async fn leave_a_service_worker_behind(browser: &Browser, probe_page: &str) -> TestResult {
    let (cdp, _events) = Cdp::connect(&browser.endpoint().ws_url()).await?;
    let target = cdp
        .call(None, "Target.createTarget", json!({"url": probe_page}))
        .await?;
    let attach = json!({"targetId": target["targetId"], "flatten": true});
    let session = SessionId::new(
        cdp.call(None, "Target.attachToTarget", attach).await?["sessionId"]
            .as_str()
            .ok_or("no sessionId")?,
    );
    evaluate_until(&cdp, &session, "typeof window.setUp === 'function' || null").await?;
    assert_eq!(evaluate(&cdp, &session, "window.setUp()").await?, "controlled");
    assert_eq!(
        evaluate(&cdp, &session, "window.probe()").await?,
        "from-service-worker",
        "unfenced, the service worker answers"
    );
    assert_eq!(
        evaluate(&cdp, &session, "window.leftovers()").await?,
        json!({"registrations": 1, "caches": ["lab"]})
    );
    cdp.call(None, "Target.closeTarget", json!({"targetId": target["targetId"]}))
        .await?;
    Ok(())
}

#[tokio::test]
async fn service_workers_are_neutralized_at_run_start_and_bypassed_during_it() {
    let Some(chrome) = common::chrome_or_skip() else {
        return;
    };
    let server = serve_pages().await;
    let probe_page = format!("{}/pages/service-worker-probe.html", server.uri());
    let profile = tempfile::tempdir().unwrap();
    let browser = Browser::launch(&chrome, &LaunchOptions::new(profile.path().join("p")), STEP)
        .await
        .unwrap();

    // the browser moves into the run: an error or the timeout drops it (or the fence owning it), which kills it
    let outcome = tokio::time::timeout(Duration::from_secs(90), async move {
        leave_a_service_worker_behind(&browser, &probe_page).await?;

        let lab = lab()?;
        let options = ArmOptions {
            service_worker_origins: vec![server.uri()],
        };
        let armed = ArmedBrowser::arm(
            browser,
            http_fence(&lab, Classifier::seeded(), &[]),
            ws_fence(&lab)?,
            &options,
        )
        .await?;
        let cdp = armed.cdp().clone();
        let page = armed.open_page(STEP).await?;
        navigate(&cdp, &page, &probe_page).await?;
        evaluate_until(&cdp, &page.session, "typeof window.leftovers === 'function' || null").await?;
        assert_eq!(
            evaluate(&cdp, &page.session, "window.leftovers()").await?,
            json!({"registrations": 0, "caches": []}),
            "the run starts with no service worker and no cache"
        );

        // a service worker the page registers during the run still controls it, but never answers its requests
        assert_eq!(evaluate(&cdp, &page.session, "window.setUp()").await?, "controlled");
        assert_eq!(evaluate(&cdp, &page.session, "window.probe()").await?, "from-network");
        armed.verdict()?;
        armed.close(Duration::from_secs(10)).await?;
        TestResult::Ok(())
    })
    .await;
    outcome.expect("the run took over 90 s").unwrap();
}

// --- reports the network service uploads by itself ---

/// A page that sets a reporting endpoint and a CSP whose violation (its own image) is reported there.
const REPORTING_PAGE: &str = r#"<!doctype html><title>reporting</title><img src="/blocked.png">"#;

/// A local HTTPS server with a fresh self-signed certificate (Chrome uploads reports only over a secure connection;
/// the test browser ignores certificate errors). It serves [`REPORTING_PAGE`] at `/page`, answers anything else with
/// an empty 200, and records every request as `METHOD path`.
struct ReportingServer {
    origin: String,
    received: Arc<Mutex<Vec<String>>>,
}

impl ReportingServer {
    async fn start() -> TestResult<Self> {
        let certified = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned()])?;
        let key = PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der());
        let provider = Arc::new(tokio_rustls::rustls::crypto::aws_lc_rs::default_provider());
        let config = tokio_rustls::rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_single_cert(vec![certified.cert.der().clone()], key.into())?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("https://127.0.0.1:{}", listener.local_addr()?.port());
        let received = Arc::new(Mutex::new(Vec::new()));
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        tokio::spawn(accept_reporting(
            listener,
            acceptor,
            origin.clone(),
            Arc::clone(&received),
        ));
        Ok(Self { origin, received })
    }

    fn received(&self) -> Vec<String> {
        self.received.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

async fn accept_reporting(
    listener: tokio::net::TcpListener,
    acceptor: tokio_rustls::TlsAcceptor,
    origin: String,
    received: Arc<Mutex<Vec<String>>>,
) -> SendResult<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let (acceptor, origin, received) = (acceptor.clone(), origin.clone(), Arc::clone(&received));
        tokio::spawn(async move {
            // a connection that fails the handshake or breaks off is just over
            if let Ok(stream) = acceptor.accept(stream).await {
                let _ = answer_reporting(stream, &origin, &received).await;
            }
        });
    }
}

/// Answers the HTTP/1.1 requests on one connection.
async fn answer_reporting(
    stream: impl AsyncRead + AsyncWrite + Unpin,
    origin: &str,
    received: &Mutex<Vec<String>>,
) -> SendResult<()> {
    let mut stream = BufReader::new(stream);
    while let Some((method, path)) = read_request(&mut stream).await? {
        received
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(format!("{method} {path}"));
        let response = if path == "/page" {
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nReporting-Endpoints: main=\"{origin}/reports\"\r\n\
                 Content-Security-Policy: img-src 'none'; report-to main\r\nContent-Length: {}\r\n\r\n{REPORTING_PAGE}",
                REPORTING_PAGE.len()
            )
        } else {
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_owned()
        };
        stream.get_mut().write_all(response.as_bytes()).await?;
    }
    Ok(())
}

/// Reads one request (its body skipped) and gives its method and path; `None` once the connection is closed.
async fn read_request(stream: &mut (impl AsyncBufRead + Unpin)) -> SendResult<Option<(String, String)>> {
    let mut line = String::new();
    if stream.read_line(&mut line).await? == 0 {
        return Ok(None);
    }
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or_default(), parts.next().unwrap_or_default());
    let mut length = 0;
    loop {
        let mut header = String::new();
        stream.read_line(&mut header).await?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse()?;
        }
    }
    stream.read_exact(&mut vec![0; length]).await?;
    Ok(Some((method.to_owned(), path.to_owned())))
}

/// How long a report gets to arrive. With `--short-reporting-delay` Chrome uploads within a second.
const REPORT_WINDOW: Duration = Duration::from_secs(5);

/// The test's Chrome: the lab flags plus `--ignore-certificate-errors` and `--short-reporting-delay`. With
/// `reporting_on`, the lab's `--disable-features` is left out.
fn reporting_chrome(chrome: &Path, dir: &Path, reporting_on: bool) -> TestResult<PathBuf> {
    let skipped = if reporting_on { "--disable-features=*" } else { "--none" };
    let script = format!(
        "#!/bin/sh\n\
         for arg; do shift; case \"$arg\" in {skipped}) ;; *) set -- \"$@\" \"$arg\";; esac; done\n\
         exec '{}' \"$@\" --ignore-certificate-errors --short-reporting-delay\n",
        chrome.display()
    );
    let wrapper = dir.join("chrome");
    std::fs::write(&wrapper, script)?;
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))?;
    Ok(wrapper)
}

/// Loads the reporting page in an armed browser and gives what reached the server within [`REPORT_WINDOW`] (or until
/// a report did) and what the fence denied.
async fn reports_reaching(chrome: &Path, reporting_on: bool) -> TestResult<(Vec<String>, Vec<String>)> {
    let server = ReportingServer::start().await?;
    let profile = tempfile::tempdir()?;
    let wrapper = reporting_chrome(chrome, profile.path(), reporting_on)?;
    let browser = Browser::launch(&wrapper, &LaunchOptions::new(profile.path().join("p")), STEP).await?;
    let lab = lab()?;
    let options = ArmOptions {
        service_worker_origins: vec![],
    };
    let armed = ArmedBrowser::arm(
        browser,
        http_fence(&lab, Classifier::seeded(), &[]),
        ws_fence(&lab)?,
        &options,
    )
    .await?;
    let page = armed.open_page(STEP).await?;
    navigate(armed.cdp(), &page, &format!("{}/page", server.origin)).await?;
    let reported = async {
        while !server.received().iter().any(|request| request.starts_with("POST ")) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    let _ = tokio::time::timeout(REPORT_WINDOW, reported).await;
    let denied = armed
        .denials()
        .iter()
        .map(|denial| format!("{} {}", denial.method, denial.url))
        .collect();
    armed.close(Duration::from_secs(10)).await?;
    Ok((server.received(), denied))
}

#[tokio::test]
async fn reports_the_network_service_uploads_by_itself_are_off() {
    let Some(chrome) = common::chrome_or_skip() else {
        return;
    };
    let run = |reporting_on| tokio::time::timeout(Duration::from_secs(60), reports_reaching(&chrome, reporting_on));

    // with the Reporting API on, the CSP report reaches the server and the fence never sees it
    let (received, denied) = run(true).await.expect("the run took over 60 s").unwrap();
    assert!(
        received.contains(&"POST /reports".to_owned()),
        "no report even with Reporting on, so the test proves nothing: {received:?}"
    );
    assert_eq!(denied, Vec::<String>::new(), "the report went past the fence");

    // a lab browser sends none
    let (received, denied) = run(false).await.expect("the run took over 60 s").unwrap();
    assert!(received.contains(&"GET /page".to_owned()), "{received:?}");
    let posted: Vec<&String> = received.iter().filter(|request| request.starts_with("POST ")).collect();
    assert_eq!(posted, Vec::<&String>::new(), "a report got out");
    assert_eq!(denied, Vec::<String>::new());
}
