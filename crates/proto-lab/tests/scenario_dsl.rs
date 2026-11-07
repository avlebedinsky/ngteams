//! The scenario steps (`proto_lab::scenario::dsl`) against a local page in a throwaway armed browser: waiting for a
//! `data-tid` element, for a request matching a pattern, typing and pressing Enter; and a wait that runs out fails
//! with an error naming its step. Also what a scenario expects of the fence (`DenyPolicy`) and `ab-chat`'s helpers.

mod common;
#[path = "common/page.rs"]
mod page;

use std::time::Duration;

use futures_util::StreamExt as _;
use proto_lab::arming::ArmedBrowser;
use proto_lab::browser::{Browser, LaunchOptions};
use proto_lab::capture_hold::CaptureHold;
use proto_lab::cdp::{Cdp, CdpError, Event, RemoteError, SessionId};
use proto_lab::scenario::ScenarioName;
use proto_lab::scenario::ab_chat::{
    self, HELD_MARKER, MESSAGE_POST, chat_link, chat_requests, chat_url, message_text, one_to_one_id,
};
use proto_lab::scenario::ab_chat_full::{self, Texts, Writes};
use proto_lab::scenario::dsl::{self, Driver, Key, RequestPattern, SeenRequest, Selector, StepError};
use serde_json::{Value, json};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// A page that adds a `data-tid` element after 400 ms, fetches `/api/ping` at once and `/api/late` after 600 ms, and
/// echoes what is typed into its box when Enter is pressed. Its `hang` button stalls the page for good, a moment after
/// the click. Its message, far down the page, grows a `bar` button once the mouse is over it; the box reports other
/// keys pressed in it.
const PAGE: &str = r#"<!doctype html><html><body>
<div id="out"></div>
<div id="keys"></div>
<button data-tid="hang" onclick="setTimeout(() => { for (;;) {} }, 50)">hang</button>
<div contenteditable="true" role="textbox" data-tid="box" id="box"></div>
<div data-tid="message" id="message" style="margin-top: 3000px">a message</div>
<script>
const message = document.getElementById('message');
message.addEventListener('mouseover', () => {
  if (document.querySelector('[data-tid="bar"]')) return;
  const bar = document.createElement('button'); bar.dataset.tid = 'bar'; message.append(bar);
});
document.getElementById('box').addEventListener('keydown', (event) => {
  if (event.key === 'Enter') return;
  document.getElementById('keys').textContent = 'key ' + (event.ctrlKey ? 'ctrl+' : '') + event.key;
});
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
    /// The tab's session.
    page: SessionId,
    server: MockServer,
    _dir: tempfile::TempDir,
}

async fn serve(page: &'static str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/page"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(page, "text/html"))
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

/// Runs `body` with a driver on a tab of an armed throwaway browser, opened on the page and loaded; closes the browser
/// after. Without Chrome it does nothing.
async fn with_page<F>(body: F) -> TestResult
where
    F: AsyncFnOnce(&mut Run) -> TestResult,
{
    with_html(PAGE, body).await
}

/// [`with_page`] on the page `page`.
async fn with_html<F>(page: &'static str, body: F) -> TestResult
where
    F: AsyncFnOnce(&mut Run) -> TestResult,
{
    let Some(chrome) = common::chrome_or_skip() else {
        return Ok(());
    };
    let server = serve(page).await;
    let dir = tempfile::tempdir()?;
    let browser = Browser::launch(
        &chrome,
        &LaunchOptions::new(dir.path().join("profile")),
        Duration::from_secs(30),
    )
    .await?;

    let http = CaptureHold::new();

    let armed = ArmedBrowser::arm(browser, http).await?;
    let page = armed.open_page(Duration::from_secs(15)).await?.session;
    let driver = Driver::new(armed.cdp().clone(), page.clone(), armed.watch());
    let mut run = Run {
        armed,
        driver,
        page,
        server,
        _dir: dir,
    };
    // a test acts on the page at once: once it has loaded, its elements and their handlers are there
    let url = format!("{}/page", run.server.uri());
    page::navigate(run.armed.cdp(), &run.page, &url, Duration::from_secs(15)).await?;
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
async fn ab_chat_sends_and_a_stalled_page_fails_its_steps_in_time() -> TestResult {
    with_page(async |run| {
        run.driver
            .wait_for_tid("the box", "box", Duration::from_secs(5))
            .await?;
        // the page's box is a compose box: Enter sends and empties it
        ab_chat::send(&mut run.driver, "a", "hello lab").await?;
        run.driver
            .wait_for_text("the echo", "sent: hello lab", Duration::from_secs(5))
            .await?;

        run.driver.set_call_timeout(Duration::from_secs(1));
        run.driver.click("hang the page", Selector::Tid("hang")).await?;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let started = std::time::Instant::now();
        let error = ab_chat::send(&mut run.driver, "a", "never sent")
            .await
            .err()
            .ok_or("a stalled page took a send")?;
        assert!(error.to_string().contains("timed out"), "{error}");
        let error = run
            .driver
            .shows_text("a stalled page", "x")
            .await
            .err()
            .ok_or("a stalled page answered")?;
        assert!(matches!(error, StepError::Timeout { .. }), "{error:?}");
        // a screenshot of a stalled page either comes or times out: it never hangs
        let _ = run.driver.screenshot("a screenshot of a stalled page").await;
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
        Ok(())
    })
    .await
}

/// A chat opened at its "last read" mark: the newest message shows only after "Jump to newest".
const LAST_READ_PAGE: &str = r#"<!doctype html><html><body><div id="list">an older message</div>
<button onclick="setTimeout(() => { document.getElementById('list').insertAdjacentHTML('beforeend',
'<div>the newest message</div>'); this.remove(); }, 100)">Jump to newest</button></body></html>"#;

/// The same chat with the web client's other pill, "New messages", a `role="button"` element with an icon.
const NEW_MESSAGES_PAGE: &str = r#"<!doctype html><html><body><div id="list">an older message</div>
<div role="button" onclick="setTimeout(() => { document.getElementById('list').insertAdjacentHTML('beforeend',
'<div>the newest message</div>'); this.remove(); }, 100)"><svg width="8" height="8"></svg>
New messages </div></body></html>"#;

/// `wait_for_newest` on `page`: the newest message shows after the click, and a text that never shows times out.
async fn jumps_to_the_newest_message(page: &'static str) -> TestResult {
    with_html(page, async |run| {
        assert!(!run.driver.shows_text("before", "the newest message").await?);
        ab_chat::wait_for_newest(&mut run.driver, "b", "the newest message", Duration::from_secs(5)).await?;
        let error = ab_chat::wait_for_newest(&mut run.driver, "b", "never sent", Duration::from_millis(500))
            .await
            .err()
            .ok_or("a text that never shows was found")?;
        assert!(matches!(error, StepError::Timeout { .. }), "{error:?}");
        Ok(())
    })
    .await
}

#[tokio::test]
async fn ab_chat_jumps_to_the_newest_message_of_a_chat_opened_at_its_last_read_mark() -> TestResult {
    jumps_to_the_newest_message(LAST_READ_PAGE).await
}

