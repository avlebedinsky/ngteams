//! The scenario steps (`proto_lab::scenario::dsl`) against a local page in a throwaway armed browser: waiting for a
//! `data-tid` element, for a request matching a pattern, typing and pressing Enter; and a wait that runs out fails
//! with an error naming its step. Also what a scenario expects of the fence (`DenyPolicy`) and `ab-chat`'s helpers.

mod common;

use std::sync::Arc;
use std::time::Duration;

use proto_lab::arming::{ArmOptions, ArmedBrowser};
use proto_lab::browser::{Browser, LaunchOptions};
use proto_lab::classify::{Classifier, OwnKind, OwnRule};
use proto_lab::config::Alias;
use proto_lab::fence_http::{Denial, DenyCause, HttpFence};
use proto_lab::fence_ws::{FrameClassifier, FrameDenial, GuardScript, WsFence};
use proto_lab::scenario::ab_chat::{MESSAGE_POST, chat_link, message_text, probe_text};
use proto_lab::scenario::dsl::{Driver, RequestPattern, Selector, StepError};
use proto_lab::scenario::{AccountRun, DenyPolicy, ScenarioName, ScenarioReport, TELEMETRY_HOSTS};
use proto_lab::session::FenceReport;
use teams_fence::{FailClosed, Fence, FenceLog, MembershipSource, Roster};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// A page that adds a `data-tid` element after 400 ms, fetches `/api/ping` at once and `/api/late` after 600 ms, and
/// echoes what is typed into its box when Enter is pressed. Its `hang` button stalls the page for good, a moment after
/// the click.
const PAGE: &str = r#"<!doctype html><html><body>
<div id="out"></div>
<button data-tid="hang" onclick="setTimeout(() => { for (;;) {} }, 50)">hang</button>
<div contenteditable="true" role="textbox" data-tid="box" id="box"></div>
<script>
fetch('/api/ping?n=1');
setTimeout(() => { const e = document.createElement('div'); e.dataset.tid = 'chat-list'; e.textContent = 'chats'; document.body.append(e); }, 400);
setTimeout(() => fetch('/api/late'), 600);
document.getElementById('box').addEventListener('keydown', (event) => {
  if (event.key !== 'Enter') return;
  event.preventDefault();
  const box = document.getElementById('box');
  document.getElementById('out').textContent = 'sent: ' + box.textContent;
  fetch('/api/send', {method: 'POST', body: box.textContent});
  box.textContent = '';
});
</script></body></html>"#;

struct Run {
    armed: ArmedBrowser,
    driver: Driver,
    server: MockServer,
    _dir: tempfile::TempDir,
}

