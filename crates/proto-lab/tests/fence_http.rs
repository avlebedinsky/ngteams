//! The browser fence's HTTP layer: the classifier, the decision on one paused request, the arming of the browser and
//! of every target (against a scripted fake browser), and, with Chrome installed, enforcement on local pages: a denied
//! request never reaches the mock server and the page sees a network error, wherever it comes from.
//!
//! The Chrome tests run headless on throwaway user-data-dirs against a local mock server; see `common` for how Chrome
//! is found.

mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use proto_lab::arming::{ArmOptions, ArmedBrowser, ArmedTarget, Step, TEAMS_ORIGINS, TargetKind};
use proto_lab::browser::{Browser, LaunchOptions};
use proto_lab::cdp::{Cdp, SessionId};
use proto_lab::classify::{Classification, Classifier, OwnKind, OwnRule, classify, is_mutating};
use proto_lab::fence_http::{DenyCause, HttpFence, HttpVerdict, MalformedPause, PausedRequest};
use serde_json::{Value, json};
use teams_fence::{
    DenyReason, Fence, FenceLog, Membership, MembershipFuture, MembershipSource, Mri, Roster, Scope, Target, WriteKind,
};
use tokio::sync::mpsc;
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
    }
}

#[tokio::test]
async fn an_unclassified_post_is_denied_kept_and_logged_without_its_query() {
    let lab = lab().unwrap();
    let fence = http_fence(&lab, Classifier::seeded(), &[LAB_A]);

    let verdict = fence.decide(&paused("POST", &format!("{CHAT}?secret=planted"))).await;

    assert_eq!(verdict, HttpVerdict::Deny(DenyCause::Unclassified));
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

    assert_eq!(fence.decide(&paused("GET", CHAT)).await, HttpVerdict::Allow);
    assert_eq!(fence.decide(&paused("POST", TOKEN)).await, HttpVerdict::Allow);

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

    assert_eq!(verdict, HttpVerdict::Deny(DenyCause::LogUnavailable));
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
    assert_eq!(verdict, HttpVerdict::Allow);

    let with_coworker = http_fence(&lab, Classifier::seeded(), &[LAB_A, COWORKER]);
    let verdict = with_coworker.judge(Classification::Write(target()), "POST", CHAT).await;
    assert_eq!(
        verdict,
        HttpVerdict::Deny(DenyCause::Fenced(DenyReason::Outsiders { count: 1 }))
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
/// whatever events the test pushes.
struct FakeBrowser {
    url: String,
    commands: Arc<Mutex<Vec<Value>>>,
    push: mpsc::UnboundedSender<Value>,
}

impl FakeBrowser {
    async fn start(refuse: fn(&str, Option<&str>) -> bool) -> TestResult<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("ws://{}/devtools/browser/fake", listener.local_addr()?);
        let commands = Arc::new(Mutex::new(Vec::new()));
        let (push, pushed) = mpsc::unbounded_channel();
        tokio::spawn(serve_fake(listener, Arc::clone(&commands), pushed, refuse));
        Ok(Self { url, commands, push })
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
                let reply = fake_reply(&command, refuse);
                let created = command["method"] == "Target.createTarget";
                commands.lock().unwrap_or_else(PoisonError::into_inner).push(command);
                socket.send(Message::text(reply.to_string())).await?;
                if created {
                    // a new tab attaches through the browser's auto-attach
                    let event = json!({"method": "Target.attachedToTarget", "params": attached("S-new", "T-new", "page")});
                    socket.send(Message::text(event.to_string())).await?;
                }
            }
            event = pushed.recv() => {
                let Some(event) = event else { return Ok(()) };
                socket.send(Message::text(event.to_string())).await?;
            }
        }
    }
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
        "Target.closeTarget" => json!({"success": true}),
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
    let (cdp, events) = Cdp::connect(&fake.url).await?;
    let options = ArmOptions {
        service_worker_origins: origins.iter().map(|&origin| origin.to_owned()).collect(),
    };
    let armed = ArmedBrowser::arm(cdp, events, http_fence(&lab, Classifier::seeded(), &[]), &options).await?;
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
            "Target.setAutoAttach",
        ]
    };
    let cases = [
        (None, "S-page", "page", page_like()),
        (Some("S-page"), "S-frame", "iframe", page_like()),
        (Some("S-page"), "S-worker", "worker", vec!["Target.setAutoAttach"]),
        (
            None,
            "S-shared",
            "shared_worker",
            vec!["Fetch.enable", "Target.setAutoAttach"],
        ),
        (
            Some("S-page"),
            "S-service",
            "service_worker",
            vec!["Fetch.enable", "Target.setAutoAttach"],
        ),
        (None, "S-ui", "browser_ui", vec!["Fetch.enable", "Target.setAutoAttach"]),
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

#[tokio::test]
async fn a_session_gone_before_it_is_armed_is_dropped_without_closing_its_target() {
    let fake = FakeBrowser::start(refuse_nothing).await.unwrap();
    let (armed, _lab) = arm_fake(&fake, &[]).await.unwrap();

    // a service worker attached through a page whose tab closes while the worker is being armed
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
        command["sessionId"] == "S-gone" && command["method"] == "Target.setAutoAttach"
    })
    .await
    .unwrap();
    assert_eq!(
        fake.methods_on(Some("S-gone")),
        ["Fetch.enable", "Target.setAutoAttach"],
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
    browser: Browser,
    armed: ArmedBrowser,
    lab: Lab,
    _profile: tempfile::TempDir,
}

async fn start_run(chrome: &Path, server: &MockServer, classifier: Classifier) -> TestResult<Run> {
    let profile = tempfile::tempdir()?;
    let browser = Browser::launch(chrome, &LaunchOptions::new(profile.path().join("p")), STEP).await?;
    let lab = lab()?;
    let (cdp, events) = Cdp::connect(&browser.endpoint().ws_url()).await?;
    let options = ArmOptions {
        service_worker_origins: vec![server.uri()],
    };
    let armed = ArmedBrowser::arm(cdp, events, http_fence(&lab, classifier, &[]), &options).await?;
    Ok(Run {
        browser,
        armed,
        lab,
        _profile: profile,
    })
}

impl Run {
    async fn finish(self) -> TestResult {
        // closing is best effort: the connection may drop before the reply
        let _ = self.armed.cdp().call(None, "Browser.close", Value::Null).await;
        drop(self.armed);
        self.browser.wait_or_kill(Duration::from_secs(10)).await?;
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

    let outcome = tokio::time::timeout(Duration::from_secs(90), async {
        leave_a_service_worker_behind(&browser, &probe_page).await?;

        let lab = lab()?;
        let (cdp, events) = Cdp::connect(&browser.endpoint().ws_url()).await?;
        let options = ArmOptions {
            service_worker_origins: vec![server.uri()],
        };
        let armed = ArmedBrowser::arm(cdp, events, http_fence(&lab, Classifier::seeded(), &[]), &options).await?;
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
        let _ = cdp.call(None, "Browser.close", Value::Null).await;
        TestResult::Ok(())
    })
    .await;
    browser.wait_or_kill(Duration::from_secs(10)).await.unwrap();
    outcome.expect("the run took over 90 s").unwrap();
}