#[tokio::test]
async fn ab_chat_follows_the_new_messages_pill_too() -> TestResult {
    jumps_to_the_newest_message(NEW_MESSAGES_PAGE).await
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

#[tokio::test]
async fn hovering_shows_what_is_under_the_pointer() -> TestResult {
    with_page(async |run| {
        let message = Selector::Tid("message");
        run.driver
            .wait_for("the message", message, Duration::from_secs(5))
            .await?;
        assert!(
            run.driver
                .wait_for_tid("no bar yet", "bar", Duration::from_millis(300))
                .await
                .is_err()
        );
        // the message is far below the fold: hovering scrolls it into view first
        run.driver.hover("hover the message", message).await?;
        run.driver
            .wait_for_tid("the bar", "bar", Duration::from_secs(5))
            .await?;
        let error = run
            .driver
            .hover("hover nothing", Selector::Tid("missing"))
            .await
            .err()
            .ok_or("pointed at a missing element")?;
        assert!(matches!(error, StepError::Failed { .. }), "{error:?}");
        assert!(
            error
                .to_string()
                .contains("no an element with data-tid \"missing\" to point at")
        );
        Ok(())
    })
    .await
}

/// A page that keeps its renderer busy most of the time, unless it was opened as `?arrived`: then it grows the
/// element a wait looks for.
const BUSY_PAGE: &str = r"<!doctype html><html><body><script>
if (location.search === '?arrived') {
  setTimeout(() => { const e = document.createElement('div'); e.dataset.tid = 'arrived'; document.body.append(e); }, 300);
} else {
  setInterval(() => { const until = Date.now() + 400; while (Date.now() < until) {} }, 450);
}
</script></body></html>";

#[tokio::test]
async fn a_wait_goes_on_across_navigations_of_its_page() -> TestResult {
    with_html(BUSY_PAGE, async |run| {
        // a busy page on one site, left for the other (a new renderer) while waits look into it, a few times over.
        // The race the live run hit (an evaluation still pending when its page navigates) can't be forced here;
        // what the browser answers then is `navigated_away`'s, tested below
        let port = Url::parse(&run.server.uri())?.port().ok_or("no port")?;
        for round in 0..3 {
            let (here, there) = if round % 2 == 0 {
                ("localhost", "127.0.0.1")
            } else {
                ("127.0.0.1", "localhost")
            };
            run.driver
                .navigate("the busy page", &format!("http://{here}:{port}/page"))
                .await?;
            tokio::time::sleep(Duration::from_millis(500)).await;
            run.driver
                .navigate("the other site", &format!("http://{there}:{port}/page?arrived"))
                .await?;
            run.driver
                .wait_for_tid("after the hop", "arrived", Duration::from_secs(20))
                .await?;
        }
        Ok(())
    })
    .await
}

/// A connection to a local endpoint that takes commands and never answers: for a driver whose waits read only the
/// events a test feeds it.
async fn silent_cdp() -> TestResult<Cdp> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await
            && let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await
        {
            while let Some(Ok(_)) = socket.next().await {}
        }
    });
    let (cdp, _events) = Cdp::connect(&format!("ws://{address}")).await?;
    Ok(cdp)
}

fn network_event(method: &str, params: Value) -> Event {
    Event {
        session_id: Some(SessionId::new("S-page")),
        method: method.to_owned(),
        params,
    }
}

/// A `fetch` the page drops once it is answered fails after its response (`net::ERR_ABORTED`, seen on a loaded
/// machine): it still got a response, whether the wait looks between the two events or only after both.
#[tokio::test]
async fn a_request_answered_and_then_aborted_got_a_response_and_didnt_fail() -> TestResult {
    let (events, watch) = tokio::sync::mpsc::unbounded_channel();
    let mut driver = Driver::new(silent_cdp().await?, SessionId::new("S-page"), watch);
    let request = json!({"method": "GET", "url": "http://127.0.0.1/api/late"});
    events.send(network_event(
        "Network.requestWillBeSent",
        json!({"requestId": "1", "request": request}),
    ))?;
    events.send(network_event(
        "Network.responseReceived",
        json!({"requestId": "1", "response": {"status": 204}}),
    ))?;
    events.send(network_event(
        "Network.loadingFailed",
        json!({"requestId": "1", "errorText": "net::ERR_ABORTED"}),
    ))?;
    let late = RequestPattern::new(None, "/api/late$")?;
    let seen = driver
        .wait_for_request("the late one", &late, Duration::from_secs(1))
        .await?;
    let error = driver
        .wait_for_failure("the late one fails", &seen, Duration::from_secs(1))
        .await
        .err()
        .ok_or("an answered request counted as failed")?;
    assert!(error.to_string().contains("got a response (204)"), "{error}");
    let error = driver
        .wait_for_failure_of("the late one by its id", "1", Duration::from_secs(1))
        .await
        .err()
        .ok_or("an answered request counted as failed")?;
    assert!(error.to_string().contains("got a response (204)"), "{error}");
    Ok(())
}

/// A wait for a failed request passes over a matching one that got a response and then failed (`net::ERR_ABORTED`),
/// and gives the one that failed without a response (the fence's `net::ERR_BLOCKED_BY_CLIENT`); with only the answered
/// one it runs out.
#[tokio::test]
async fn a_failed_request_wait_passes_over_one_that_got_a_response_and_then_failed() -> TestResult {
    let (events, watch) = tokio::sync::mpsc::unbounded_channel();
    let mut driver = Driver::new(silent_cdp().await?, SessionId::new("S-page"), watch);
    let posts = RequestPattern::new(Some("POST"), "/api/send$")?;
    let request = json!({"method": "POST", "url": "http://127.0.0.1/api/send"});
    events.send(network_event(
        "Network.requestWillBeSent",
        json!({"requestId": "1", "request": request}),
    ))?;
    events.send(network_event(
        "Network.responseReceived",
        json!({"requestId": "1", "response": {"status": 204}}),
    ))?;
    events.send(network_event(
        "Network.loadingFailed",
        json!({"requestId": "1", "errorText": "net::ERR_ABORTED"}),
    ))?;
    let error = driver
        .wait_for_failed_request("only the answered one", &posts, Duration::from_millis(300))
        .await
        .err()
        .ok_or("an answered request counted as failed")?;
    assert!(matches!(error, StepError::Timeout { .. }), "{error}");

    events.send(network_event(
        "Network.requestWillBeSent",
        json!({"requestId": "2", "request": request}),
    ))?;
    events.send(network_event(
        "Network.loadingFailed",
        json!({"requestId": "2", "errorText": "net::ERR_BLOCKED_BY_CLIENT"}),
    ))?;
    let failed = driver
        .wait_for_failed_request("the blocked one", &posts, Duration::from_secs(1))
        .await?;
    assert_eq!(failed.request_id, "2");
    assert_eq!(failed.failed.as_deref(), Some("net::ERR_BLOCKED_BY_CLIENT"));
    Ok(())
}

/// A navigation the browser never answers runs out its bound: the `Page.navigate` command is inside it.
#[tokio::test]
async fn a_navigation_the_browser_never_answers_runs_out_its_bound() -> TestResult {
    let cdp = silent_cdp().await?;
    let session = SessionId::new("S-page");
    let navigation = page::navigate(&cdp, &session, "http://127.0.0.1/page", Duration::from_millis(300));
    let error = tokio::time::timeout(Duration::from_secs(10), navigation)
        .await
        .map_err(|_| "an unanswered navigation outlived its bound")?
        .err()
        .ok_or("an unanswered navigation succeeded")?;
    assert!(error.to_string().contains("didn't commit within 300ms"), "{error}");
    Ok(())
}