async fn serve() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/page"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(PAGE, "text/html"))
        .mount(&server)
        .await;
    Mock::given(path("/api/ping"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    Mock::given(path("/api/late"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    Mock::given(path("/api/send"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    server
}

/// Runs `body` with a driver on a tab of an armed throwaway browser, opened on the page; closes the browser after.
/// Without Chrome it does nothing.
async fn with_page<F>(body: F) -> TestResult
where
    F: AsyncFnOnce(&mut Run) -> TestResult,
{
    let Some(chrome) = common::chrome_or_skip() else {
        return Ok(());
    };
    let server = serve().await;
    let dir = tempfile::tempdir()?;
    let browser = Browser::launch(
        &chrome,
        &LaunchOptions::new(dir.path().join("profile")),
        Duration::from_secs(30),
    )
    .await?;
    let fence = Arc::new(Fence::new(
        Roster::default(),
        FenceLog::open(&dir.path().join("fence.log"))?,
    ));
    let members: Arc<dyn MembershipSource + Send> = Arc::new(FailClosed);
    let classifier = Classifier::seeded().with_own(OwnRule::new(OwnKind::Authz, "POST", &server.uri(), "/api/send"));
    let http = HttpFence::new(classifier, Arc::clone(&fence), Arc::clone(&members));
    let ws = WsFence::new(FrameClassifier::seeded(), fence, members, GuardScript::generate()?);
    let options = ArmOptions {
        service_worker_origins: vec![server.uri()],
    };
    let armed = ArmedBrowser::arm(browser, http, ws, &options).await?;
    let page = armed.open_page(Duration::from_secs(15)).await?.session;
    let driver = Driver::new(armed.cdp().clone(), page, armed.watch());
    let mut run = Run {
        armed,
        driver,
        server,
        _dir: dir,
    };
    let url = format!("{}/page", run.server.uri());
    run.driver.navigate("open the page", &url).await?;
    let outcome = tokio::time::timeout(Duration::from_secs(60), body(&mut run)).await;
    run.armed.close(Duration::from_secs(10)).await?;
    outcome.map_err(|_| "the run took over 60 s")?
}

#[tokio::test]
async fn a_data_tid_element_is_waited_for() -> TestResult {
    with_page(async |run| {
        run.driver
            .wait_for_tid("open the chat list", "chat-list", Duration::from_secs(10))
            .await?;
        run.driver
            .wait_for(
                "the compose box",
                Selector::Css(r#"[contenteditable="true"][role="textbox"]"#),
                Duration::from_secs(1),
            )
            .await?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn a_request_is_waited_for_even_when_it_came_first_and_matched_once() -> TestResult {
    with_page(async |run| {
        // /api/ping went out as the page loaded, before this wait
        run.driver
            .wait_for_tid("the page ran", "chat-list", Duration::from_secs(10))
            .await?;
        let ping = RequestPattern::new(Some("GET"), "/api/ping")?;
        let seen = run
            .driver
            .wait_for_request("the ping", &ping, Duration::from_secs(5))
            .await?;
        assert!(seen.url.ends_with("/api/ping?n=1"), "{seen:?}");
        assert_eq!(seen.method, "GET");
        let late = RequestPattern::new(None, "/api/late$")?;
        let late = run
            .driver
            .wait_for_request("the late one", &late, Duration::from_secs(5))
            .await?;
        // it was answered, so waiting for it to fail says so
        let error = match run
            .driver
            .wait_for_failure("the late one fails", &late, Duration::from_secs(5))
            .await
        {
            Ok(failure) => return Err(format!("an answered request failed: {failure}").into()),
            Err(error) => error,
        };
        assert!(error.to_string().contains("got a response (204)"), "{error}");
        // the ping was matched already: a second wait for it runs out
        let error = run
            .driver
            .wait_for_request("a second ping", &ping, Duration::from_millis(300))
            .await
            .err()
            .ok_or("matched the same request twice")?;
        assert_eq!(error.step(), "a second ping");
        Ok(())
    })
    .await
}

#[tokio::test]
async fn a_wait_that_runs_out_names_its_step() -> TestResult {
    with_page(async |run| {
        let error = run
            .driver
            .wait_for_tid("open the chat list", "no-such-tid", Duration::from_millis(500))
            .await
            .err()
            .ok_or("found an element that isn't there")?;
        assert!(matches!(error, StepError::Timeout { .. }), "{error:?}");
        let shown = error.to_string();
        assert!(shown.contains("step `open the chat list`"), "{shown}");
        assert!(shown.contains("timed out after 500ms"), "{shown}");
        assert!(shown.contains("no-such-tid"), "{shown}");

        let never = RequestPattern::new(Some("POST"), "/never")?;
        let error = run
            .driver
            .wait_for_request("send the message", &never, Duration::from_millis(300))
            .await
            .err()
            .ok_or("matched a request never sent")?;
        let shown = error.to_string();
        assert!(
            shown.contains("step `send the message`") && shown.contains("POST") && shown.contains("/never"),
            "{shown}"
        );

        let error = run
            .driver
            .click("press a missing button", Selector::Tid("missing"))
            .await
            .err()
            .ok_or("clicked nothing")?;
        assert!(error.to_string().contains("step `press a missing button`"), "{error}");
        Ok(())
    })
    .await
}

#[tokio::test]
async fn a_wait_runs_out_even_when_the_page_stops_answering() -> TestResult {
    with_page(async |run| {
        run.driver
            .wait_for_tid("the page ran", "hang", Duration::from_secs(10))
            .await?;
        run.driver.click("hang the page", Selector::Tid("hang")).await?;
        let started = std::time::Instant::now();
        let error = run
            .driver
            .wait_for_text("a stalled page", "never shown", Duration::from_secs(1))
            .await
            .err()
            .ok_or("a stalled page showed the text")?;
        assert!(matches!(error, StepError::Timeout { .. }), "{error:?}");
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
        Ok(())
    })
    .await
}

#[tokio::test]
async fn typing_and_enter_send_and_the_text_shows() -> TestResult {
    with_page(async |run| {
        run.driver
            .wait_for_tid("the box", "box", Duration::from_secs(5))
            .await?;
        run.driver.type_into("type", Selector::Tid("box"), "hello lab").await?;
        run.driver.press_enter("send").await?;
        run.driver
            .wait_for_text("the echo", "sent: hello lab", Duration::from_secs(5))
            .await?;
        let send = RequestPattern::new(Some("POST"), "/api/send$")?;
        let seen = run
            .driver
            .wait_for_request("the post", &send, Duration::from_secs(5))
            .await?;
        assert_eq!(seen.method, "POST");
        assert!(run.driver.shows_text("look", "sent: hello lab").await?);
        assert!(!run.driver.shows_text("look", "never shown").await?);
        Ok(())
    })
    .await
}

#[test]
fn a_request_pattern_matches_method_and_url() -> TestResult {
    let pattern = RequestPattern::new(Some("post"), MESSAGE_POST)?;
    let url = "https://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/conversations/19%3Aabc%40thread.v2/messages";
    assert!(pattern.matches("POST", url));
    assert!(pattern.matches("POST", &format!("{url}?x=1")));
    assert!(!pattern.matches("GET", url));
    assert!(!pattern.matches("POST", &format!("{url}/123")));
    assert!(RequestPattern::new(None, url).is_ok_and(|any| any.matches("PUT", url)));
    assert!(RequestPattern::new(None, "(").is_err());
    Ok(())
}

fn denial(url: &str, cause: DenyCause) -> Denial {
    Denial {
        method: "POST".to_owned(),
        url: url.to_owned(),
        cause,
    }
}

#[test]
fn a_writing_scenario_expects_only_forced_and_telemetry_denies() {
    let policy = DenyPolicy::ForcedAnd(&TELEMETRY_HOSTS);
    let expected = [
        denial(
            "https://browser.events.data.microsoft.com/OneCollector/1.0/",
            DenyCause::Unclassified,
        ),
        denial(
            "https://teams.events.data.microsoft.com/OneCollector/1.0/",
            DenyCause::Unclassified,
        ),
        denial("https://teams.microsoft.com/api/chatsvc/emea/v1/x", DenyCause::Forced),
    ];
    let unexpected = [
        denial(
            "https://teams.microsoft.com/api/chatsvc/emea/v2/users/ME/endpoints/x",
            DenyCause::Unclassified,
        ),
        denial(
            "https://notevents.data.microsoft.com.example.com/",
            DenyCause::Unclassified,
        ),
        denial("https://xevents.data.microsoft.com/", DenyCause::Unclassified),
        denial("not a url", DenyCause::Malformed),
    ];
    for denial in &expected {
        assert!(policy.expects(denial), "{denial:?}");
    }
    for denial in &unexpected {
        assert!(!policy.expects(denial), "{denial:?}");
    }
    let all: Vec<Denial> = expected.iter().chain(&unexpected).cloned().collect();
    assert_eq!(policy.unexpected(&all), unexpected.to_vec());
    assert!(DenyPolicy::AllExpected.unexpected(&all).is_empty());
}

#[test]
fn scenario_names_and_ab_chat_texts() {
    assert_eq!(ScenarioName::Load.as_str(), "load");
    assert_eq!(ScenarioName::AbChat.to_string(), "ab-chat");
    assert_eq!(
        chat_link("labbot2@example.com"),
        "https://teams.microsoft.com/l/chat/0/0?users=labbot2%40example.com"
    );
    let (probe, message) = (probe_text("n1"), message_text("n1"));
    assert!(probe.contains("n1") && message.contains("n1"));
    assert!(!message.contains(&probe) && !probe.contains(&message));
}

fn account(denials: Vec<Denial>, fence: FenceReport) -> AccountRun {
    AccountRun {
        alias: Alias::A,
        fence,
        denials,
        frame_denials: Vec::new(),
        capture: None,
    }
}

#[test]
fn a_writing_run_fails_on_any_denied_frame() {
    let policy = DenyPolicy::ForcedAnd(&TELEMETRY_HOSTS);
    let frame = FrameDenial {
        url: "wss://teams.microsoft.com/api/socket".to_owned(),
        cause: DenyCause::Unclassified,
    };
    let fence = FenceReport {
        denied_frames: 1,
        ..FenceReport::default()
    };
    let run = AccountRun {
        frame_denials: vec![frame.clone()],
        ..account(Vec::new(), fence.clone())
    };
    let mut report = ScenarioReport::new(ScenarioName::AbChat);
    report.add_account(run, policy);
    assert_eq!(report.failures.len(), 1, "{:?}", report.failures);
    assert!(
        report.failures[0].contains("WebSocket frame") && report.failures[0].contains("/api/socket"),
        "{:?}",
        report.failures
    );

    // a deny the report counts but the list missed fails it too
    let mut counted = ScenarioReport::new(ScenarioName::AbChat);
    counted.add_account(account(Vec::new(), fence.clone()), policy);
    assert!(counted.failed());
    assert!(
        counted.failures[0].contains("1 more WebSocket frame"),
        "{:?}",
        counted.failures
    );

    // the fail-closed load expects frame denies like any other
    let mut load = ScenarioReport::new(ScenarioName::Load);
    let run = AccountRun {
        frame_denials: vec![frame],
        ..account(Vec::new(), fence)
    };
    load.add_account(run, DenyPolicy::AllExpected);
    assert!(!load.failed(), "{:?}", load.failures);
}

#[test]
fn a_run_fails_on_an_unexpected_deny_or_a_breach_only() {
    let telemetry = denial(
        "https://browser.events.data.microsoft.com/OneCollector/1.0/",
        DenyCause::Unclassified,
    );
    let endpoint = denial(
        "https://teams.microsoft.com/api/chatsvc/emea/v2/users/ME/endpoints/x",
        DenyCause::Unclassified,
    );
    let policy = DenyPolicy::ForcedAnd(&TELEMETRY_HOSTS);

    let mut clean = ScenarioReport::new(ScenarioName::AbChat);
    clean.add_account(account(vec![telemetry.clone()], FenceReport::default()), policy);
    assert!(!clean.failed(), "{:?}", clean.failures);

    let mut denied = ScenarioReport::new(ScenarioName::AbChat);
    denied.add_account(
        account(vec![telemetry, endpoint.clone()], FenceReport::default()),
        policy,
    );
    assert!(denied.failed());
    assert_eq!(denied.failures.len(), 1);
    assert!(
        denied.failures[0].contains("unexpected deny") && denied.failures[0].contains("/endpoints/"),
        "{:?}",
        denied.failures
    );

    // the fail-closed load expects every deny, but not a breach
    let mut load = ScenarioReport::new(ScenarioName::Load);
    load.add_account(account(vec![endpoint], FenceReport::default()), DenyPolicy::AllExpected);
    assert!(!load.failed());
    let breached = FenceReport {
        unsanctioned_frames: 1,
        ..FenceReport::default()
    };
    load.add_account(account(Vec::new(), breached), DenyPolicy::AllExpected);
    assert!(load.failed());
    assert!(load.failures[0].contains("breached"), "{:?}", load.failures);
}