#[test]
fn only_an_evaluation_refused_because_its_page_navigated_counts_as_navigated_away() {
    let remote = |message: &str| StepError::Cdp {
        step: "a wait".to_owned(),
        source: CdpError::Remote {
            method: "Runtime.evaluate".to_owned(),
            error: RemoteError {
                code: -32000,
                message: message.to_owned(),
            },
        },
    };
    for message in [
        "Inspected target navigated or closed",
        "Execution context was destroyed.",
        "Cannot find context with specified id",
    ] {
        assert!(dsl::navigated_away(&remote(message)), "{message}");
    }
    for error in [
        remote("Session with given id not found."),
        StepError::Cdp {
            step: "a wait".to_owned(),
            source: CdpError::Closed,
        },
        StepError::Closed {
            step: "a wait".to_owned(),
        },
    ] {
        assert!(!dsl::navigated_away(&error), "{error}");
    }
}

#[tokio::test]
async fn keys_are_pressed_with_their_modifiers() -> TestResult {
    with_page(async |run| {
        run.driver.type_into("type", Selector::Tid("box"), "hello").await?;
        run.driver.press("select all", Key::SELECT_ALL).await?;
        run.driver
            .wait_for_text("ctrl+a", "key ctrl+a", Duration::from_secs(5))
            .await?;
        run.driver.press("escape", Key::ESCAPE).await?;
        run.driver
            .wait_for_text("escape", "key Escape", Duration::from_secs(5))
            .await?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn an_answer_is_waited_for_only_among_requests_not_passed_over() -> TestResult {
    with_page(async |run| {
        run.driver
            .wait_for_tid("the page ran", "chat-list", Duration::from_secs(10))
            .await?;
        let ping = RequestPattern::new(Some("GET"), "/api/ping")?;
        // the ping went out (and was answered) before: passed over, a wait for its answer runs out
        tokio::time::sleep(Duration::from_millis(300)).await;
        run.driver.skip_seen();
        let error = run
            .driver
            .wait_for_answer("the ping, passed over", &ping, Duration::from_millis(300))
            .await
            .err()
            .ok_or("waited for a request passed over")?;
        assert!(error.to_string().contains("to be answered"), "{error}");
        run.driver.type_into("type", Selector::Tid("box"), "hello").await?;
        run.driver.press_enter("send").await?;
        let send = RequestPattern::new(Some("POST"), "/api/send$")?;
        let seen = run
            .driver
            .wait_for_answer(
                "the post",
                &send.clone().with_body_containing("hello"),
                Duration::from_secs(5),
            )
            .await?;
        assert_eq!((seen.method.as_str(), seen.status), ("POST", Some(204)));
        assert_eq!(seen.body.as_deref(), Some("hello"));
        Ok(())
    })
    .await
}

#[test]
fn a_request_pattern_with_a_body_text_matches_only_a_body_holding_it() -> TestResult {
    let request = |body: Option<&str>| SeenRequest {
        session: None,
        request_id: "1".to_owned(),
        method: "POST".to_owned(),
        url: "https://example.test/messages".to_owned(),
        status: None,
        failed: None,
        body: body.map(str::to_owned),
        sent_ms: None,
    };
    let any = RequestPattern::new(Some("POST"), "/messages$")?;
    let message = any.clone().with_body_containing("lab message");
    assert!(any.matches_request(&request(None)));
    assert!(message.matches_request(&request(Some(r#"{"content":"<p>lab message 1</p>"}"#))));
    // a typing post to the same URL, and a body the events left out
    assert!(!message.matches_request(&request(Some(r#"{"messagetype":"Control/Typing"}"#))));
    assert!(!message.matches_request(&request(None)));
    assert!(message.to_string().ends_with(r#"holding "lab message""#), "{message}");
    Ok(())
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

const LOW: &str = "8:orgid:0000000a-0000-4000-8000-000000000001";
const HIGH: &str = "8:orgid:0000000B-0000-4000-8000-000000000002";
const CHAT: &str = "19:0000000a-0000-4000-8000-000000000001_0000000b-0000-4000-8000-000000000002@unq.gbl.spaces";

#[test]
fn ab_chat_finds_the_one_to_one_chat_by_its_members() {
    let (low, high, chat) = (LOW, HIGH, CHAT);
    // the object ids sorted and lowercased, whichever account comes first
    assert_eq!(one_to_one_id(low, high).as_deref(), Some(chat));
    assert_eq!(one_to_one_id(high, low).as_deref(), Some(chat));
    for (first, second) in [(low, low), (low, ""), ("28:bot", high), ("8:orgid:not-a-guid", high)] {
        assert_eq!(one_to_one_id(first, second), None, "{first} {second}");
    }
    assert_eq!(
        chat_link(high),
        "https://teams.microsoft.com/l/chat/0/0?users=8%3Aorgid%3A0000000B-0000-4000-8000-000000000002"
    );
}

#[test]
fn ab_chat_knows_the_chat_in_a_url() -> TestResult {
    let chat = CHAT;
    let opened = chat_requests(chat)?;
    let names = chat_url(chat)?;
    let post = format!("https://teams.cloud.microsoft/api/chatsvc/amer/v1/users/ME/conversations/{chat}/messages")
        .replace("19:", "19%3A")
        .replace("@unq", "%40unq");
    assert!(names.is_match(&post));
    assert!(!names.is_match(&post.replace("0000000b", "0000000c")));
    let plain = format!("https://teams.microsoft.com/api/chatsvc/amer/v1/threads/{chat}?view=msnp24Equivalent");
    let encoded = plain.replace("19:", "19%3A").replace("@unq", "%40unq");
    assert!(opened.matches("GET", &plain));
    assert!(opened.matches("GET", &encoded));
    assert!(!opened.matches("POST", &plain));
    assert!(!opened.matches("GET", &plain.replace("0000000b", "0000000c")));
    Ok(())
}

#[test]
fn scenario_names_and_ab_chat_texts() {
    assert_eq!(ScenarioName::Load.as_str(), "load");
    assert_eq!(ScenarioName::AbChat.to_string(), "ab-chat");
    let message = message_text("n1");
    assert!(message.contains("n1"));
    assert!(!message.contains(HELD_MARKER.trim()));
}

/// A page shaped like the web client's chat where `ab-chat-full` acts: two messages (the second quoting the first), a
/// hover bar that shows once the first is pointed at, with an edit action, a like and a more-options button whose menu
/// holds delete; a hidden like ahead of the bar's; and a compose box. Each control says on the page what it did; the
/// like posts too.
const CHAT_PAGE: &str = r#"<!doctype html><html><body>
<div id="out"></div>
<button data-tid="message-actions-like" hidden>a hidden like</button>
<div data-tid="chat-pane-message" id="first"><p>lab full n1 first</p></div>
<div data-tid="chat-pane-message" id="quote"><blockquote>lab full n1 first</blockquote><p>lab full n1 reply</p></div>
<div id="bar" hidden>
  <div data-tid="message-hover-action-edit"><button onclick="say('edit')">edit</button></div>
  <button data-tid="message-actions-like" onclick="say('like'); fetch('/api/send', {method: 'POST', body: 'like'})">like</button>
  <button data-tid="message-actions-more" onclick="document.getElementById('menu').hidden = false">more</button>
</div>
<div id="menu" hidden><div role="menuitem" data-tid="message-actions-delete" onclick="say('delete')">delete</div></div>
<div contenteditable="true" role="textbox" id="compose"></div>
<script>
function say(what) { document.getElementById('out').textContent = 'did ' + what; }
document.getElementById('first').addEventListener('mouseover', () => { document.getElementById('bar').hidden = false; });
</script></body></html>"#;

#[tokio::test]
async fn ab_chat_full_finds_the_oldest_message_holding_a_text_and_its_actions() -> TestResult {
    with_html(CHAT_PAGE, async |run| {
        let drive = &mut run.driver;
        ab_chat_full::find_message(drive, "find the first message", "lab full n1 first").await?;
        // the quote of it comes later: the message itself is marked
        let marked = |id| format!("#{id}[data-lab-mark]");
        drive
            .wait_for("marked", Selector::Css(&marked("first")), Duration::from_secs(1))
            .await?;
        // on the hover bar, which shows once the message is pointed at
        ab_chat_full::message_action(drive, "a", "edit").await?;
        drive
            .wait_for_text("edited", "did edit", Duration::from_secs(5))
            .await?;
        // not on the bar: from the more-options menu
        ab_chat_full::message_action(drive, "a", "delete").await?;
        drive
            .wait_for_text("deleted", "did delete", Duration::from_secs(5))
            .await?;
        // the visible like, not the hidden one ahead of it
        drive.skip_seen();
        let like = r#"[data-tid="message-actions-like"]"#;
        ab_chat_full::click_visible(drive, "like", like, Duration::from_secs(5)).await?;
        drive.wait_for_text("liked", "did like", Duration::from_secs(5)).await?;
        let post = RequestPattern::new(Some("POST"), "/api/send$")?;
        let seen = drive
            .wait_for_answer("the like's post", &post, Duration::from_secs(5))
            .await?;
        ab_chat_full::answered("the like", &seen)?;
        // the mark moves to the next message found
        ab_chat_full::find_message(drive, "find the reply", "lab full n1 reply").await?;
        drive
            .wait_for("marked", Selector::Css(&marked("quote")), Duration::from_secs(1))
            .await?;
        assert!(!drive.shows_text("look", "nothing").await?);
        let missing =
            ab_chat_full::click_visible(drive, "nothing", r#"[data-tid="nope"]"#, Duration::from_millis(300)).await;
        assert!(matches!(missing, Err(StepError::Timeout { .. })), "{missing:?}");
        Ok(())
    })
    .await
}

/// An edit box like the web client's: ahead of the compose box, it rebuilds its element (a new one, same text, the
/// focus moved to it) the first time it is focused, and Enter in it says what it saves.
const EDIT_PAGE: &str = r#"<!doctype html><html><body>
<div id="out"></div>
<div id="slot"><div contenteditable="true" role="textbox" class="edit">lab full n1 first</div></div>
<div contenteditable="true" role="textbox" id="compose"></div>
<script>
let rebuilt = false;
document.getElementById('slot').addEventListener('focusin', (event) => {
  if (rebuilt) return;
  rebuilt = true;
  const old = event.target, fresh = old.cloneNode(true);
  setTimeout(() => { old.replaceWith(fresh); fresh.focus(); }, 0);
});
document.getElementById('slot').addEventListener('keydown', (event) => {
  if (event.key !== 'Enter') return;
  event.preventDefault();
  document.getElementById('out').textContent = 'saved ' + event.target.innerText;
});
</script></body></html>"#;

#[tokio::test]
async fn ab_chat_full_rewrites_an_editor_that_rebuilds_itself_when_focused() -> TestResult {
    with_html(EDIT_PAGE, async |run| {
        let drive = &mut run.driver;
        ab_chat_full::rewrite_editor(drive, "the edit box", "lab full n1 first", "lab full n1 first, edited").await?;
        drive.press_enter("save").await?;
        drive
            .wait_for_text("saved", "saved lab full n1 first, edited", Duration::from_secs(5))
            .await?;
        // nothing went into the compose box
        let compose = drive
            .wait_until_true(
                "the compose box is empty",
                "document.getElementById('compose').innerText === ''",
                "an empty compose box",
                Duration::from_secs(1),
            )
            .await;
        assert!(compose.is_ok(), "{compose:?}");
        Ok(())
    })
    .await
}

fn seen(status: Option<u16>, failed: Option<&str>) -> SeenRequest {
    SeenRequest {
        session: None,
        request_id: "1".to_owned(),
        method: "PUT".to_owned(),
        url: "https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations/c/messages/1?name=emotions"
            .to_owned(),
        status,
        failed: failed.map(str::to_owned),
        body: None,
        sent_ms: None,
    }
}

#[test]
fn ab_chat_full_takes_only_a_2xx_answer_for_a_write_that_went_through() {
    assert!(ab_chat_full::answered("a write", &seen(Some(201), None)).is_ok());
    assert!(ab_chat_full::answered("a write", &seen(Some(200), None)).is_ok());
    for (status, failed, says) in [
        (
            None,
            Some("net::ERR_BLOCKED_BY_CLIENT"),
            "failed in the page: net::ERR_BLOCKED_BY_CLIENT",
        ),
        (Some(404), None, "was answered Some(404)"),
        (Some(302), None, "was answered Some(302)"),
    ] {
        let error = ab_chat_full::answered("a write", &seen(status, failed))
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(error.contains(says), "{error}");
        // the request is named without its query
        assert!(
            error.contains("PUT https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations/c/messages/1 "),
            "{error}"
        );
    }
}

#[test]
fn ab_chat_full_waits_for_the_writes_on_the_lab_chat_only() -> TestResult {
    let chat = one_to_one_id(
        "8:orgid:00000000-0000-4000-8000-00000000000a",
        "8:orgid:00000000-0000-4000-8000-00000000000b",
    )
    .ok_or("no chat id")?;
    let encoded = chat.replace(':', "%3A").replace('@', "%40");
    let writes = Writes::on(&chat)?;
    let base = "https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations";
    for id in [chat.as_str(), encoded.as_str()] {
        let message = format!("{base}/{id}/messages/1792512759831");
        assert!(writes.post.matches("POST", &format!("{base}/{id}/messages")));
        assert!(writes.edit.matches("PUT", &message));
        assert!(
            writes
                .delete
                .matches("DELETE", &format!("{message}?behavior=softDelete"))
        );
        for method in ["PUT", "DELETE"] {
            assert!(
                writes
                    .reaction
                    .matches(method, &format!("{message}/properties?name=emotions"))
            );
        }
        let horizon = format!("{base}/{id}/properties?name=consumptionhorizon");
        assert!(writes.read_receipt.matches("PUT", &horizon));
        // not one for another
        assert!(!writes.post.matches("POST", &message));
        assert!(
            !writes
                .edit
                .matches("PUT", &format!("{message}/properties?name=emotions"))
        );
        assert!(
            !writes
                .reaction
                .matches("PUT", &format!("{message}/properties?name=emotionsX"))
        );
        assert!(
            !writes
                .read_receipt
                .matches("PUT", &format!("{base}/{id}/properties?name=lastTimeFavorited"))
        );
    }
    // another chat's writes are none of them
    let other = "19%3Aother%40thread.v2";
    assert!(!writes.post.matches("POST", &format!("{base}/{other}/messages")));
    assert!(!writes.edit.matches("PUT", &format!("{base}/{other}/messages/1")));
    Ok(())
}

#[test]
fn ab_chat_full_texts_are_this_runs_and_apart() {
    let texts = Texts::new("n1");
    let all = [&texts.first, &texts.edited, &texts.reply, &texts.doomed];
    for (index, text) in all.iter().enumerate() {
        assert!(text.contains("n1") && !text.contains(HELD_MARKER.trim()), "{text}");
        for (other_index, other) in all.iter().enumerate() {
            // only the edit holds another text: the message it edits
            let holds = index != other_index && text.contains(other.as_str());
            assert_eq!(holds, (index, other_index) == (1, 0), "{text} / {other}");
        }
    }
    assert_eq!(ScenarioName::AbChatFull.as_str(), "ab-chat-full");
}

/// A compose box that, like the web client's editor, takes an image file handed to it (by a paste, or by a drop when
/// `#mode` says `drop`) and shows it as an `img`, saying on the page the file's name, type and size.
const IMAGE_PAGE: &str = r#"<!doctype html><html><body>
<div id="out"></div>
<div id="mode">paste</div>
<div contenteditable="true" role="textbox" id="compose"></div>
<script>
const compose = document.getElementById('compose');
function take(files, how) {
  if (document.getElementById('mode').textContent !== how) return;
  const file = files[0];
  document.getElementById('out').textContent = how + ' ' + file.name + ' ' + file.type + ' ' + file.size;
  const img = document.createElement('img');
  img.src = URL.createObjectURL(file);
  compose.append(img);
}
compose.addEventListener('paste', (event) => { event.preventDefault(); take(event.clipboardData.files, 'paste'); });
compose.addEventListener('dragover', (event) => event.preventDefault());
compose.addEventListener('drop', (event) => { event.preventDefault(); take(event.dataTransfer.files, 'drop'); });
</script></body></html>"#;

#[tokio::test]
async fn ab_image_hands_the_compose_box_its_test_image_by_paste_or_drop() -> TestResult {
    use proto_lab::scenario::ab_image::{compose_has_image, image_event, test_png};
    with_html(IMAGE_PAGE, async |run| {
        let drive = &mut run.driver;
        let png = test_png(0x40);
        let has_image = compose_has_image();
        let short = Duration::from_secs(5);
        // the page takes pastes: a drop does nothing there
        drive
            .wait_until_true(
                "drop",
                &image_event(&png, "lab-image-n1.png", "drop"),
                "dispatched",
                short,
            )
            .await?;
        let none = drive
            .wait_until_true("no image", &has_image, "an image", Duration::from_millis(500))
            .await;
        assert!(matches!(none, Err(StepError::Timeout { .. })), "{none:?}");
        drive
            .wait_until_true(
                "paste",
                &image_event(&png, "lab-image-n1.png", "paste"),
                "dispatched",
                short,
            )
            .await?;
        drive
            .wait_until_true("image", &has_image, "an image", Duration::from_secs(5))
            .await?;
        let said = format!("paste lab-image-n1.png image/png {}", png.len());
        drive.wait_for_text("pasted", &said, Duration::from_secs(5)).await?;
        // a page that takes drops
        drive
            .wait_until_true(
                "switch",
                "(() => { document.getElementById('mode').textContent = 'drop'; return true; })()",
                "switched",
                short,
            )
            .await?;
        drive
            .wait_until_true(
                "drop",
                &image_event(&png, "lab-image-n2.png", "drop"),
                "dispatched",
                short,
            )
            .await?;
        let said = format!("drop lab-image-n2.png image/png {}", png.len());
        drive.wait_for_text("dropped", &said, Duration::from_secs(5)).await?;
        Ok(())
    })
    .await
}

/// A compose box that, like the web client's, takes a file dropped on it (or pasted, when `#mode` says `paste`) and
/// shows it as an attachment beside the box: a chip with the file's name apart from its extension, saying on the page
/// the file's type and size. A chat message above names a file of another run.
const FILE_PAGE: &str = r#"<!doctype html><html><body>
<div data-tid="chat-pane-message"><span>lab-file-n0</span><span>.txt</span></div>
<div id="out"></div>
<div id="mode">drop</div>
<div contenteditable="true" role="textbox" id="compose"></div>
<div id="attachments"></div>
<script>
const compose = document.getElementById('compose');
function take(files, how) {
  if (document.getElementById('mode').textContent !== how) return;
  const file = files[0];
  document.getElementById('out').textContent = how + ' ' + file.type + ' ' + file.size;
  const chip = document.createElement('div');
  chip.setAttribute('title', file.name);
  const stem = document.createElement('span');
  stem.textContent = file.name.replace(/\.txt$/, '');
  const extension = document.createElement('span');
  extension.textContent = '.txt';
  chip.append(stem, extension);
  document.getElementById('attachments').append(chip);
}
compose.addEventListener('paste', (event) => { event.preventDefault(); take(event.clipboardData.files, 'paste'); });
compose.addEventListener('dragover', (event) => event.preventDefault());
compose.addEventListener('drop', (event) => { event.preventDefault(); take(event.dataTransfer.files, 'drop'); });
</script></body></html>"#;

#[tokio::test]
async fn ab_file_hands_the_compose_box_its_file_by_drop_or_paste_and_sees_it_attached() -> TestResult {
    use proto_lab::scenario::ab_file::{file_event, shows_attachment, test_file};
    with_html(FILE_PAGE, async |run| {
        let drive = &mut run.driver;
        let content = test_file("n1");
        let short = Duration::from_secs(5);
        // another run's file in a message isn't this run's attachment
        let other = drive
            .wait_until_true(
                "another run's",
                &shows_attachment("lab-file-n0.txt"),
                "an attachment",
                Duration::from_millis(500),
            )
            .await;
        assert!(matches!(other, Err(StepError::Timeout { .. })), "{other:?}");
        let attached = shows_attachment("lab-file-n1.txt");
        // the caption typed in the box names the file's stem, never its whole name: not an attachment
        drive
            .wait_until_true(
                "caption",
                "(() => { document.getElementById('compose').textContent = 'lab-file-n1 caption'; return true; })()",
                "typed",
                short,
            )
            .await?;
        let caption = drive
            .wait_until_true("only a caption", &attached, "an attachment", Duration::from_millis(500))
            .await;
        assert!(matches!(caption, Err(StepError::Timeout { .. })), "{caption:?}");
        // the page takes drops: a paste does nothing there
        drive
            .wait_until_true(
                "paste",
                &file_event(&content, "lab-file-n1.txt", "text/plain", "paste"),
                "dispatched",
                short,
            )
            .await?;
        let none = drive
            .wait_until_true("not attached", &attached, "an attachment", Duration::from_millis(500))
            .await;
        assert!(matches!(none, Err(StepError::Timeout { .. })), "{none:?}");
        drive
            .wait_until_true(
                "drop",
                &file_event(&content, "lab-file-n1.txt", "text/plain", "drop"),
                "dispatched",
                short,
            )
            .await?;
        drive
            .wait_until_true("attached", &attached, "an attachment", short)
            .await?;
        let said = format!("drop text/plain {}", content.len());
        drive.wait_for_text("dropped", &said, short).await?;
        // a page that takes pastes
        drive
            .wait_until_true(
                "switch",
                "(() => { document.getElementById('mode').textContent = 'paste'; return true; })()",
                "switched",
                short,
            )
            .await?;
        drive
            .wait_until_true(
                "paste",
                &file_event(&content, "lab-file-n2.txt", "text/plain", "paste"),
                "dispatched",
                short,
            )
            .await?;
        drive
            .wait_until_true("pasted", &shows_attachment("lab-file-n2.txt"), "an attachment", short)
            .await?;
        let said = format!("paste text/plain {}", content.len());
        drive.wait_for_text("pasted", &said, short).await?;
        Ok(())
    })
    .await
}

/// A compose box whose attach button opens a menu, and whose upload item clicks a hidden file input, as a web client
/// does; the input's chosen file is shown beside the box with its type and size. A profile button (whose label holds
/// "file") and a send button stand first, so the attach button has to be told by its label.
const PICKER_PAGE: &str = r#"<!doctype html><html><body>
<button aria-label="Your profile, status Unknown">me</button>
<div id="out"></div>
<div contenteditable="true" role="textbox" id="compose"></div>
<button aria-label="Send">send</button>
<button aria-label="Attach file" id="attach">clip</button>
<div id="menu"></div>
<div id="attachments"></div>
<script>
document.getElementById('attach').addEventListener('click', () => {
  const item = document.createElement('div');
  item.setAttribute('role', 'menuitem');
  item.textContent = 'Upload from this device';
  item.addEventListener('click', () => {
    const input = document.createElement('input');
    input.type = 'file';
    input.style.display = 'none';
    input.addEventListener('change', () => {
      const file = input.files[0];
      document.getElementById('out').textContent = 'chosen ' + file.type + ' ' + file.size;
      document.getElementById('attachments').textContent = file.name;
    });
    document.body.append(input);
    input.click();
  });
  document.getElementById('menu').append(item);
});
</script></body></html>"#;

#[tokio::test]
async fn ab_file_hands_the_file_to_the_attach_button_s_picker_without_a_dialog() -> TestResult {
    use proto_lab::scenario::ab_file::{
        CLICK_ATTACH, PAGE_NOTICES, PICK_UPLOAD, hand_file_to_pickers, shows_attachment, test_file,
    };
    with_html(PICKER_PAGE, async |run| {
        let drive = &mut run.driver;
        let content = test_file("n1");
        let short = Duration::from_secs(5);
        drive
            .wait_until_true(
                "hand",
                &hand_file_to_pickers(&content, "lab-file-n1.txt", "text/plain"),
                "installed",
                short,
            )
            .await?;
        let attach = format!("({CLICK_ATTACH}) === 'Attach file'");
        drive
            .wait_until_true("attach", &attach, "the attach button clicked", short)
            .await?;
        drive
            .wait_until_true("upload", PICK_UPLOAD, "an upload item", short)
            .await?;
        drive
            .wait_until_true("attached", &shows_attachment("lab-file-n1.txt"), "an attachment", short)
            .await?;
        let said = format!("chosen text/plain {}", content.len());
        drive.wait_for_text("chosen", &said, short).await?;
        drive
            .wait_until_true("handed", "window.__labFileHanded === 1", "one hand-over", short)
            .await?;
        // a page with no attach button: nothing clicked
        drive
            .wait_until_true(
                "remove",
                "(() => { document.getElementById('attach').remove(); return true; })()",
                "removed",
                short,
            )
            .await?;
        let none = format!("({CLICK_ATTACH}) === null");
        drive
            .wait_until_true("no attach", &none, "nothing clicked", short)
            .await?;
        // what the page says in a popup, shown ones only, each on one line
        drive
            .wait_until_true(
                "popups",
                r"(() => {
  const hidden = document.createElement('div');
  hidden.setAttribute('role', 'dialog');
  hidden.style.display = 'none';
  hidden.textContent = 'not shown';
  const shown = document.createElement('div');
  shown.setAttribute('role', 'dialog');
  shown.innerHTML = '<p>Sorry, you need a made-up thing</p>\n<p>  to share files</p>';
  document.body.append(hidden, shown);
  return true;
})()",
                "added",
                short,
            )
            .await?;
        let said = format!(
            "JSON.stringify({PAGE_NOTICES}) === JSON.stringify(['Sorry, you need a made-up thing to share files'])"
        );
        drive
            .wait_until_true("notices", &said, "the shown popup's text", short)
            .await?;
        Ok(())
    })
    .await
}

/// A chat with two messages naming this run's file (the newer by its chip's title only), and messages that must never
/// be taken for one: another run's file, a message of text, and this run's file name in no message (the compose box's
/// attachment).
const FILES_CHAT_PAGE: &str = r#"<!doctype html><html><body>
<div data-tid="chat-pane-message" id="older"><p>lab-file-n1.txt</p></div>
<div data-tid="chat-pane-message" id="newer"><div title="lab-file-n1.txt"><svg></svg></div></div>
<div data-tid="chat-pane-message" id="other"><p>lab-file-n0.txt</p></div>
<div data-tid="chat-pane-message" id="text"><p>lab file n1 plain</p></div>
<div id="attachment"><p>lab-file-n1.txt</p></div>
</body></html>"#;

#[tokio::test]
async fn ab_file_marks_only_this_run_s_unsent_file() -> TestResult {
    use proto_lab::scenario::ab_file::mark_unsent_file;
    let mark = mark_unsent_file("lab-file-n1");
    with_html(FILES_CHAT_PAGE, async |run| {
        let drive = &mut run.driver;
        drive
            .wait_until_true("mark", &mark, "this run's unsent file", Duration::from_secs(5))
            .await?;
        drive
            .wait_for("marked", Selector::Css("#newer[data-lab-mark]"), Duration::from_secs(1))
            .await?;
        drive
            .wait_until_true("one mark", ONE_MARK, "one marked message", Duration::from_secs(1))
            .await?;
        drive
            .wait_until_true(
                "remove",
                "(() => { document.getElementById('newer').remove(); document.getElementById('older').remove(); return true; })()",
                "removed",
                Duration::from_secs(5),
            )
            .await?;
        let none = drive
            .wait_until_true("mark again", &mark, "this run's unsent file", Duration::from_millis(500))
            .await;
        assert!(matches!(none, Err(StepError::Timeout { .. })), "{none:?}");
        drive
            .wait_until_true("no mark", NO_MARK, "no marked message", Duration::from_secs(1))
            .await?;
        Ok(())
    })
    .await
}

/// A page that asks before it is left, as the web client does while a message is still being sent: once a key has
/// been pressed in it (a dialog before leaving needs the user to have done something on the page).
const LEAVING_PAGE: &str = r#"<!doctype html><html><body>
<div contenteditable="true" role="textbox" id="box"></div>
<script>
window.addEventListener('beforeunload', (event) => { event.preventDefault(); event.returnValue = ''; });
</script></body></html>"#;

#[tokio::test]
async fn a_navigation_held_by_a_dialog_before_leaving_is_bounded_and_leaves_the_page() -> TestResult {
    with_html(LEAVING_PAGE, async |run| {
        run.driver
            .wait_for("the box", Selector::Css("#box"), Duration::from_secs(10))
            .await?;
        run.driver.type_into("type", Selector::Css("#box"), "unsent").await?;
        run.driver.press("a key", Key::ENTER).await?;
        // what hung a live run: Page.navigate unanswered while the page's dialog shows to nobody
        let cdp = run.armed.cdp().clone();
        let held = tokio::time::timeout(
            Duration::from_secs(3),
            cdp.call(
                Some(&run.page),
                "Page.navigate",
                serde_json::json!({"url": "about:blank"}),
            ),
        )
        .await;
        assert!(held.is_err(), "the dialog didn't hold the navigation: {held:?}");
        // the bounded navigation accepts the dialog, and the page is left
        dsl::navigate_tab(&cdp, &run.page, "about:blank", Duration::from_secs(20)).await?;
        run.driver
            .wait_until_true(
                "the page was left",
                "location.href === 'about:blank'",
                "about:blank",
                Duration::from_secs(10),
            )
            .await?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn a_navigation_the_page_holds_past_its_timeout_fails_in_time() -> TestResult {
    with_html(LEAVING_PAGE, async |run| {
        run.driver
            .wait_for("the box", Selector::Css("#box"), Duration::from_secs(10))
            .await?;
        run.driver.type_into("type", Selector::Css("#box"), "unsent").await?;
        run.driver.press("a key", Key::ENTER).await?;
        let cdp = run.armed.cdp().clone();
        // the first navigation is held by the dialog and given up at once, before the dialog wait
        let started = tokio::time::Instant::now();
        let outcome = dsl::navigate_tab(&cdp, &run.page, "about:blank", Duration::from_millis(500)).await;
        assert!(matches!(outcome, Err(dsl::NavigateError::Timeout(_))), "{outcome:?}");
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
        // and a driver's navigation is bounded by its call timeout, failing the step by name
        run.driver.set_call_timeout(Duration::from_millis(500));
        let error = match run.driver.navigate("leave", "about:blank").await {
            Ok(()) => return Ok(()), // the earlier accept may have left the page already
            Err(error) => error,
        };
        assert!(matches!(error, StepError::Timeout { .. }), "{error}");
        assert!(
            error.to_string().contains("leave") && error.to_string().contains("Page.navigate"),
            "{error}"
        );
        Ok(())
    })
    .await
}

/// A chat with two images of this run's left unsent (local previews with this run's caption), and messages that must
/// never be taken for one: an image delivered from the media service (another caption, the newest), an image without a
/// caption, an avatar (an image from a URL) on the other account's message, and this run's caption without an image.
const IMAGES_CHAT_PAGE: &str = r#"<!doctype html><html><body>
<div data-tid="chat-pane-message" id="older"><img src="data:image/gif;base64,R0lGODlhAQABAAAAACw="><p>lab image n1 caption</p></div>
<div data-tid="chat-pane-message" id="newer"><img src="data:image/gif;base64,R0lGODlhAQABAAAAACw="><p>lab image n1 caption</p></div>
<div data-tid="chat-pane-message" id="bare"><img src="data:image/gif;base64,R0lGODlhAQABAAAAACw="></div>
<div data-tid="chat-pane-message" id="avatar"><img src="/avatar.png"><p>hello</p></div>
<div data-tid="chat-pane-message" id="text"><p>lab image n1 caption</p></div>
<div data-tid="chat-pane-message" id="delivered"><img src="/v1/objects/made-up/views/imgo"><p>lab image n0 caption</p></div>
</body></html>"#;

/// Whether exactly one message of the page is marked.
const ONE_MARK: &str = "document.querySelectorAll('[data-lab-mark]').length === 1";

/// Whether no message of the page is marked.
const NO_MARK: &str = "document.querySelectorAll('[data-lab-mark]').length === 0";

#[tokio::test]
async fn ab_image_marks_only_this_run_s_unsent_image() -> TestResult {
    use proto_lab::scenario::ab_image::mark_unsent_image;
    let mark = mark_unsent_image("lab image n1 caption");
    with_html(IMAGES_CHAT_PAGE, async |run| {
        let drive = &mut run.driver;
        drive
            .wait_until_true("mark", &mark, "this run's unsent image", Duration::from_secs(5))
            .await?;
        drive
            .wait_for("marked", Selector::Css("#newer[data-lab-mark]"), Duration::from_secs(1))
            .await?;
        drive
            .wait_until_true("one mark", ONE_MARK, "one marked message", Duration::from_secs(1))
            .await?;
        // once this run's are gone, nothing else is taken for one: not the delivered image, not one without the caption
        drive
            .wait_until_true(
                "remove",
                "(() => { document.getElementById('newer').remove(); document.getElementById('older').remove(); return true; })()",
                "removed",
                Duration::from_secs(5),
            )
            .await?;
        let none = drive
            .wait_until_true("mark again", &mark, "this run's unsent image", Duration::from_millis(500))
            .await;
        assert!(matches!(none, Err(StepError::Timeout { .. })), "{none:?}");
        drive
            .wait_until_true("no mark", NO_MARK, "no marked message", Duration::from_secs(1))
            .await?;
        Ok(())
    })
    .await
}

/// A request's send time is the event's `wallTime` in whole ms; an event without one (or with a time no clock gives)
/// leaves it unknown.
#[tokio::test]
async fn a_request_s_send_time_is_read_from_its_wall_time() -> TestResult {
    let (events, watch) = tokio::sync::mpsc::unbounded_channel();
    let mut driver = Driver::new(silent_cdp().await?, SessionId::new("S-page"), watch);
    let request = json!({"method": "POST", "url": "http://127.0.0.1/api/send"});
    for (id, wall_time) in [
        ("1", json!(1_792_527_333.116_4)),
        ("2", Value::Null),
        ("3", json!(-1.0)),
        ("4", json!("1792527333")),
    ] {
        events.send(network_event(
            "Network.requestWillBeSent",
            json!({"requestId": id, "request": request, "wallTime": wall_time}),
        ))?;
    }
    driver.seen_so_far();
    let times: Vec<Option<u64>> = driver.requests().iter().map(|seen| seen.sent_ms).collect();
    assert_eq!(times, [Some(1_792_527_333_116), None, None, None]);
    Ok(())
}

/// A page whose box tells Shift+Enter (a line break) from Enter (a send), as the web client's compose box does.
const LINES_PAGE: &str = r#"<!doctype html><html><body>
<div id="out"></div>
<div contenteditable="true" role="textbox" data-tid="box" id="box"></div>
<script>
document.getElementById('box').addEventListener('keydown', (event) => {
  if (event.key !== 'Enter') return;
  event.preventDefault();
  document.getElementById('out').textContent += event.shiftKey ? '[break]' : '[send]';
});
</script></body></html>"#;

#[tokio::test]
async fn shift_enter_is_a_line_break_and_not_a_send() -> TestResult {
    with_html(LINES_PAGE, async |run| {
        run.driver.type_into("type", Selector::Tid("box"), "one").await?;
        // a space pressed as a key types one
        run.driver.press("space", Key::SPACE).await?;
        run.driver
            .wait_until_true(
                "spaced",
                "document.getElementById('box').textContent.replace('\\u00a0', ' ') === 'one '",
                "the box to hold 'one '",
                Duration::from_secs(5),
            )
            .await?;
        run.driver.press("break", Key::SHIFT_ENTER).await?;
        run.driver.press_enter("send").await?;
        run.driver
            .wait_for_text("keys", "[break][send]", Duration::from_secs(5))
            .await?;
        Ok(())
    })
    .await
}

/// A page with a message's hover bar as the web client's bundle builds it: quick reactions marked
/// `message-hover-reactions` (one hidden), the same reaction again in the message's menu (`message-reaction`), and other
/// actions; and a typing indicator that shows after 1.5 s, hides by `aria-hidden` at 2 s, shows again at 2.5 s and goes
/// from the page at 3 s.
const SHAPES_PAGE: &str = r#"<!doctype html><html><body>
<div data-tid="chat-pane-message">
  <button data-tid="message-actions-like" data-inp="message-hover-reactions">like</button>
  <button data-tid="message-actions-heart" data-inp="message-hover-reactions">heart</button>
  <button data-tid="message-actions-angry" data-inp="message-hover-reactions" style="display: none">angry</button>
  <button data-tid="message-hover-action-edit">edit</button>
  <button data-tid="message-actions-more">more</button>
  <div role="menu"><button data-tid="message-actions-like" data-inp="message-reaction">like</button>
  <button data-tid="message-actions-surprised" data-inp="message-reaction">surprised</button></div>
</div>
<script>
const show = () => {
  const e = document.createElement('div'); e.dataset.tid = 'typing-indicator'; e.id = 'typing'; e.textContent = 'typing';
  document.body.append(e);
};
setTimeout(show, 1500);
setTimeout(() => document.getElementById('typing').setAttribute('aria-hidden', 'true'), 2000);
setTimeout(() => document.getElementById('typing').removeAttribute('aria-hidden'), 2500);
setTimeout(() => document.getElementById('typing').remove(), 3000);
</script></body></html>"#;

#[tokio::test]
async fn chat_shapes_reads_the_hover_bar_s_quick_reactions_once_each() -> TestResult {
    use proto_lab::scenario::chat_shapes::HOVER_REACTIONS;
    with_html(SHAPES_PAGE, async |run| {
        let expected = r#"["message-actions-like","message-actions-heart","message-actions-surprised"]"#;
        run.driver
            .wait_until_true(
                "reactions",
                &format!("JSON.stringify({HOVER_REACTIONS}) === {expected:?}"),
                "the visible quick reactions, each once",
                Duration::from_secs(5),
            )
            .await?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn chat_shapes_records_when_the_typing_indicator_shows_and_goes() -> TestResult {
    use proto_lab::scenario::chat_shapes::TYPING_RECORDER;
    with_html(SHAPES_PAGE, async |run| {
        let drive = &mut run.driver;
        drive
            .wait_until_true("install", TYPING_RECORDER, "the recorder", Duration::from_secs(5))
            .await?;
        // installed twice, it keeps its log
        drive
            .wait_until_true("again", TYPING_RECORDER, "the recorder", Duration::from_secs(5))
            .await?;
        let changes = "JSON.stringify((window.__labTyping ?? []).map((change) => change[1])) === \
                       '[true,false,true,false]' && window.__labTyping.every((change, i, all) => \
                       Number.isInteger(change[0]) && (i === 0 || change[0] >= all[i - 1][0]))";
        drive
            .wait_until_true(
                "changes",
                changes,
                "shown, hidden, shown, gone",
                Duration::from_secs(10),
            )
            .await?;
        Ok(())
    })
    .await
}

/// A page that reports where it was clicked and how far the wheel turned over it, and whose send button names the
/// conversation it is in.
const POINTER_PAGE: &str = r#"<!doctype html><html><body style="margin:0">
<div id="pad" style="width: 400px; height: 400px; overflow: scroll"><div style="height: 4000px"></div></div>
<div id="out"></div>
<button data-tid="sendMessageCommands-send" data-track-thread-id="19:synthetic-shown@thread.v2">send</button>
<script>
const out = document.getElementById('out');
document.getElementById('pad').addEventListener('click', (event) => {
  out.textContent = 'clicked ' + Math.round(event.clientX) + ',' + Math.round(event.clientY);
});
document.getElementById('pad').addEventListener('wheel', (event) => {
  out.textContent = 'wheel ' + Math.round(event.deltaY);
});
</script></body></html>"#;

#[tokio::test]
async fn a_viewport_point_is_clicked_and_wheeled_as_a_pointer_would() -> TestResult {
    with_html(POINTER_PAGE, async |run| {
        run.driver.pointer_click_at("click the pad", 120.0, 80.0).await?;
        run.driver
            .wait_for_text("the click", "clicked 120,80", Duration::from_secs(5))
            .await?;
        run.driver.wheel_at("turn the wheel", 120.0, 80.0, 240.0).await?;
        run.driver
            .wait_for_text("the wheel", "wheel 240", Duration::from_secs(5))
            .await?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn a_chat_preflight_refuses_another_selected_conversation_before_reading_recipients() -> TestResult {
    with_html(POINTER_PAGE, async |run| {
        let error = run
            .driver
            .preflight_chat("the call's chat", "19:synthetic-intended@thread.v2")
            .await
            .err()
            .ok_or("a different selected conversation passed")?;
        assert_eq!(error.step(), "the call's chat");
        assert!(
            error
                .to_string()
                .contains("selected conversation differs from the intended lab target"),
            "{error}"
        );
        // the shown conversation passes the selection check, and with no lab action targets the recipients can't
        // be read
        let error = run
            .driver
            .preflight_chat("the shown chat", "19:synthetic-shown@thread.v2")
            .await
            .err()
            .ok_or("a chat passed without its recipients")?;
        assert!(
            error.to_string().contains("no explicit lab action target context"),
            "{error}"
        );
        Ok(())
    })
    .await
}

#[tokio::test]
async fn a_chat_preflight_needs_exactly_one_selected_conversation() -> TestResult {
    with_page(async |run| {
        let error = run
            .driver
            .preflight_chat("no chat open", "19:synthetic-intended@thread.v2")
            .await
            .err()
            .ok_or("a page without a selected conversation passed")?;
        assert!(matches!(error, StepError::Failed { .. }), "{error:?}");
        assert!(
            error
                .to_string()
                .contains("no unambiguous selected conversation identity"),
            "{error}"
        );
        Ok(())
    })
    .await
}

#[tokio::test]
async fn a_driver_navigation_the_browser_never_answers_is_a_timeout_of_its_step() -> TestResult {
    let (_events, watch) = tokio::sync::mpsc::unbounded_channel();
    let mut driver = Driver::new(silent_cdp().await?, SessionId::new("S-page"), watch);
    driver.set_call_timeout(Duration::from_millis(300));
    let error = tokio::time::timeout(
        Duration::from_secs(10),
        driver.navigate("open the chat", "http://127.0.0.1/page?synthetic-secret=1"),
    )
    .await
    .map_err(|_| "an unanswered navigation outlived its bound")?
    .err()
    .ok_or("an unanswered navigation succeeded")?;
    assert!(matches!(error, StepError::Timeout { .. }), "{error:?}");
    assert_eq!(error.step(), "open the chat");
    let text = error.to_string();
    assert!(text.contains("Page.navigate to http://127.0.0.1/page"), "{text}");
    assert!(!text.contains("synthetic-secret"), "{text}");
    Ok(())
}

#[test]
fn every_step_error_names_its_step() {
    let errors = [
        StepError::Timeout {
            step: "waited".to_owned(),
            waited: Duration::from_secs(1),
            waiting_for: "something".to_owned(),
        },
        StepError::Cdp {
            step: "asked".to_owned(),
            source: CdpError::Closed,
        },
        StepError::Closed {
            step: "gone".to_owned(),
        },
        StepError::Failed {
            step: "refused".to_owned(),
            reason: "a reason".to_owned(),
        },
    ];
    let steps: Vec<&str> = errors.iter().map(StepError::step).collect();
    assert_eq!(steps, ["waited", "asked", "gone", "refused"]);
    for error in &errors {
        assert!(
            error.to_string().starts_with(&format!("step `{}`", error.step())),
            "{error}"
        );
    }
}
