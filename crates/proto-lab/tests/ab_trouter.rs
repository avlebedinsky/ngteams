//! The `ab-trouter` scenario's offline parts (`docs/protocol/trouter.md`): its names, what the quiet variant holds
//! back, how it judges its denies, and its registration checks (also on a local page in a throwaway armed
//! browser). All URLs and ids here are made up.

mod common;

use std::sync::Arc;
use std::time::Duration;

use proto_lab::arming::{ArmOptions, ArmedBrowser};
use proto_lab::browser::{Browser, LaunchOptions};
use proto_lab::cdp::SessionId;
use proto_lab::classify::{Classification, Classifier};
use proto_lab::fence_http::{Denial, DenyCause, ForcedDeny, HttpFence};
use proto_lab::fence_ws::{Frame, FrameDenial, GuardScript, Payload, WsFence, classify_frame};
use proto_lab::scenario::ab_trouter::{self, Activity};
use proto_lab::scenario::dsl::{Driver, SeenRequest, StepError};
use proto_lab::scenario::{ScenarioName, ab_chat};
use serde_json::{Value, json};
use teams_fence::{FailClosed, Fence, FenceLog, MembershipSource, Roster};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// The a–b chat of two made-up lab accounts.
const AB_CHAT: &str = "19:0000000a-0000-4000-8000-00000000000a_0000000b-0000-4000-8000-00000000000b@unq.gbl.spaces";

const REGIONAL: &str = "wss://pub-ent-zz-01-t.trouter.teams.microsoft.com/v4/c?tc=%7B%7D";

const ACTIVITY: &str = r#"5:4+::{"name":"user.activity","args":[{"state":"active","cv":"SyntheticCorrelation0.0.1"}]}"#;

fn denial(method: &str, url: &str) -> Denial {
    Denial::new(method, url, DenyCause::Unclassified)
}

#[test]
fn the_two_variants_have_their_own_names() {
    assert_eq!(ScenarioName::AbTrouter.as_str(), "ab-trouter");
    assert_eq!(ScenarioName::AbTrouterQuiet.to_string(), "ab-trouter-quiet");
    assert_eq!(ab_trouter::scenario(Activity::Reported), ScenarioName::AbTrouter);
    assert_eq!(ab_trouter::scenario(Activity::HeldBack), ScenarioName::AbTrouterQuiet);
}

#[test]
fn only_the_quiet_variant_holds_back_the_activity() -> TestResult {
    let frame = Frame {
        url: REGIONAL.to_owned(),
        payload: Payload::Text(ACTIVITY.to_owned()),
    };
    // the frame rule lets the client's own activity through; the quiet run force-denies it
    assert_eq!(classify_frame(&frame), Classification::Allow);
    assert!(ab_trouter::hold_activity()?.matches(&frame));
    let ping = Frame {
        url: REGIONAL.to_owned(),
        payload: Payload::Text(r#"5:5+::{"name":"ping"}"#.to_owned()),
    };
    assert!(!ab_trouter::hold_activity()?.matches(&ping));
    let held = FrameDenial {
        label: Some(ab_trouter::HELD_ACTIVITY.to_owned()),
        ..FrameDenial::new(REGIONAL, DenyCause::Forced)
    };
    assert!(ab_trouter::trouter_policy(Activity::HeldBack, Some(AB_CHAT))?.expects_frame(&held));
    assert!(!ab_trouter::trouter_policy(Activity::Reported, Some(AB_CHAT))?.expects_frame(&held));
    assert_eq!(
        ab_trouter::expected_forced(Activity::Reported),
        ab_chat::EXPECTED_FORCED
    );
    assert_eq!(
        ab_trouter::expected_forced(Activity::HeldBack),
        [
            ab_chat::THIS_RUN_PROBE,
            ab_chat::EARLIER_RUN_PROBE,
            ab_trouter::HELD_ACTIVITY
        ]
    );
    Ok(())
}

/// What comes up only once the push channel is (the endpoints' registrations, presence, the account's own feed) reaches
/// nobody: the lab rule lets it through, so a deny of it is no class the run expects.
#[test]
fn what_the_push_channel_brings_up_passes_and_its_deny_is_unexpected() -> TestResult {
    let teams = "https://teams.microsoft.com";
    for activity in [Activity::Reported, Activity::HeldBack] {
        let policy = ab_trouter::trouter_policy(activity, Some(AB_CHAT))?;
        for (method, url) in [
            ("POST", format!("{teams}/registrar/prod/V2/registrations")),
            (
                "POST",
                "https://teams.cloud.microsoft/registrar/prod/V2/registrations".to_owned(),
            ),
            ("PUT", format!("{teams}/ups/noam/v1/me/endpoints/")),
            ("POST", format!("{teams}/ups/noam/v1/pubsub/subscriptions/x")),
            ("POST", format!("{teams}/ups/noam/v1/presence/getpresence/")),
        ] {
            assert_eq!(
                Classifier::new().classify(method, &url, &[], Some(b"{}")),
                Classification::Allow,
                "{method} {url}"
            );
            assert!(!policy.expects(&denial(method, &url)), "{method} {url}");
        }
        // the list's classes hold
        let invoke =
            "https://teams.cloud.microsoft/api/chatsvc/amer/v1/agents/28:00000000-0000-4000-8000-0000000000aa/invoke";
        assert!(policy.expects(&denial("POST", invoke)));
        // a write on the a–b chat stays unexpected
        let chat = format!(
            "https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations/{}/messages",
            AB_CHAT.replace(':', "%3A").replace('@', "%40")
        );
        assert!(!policy.expects(&denial("POST", &chat)));
    }
    Ok(())
}

#[test]
fn the_edit_adds_no_space_and_the_reported_run_outlasts_a_delivery_s_lifetime() {
    let edited = ab_trouter::edited_text("n0");
    assert!(edited.starts_with(&ab_chat::message_text("n0")));
    assert_eq!(
        edited.matches(' ').count(),
        ab_chat::message_text("n0").matches(' ').count()
    );
    // a chat delivery carries `ttl=900` (seconds)
    assert!(ab_trouter::observation(Activity::Reported).as_secs() > 900);
    assert!(ab_trouter::observation(Activity::HeldBack) < ab_trouter::observation(Activity::Reported));
}

#[test]
fn denied_delivery_responses_are_expected_trouter_frames() -> TestResult {
    for activity in [Activity::Reported, Activity::HeldBack] {
        let policy = ab_trouter::trouter_policy(activity, Some(AB_CHAT))?;
        let denied = FrameDenial::new(
            "wss://pub-ent-zz-01-t.trouter.teams.microsoft.com/v4/c",
            DenyCause::Unclassified,
        );
        assert!(policy.expects_frame(&denied));
        let answer = Frame {
            url: REGIONAL.to_owned(),
            payload: Payload::Text(r#"3:::{"id":"-1","status":200}"#.to_owned()),
        };
        assert_eq!(classify_frame(&answer), Classification::Unknown);
    }
    Ok(())
}

fn seen(method: &str, url: &str, status: Option<u16>, failed: Option<&str>) -> SeenRequest {
    SeenRequest {
        session: None,
        request_id: "1000.1".to_owned(),
        method: method.to_owned(),
        url: url.to_owned(),
        status,
        failed: failed.map(str::to_owned),
        body: None,
        sent_ms: None,
    }
}

const REGISTRAR: &str = "https://teams.microsoft.com/registrar/prod/V2/registrations";

/// A registration body of the stack `app_id` (the captured shape, cut short), with a `note` telling the posts apart.
fn registration_body(app_id: &str, note: &str) -> String {
    json!({"clientDescription": {"appId": app_id, "templateKey": format!("{app_id}_2.6")}, "note": note}).to_string()
}

fn with_body(request: SeenRequest, body: &str) -> SeenRequest {
    SeenRequest {
        body: Some(body.to_owned()),
        ..request
    }
}

#[test]
fn only_the_messaging_stack_s_registration_answered_with_a_2xx_counts_as_registered() -> TestResult {
    let pattern = ab_trouter::registration()?;
    let messaging = registration_body("TeamsCDLWebWorker", "");
    assert!(messaging.contains(ab_trouter::MESSAGING_REGISTRATION), "{messaging}");
    assert!(ab_trouter::registered(
        &pattern,
        &with_body(seen("POST", REGISTRAR, Some(202), None), &messaging)
    ));
    for request in [
        // the calling stack's, answered: it passes under the lab rule but registers no messaging endpoint
        with_body(
            seen("POST", REGISTRAR, Some(202), None),
            &registration_body("SkypeSpacesWeb", ""),
        ),
        // a body the events left out and the browser didn't give
        seen("POST", REGISTRAR, Some(202), None),
        // denied: it fails in the page
        with_body(
            seen("POST", REGISTRAR, None, Some("net::ERR_BLOCKED_BY_CLIENT")),
            &messaging,
        ),
        // not answered yet, or answered otherwise
        with_body(seen("POST", REGISTRAR, None, None), &messaging),
        with_body(seen("POST", REGISTRAR, Some(400), None), &messaging),
        // another method or path
        with_body(seen("GET", REGISTRAR, Some(200), None), &messaging),
        with_body(seen("POST", &format!("{REGISTRAR}/x"), Some(202), None), &messaging),
    ] {
        assert!(!ab_trouter::registered(&pattern, &request), "{request:?}");
    }
    Ok(())
}

#[test]
fn the_notes_count_registrations_answered_and_denied_by_url() {
    let blocked = Some("net::ERR_BLOCKED_BY_CLIENT");
    let other = "https://teams.cloud.microsoft/registrar/prod/V2/registrations";
    let requests = [
        seen("POST", REGISTRAR, Some(202), None),
        // the calling stack's counts too: the notes go by URL
        with_body(
            seen("POST", REGISTRAR, Some(202), None),
            &registration_body("SkypeSpacesWeb", ""),
        ),
        seen("POST", REGISTRAR, None, blocked),
        seen("POST", REGISTRAR, None, blocked),
        seen("POST", other, None, blocked),
        seen("POST", REGISTRAR, None, None),
        seen("POST", "https://teams.microsoft.com/api/other", None, blocked),
    ];
    assert_eq!(
        ab_trouter::registrations_note("b", &requests),
        format!(
            "b: endpoint registrations: 2 answered with a 2xx; failed or answered otherwise: 1 × {other}, 2 × \
             {REGISTRAR}; 1 unanswered"
        )
    );
    assert_eq!(
        ab_trouter::registrations_note("a", &[]),
        "a: endpoint registrations: 0 answered with a 2xx; failed or answered otherwise: none; 0 unanswered"
    );
}

/// The registrar's path on a local server.
const LOCAL_REGISTRAR: &str = "/registrar/prod/V2/registrations";

/// How a registration's body is posted.
#[derive(Clone, Copy)]
enum Sent {
    /// As a string: the `Network` event carries it inline.
    Inline,
    /// As a `Blob`: the event leaves it out, and the wait reads it from the browser.
    Blob,
}

/// Posts a registration holding `body` from `page`, waiting for the page's fetch to end (answered or failed).
async fn post_registration(armed: &ArmedBrowser, page: &SessionId, body: &str, sent: Sent) -> TestResult {
    let body = Value::String(body.to_owned());
    let body = match sent {
        Sent::Inline => body.to_string(),
        Sent::Blob => format!("new Blob([{body}], {{type: 'application/json'}})"),
    };
    let expression = format!(
        "fetch({}, {{method: 'POST', body: {body}}}).then(r => r.status, e => String(e))",
        Value::String(LOCAL_REGISTRAR.to_owned()),
    );
    armed
        .cdp()
        .call(
            Some(page),
            "Runtime.evaluate",
            json!({"expression": expression, "awaitPromise": true, "returnByValue": true}),
        )
        .await?;
    Ok(())
}

/// b's registration waits on a local page: an earlier document's messaging registration answered `202`, then a retry
/// denied by the fence, then the calling stack's answered `202`, then the new document's messaging registration
/// answered `202`, its body left out of the event. A wait from the new document's start passes over the first three
/// and meets only the fourth; while only the denied and the calling stack's are there, it runs out.
#[tokio::test]
async fn a_registration_wait_meets_only_a_success_from_its_document_on() -> TestResult {
    let Some(chrome) = common::chrome_or_skip() else {
        return Ok(());
    };
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/page"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<!doctype html><title>lab</title>", "text/html"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(LOCAL_REGISTRAR))
        .respond_with(ResponseTemplate::new(202))
        .mount(&server)
        .await;
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
    let http = HttpFence::new(Classifier::new(), Arc::clone(&fence), Arc::clone(&members));
    let ws = WsFence::new(fence, members, GuardScript::generate()?);
    let options = ArmOptions {
        service_worker_origins: vec![server.uri()],
    };
    let armed = ArmedBrowser::arm(browser, http, ws, &options).await?;
    let outcome = tokio::time::timeout(Duration::from_secs(60), registration_run(&armed, &server)).await;
    armed.close(Duration::from_secs(10)).await?;
    outcome.map_err(|_| "the run took over 60 s")?
}

async fn registration_run(armed: &ArmedBrowser, server: &MockServer) -> TestResult {
    let page = armed.open_page(Duration::from_secs(15)).await?.session;
    let mut driver = Driver::new(armed.cdp().clone(), page.clone(), armed.watch());
    armed.force_deny(ForcedDeny::new("POST", "/registrar/")?.with_body_containing(r#""note":"denied""#));
    driver
        .navigate("open the page", &format!("{}/page", server.uri()))
        .await?;
    let messaging = |note: &str| registration_body("TeamsCDLWebWorker", note);

    post_registration(armed, &page, &messaging("first"), Sent::Inline).await?;
    let first = ab_trouter::wait_registered(&mut driver, "the first document registers", 0).await?;
    assert_eq!(first.status, Some(202), "{first:?}");
    post_registration(armed, &page, &messaging("denied"), Sent::Inline).await?;
    let denied = denied_registration(&mut driver).await?;
    // the new document begins with the denied retry: neither it nor the earlier success meets the wait
    let since = driver
        .requests()
        .iter()
        .position(|seen| seen.request_id == denied.request_id)
        .ok_or("the denied registration wasn't seen")?;
    no_success_yet(&mut driver, since).await?;
    let answered = calling_stack_passed_over(armed, &page, &mut driver, since).await?;

    let second = new_document_registers(armed, &page, &mut driver, since, &answered).await?;
    for other in [&first, &denied, &answered] {
        assert_ne!(second.request_id, other.request_id);
    }
    let note = ab_trouter::registrations_note("b", driver.requests());
    assert!(
        note.contains("3 answered with a 2xx") && note.contains(&format!("1 × {}{LOCAL_REGISTRAR}", server.uri())),
        "{note}"
    );
    Ok(())
}

/// Posts the new document's messaging registration as a `Blob`, which the event leaves out, and checks the wait from
/// `since` reads its body and meets it, past the calling stack's (`calling`, the only other one answered since).
async fn new_document_registers(
    armed: &ArmedBrowser,
    page: &SessionId,
    driver: &mut Driver,
    since: usize,
    calling: &SeenRequest,
) -> TestResult<SeenRequest> {
    let messaging = registration_body("TeamsCDLWebWorker", "second");
    post_registration(armed, page, &messaging, Sent::Blob).await?;
    let raw = answered_registration(driver, "the new document's is answered", since, Some(calling)).await?;
    assert_eq!(
        raw.body, None,
        "the event carried the Blob body inline: the read isn't tested"
    );
    let second = ab_trouter::wait_registered(driver, "the new document registers", since).await?;
    assert_eq!(second.status, Some(202), "{second:?}");
    assert_eq!(second.body.as_deref(), Some(messaging.as_str()));
    Ok(second)
}

/// Posts the calling stack's registration, which passes and is answered, and checks a wait from `since` still runs
/// out: it isn't the messaging endpoint's. Gives it.
async fn calling_stack_passed_over(
    armed: &ArmedBrowser,
    page: &SessionId,
    driver: &mut Driver,
    since: usize,
) -> TestResult<SeenRequest> {
    let calling = registration_body("SkypeSpacesWeb", "calling");
    post_registration(armed, page, &calling, Sent::Inline).await?;
    let answered = answered_registration(driver, "the calling stack's is answered", since, None).await?;
    assert_eq!(answered.body.as_deref(), Some(calling.as_str()));
    no_success_yet(driver, since).await?;
    Ok(answered)
}

/// Waits for a registration from the `since`th request on, other than `except`, answered `202` (by its URL, either
/// stack's), and gives it as the events tell it.
async fn answered_registration(
    driver: &mut Driver,
    step: &str,
    since: usize,
    except: Option<&SeenRequest>,
) -> TestResult<SeenRequest> {
    let pattern = ab_trouter::registration()?;
    let answered = driver
        .wait_for_seen(
            step,
            "a registration answered with a 202",
            since,
            Duration::from_secs(10),
            |seen| {
                pattern.matches(&seen.method, &seen.url)
                    && seen.status == Some(202)
                    && except.is_none_or(|except| seen.request_id != except.request_id)
            },
        )
        .await?;
    Ok(answered)
}

/// Waits for the retry to fail in the page as the fence blocked it, and gives it. (A fetch
/// answered `202` here was also reported failed, `net::ERR_ABORTED`: it still counts as answered, and isn't this.)
async fn denied_registration(driver: &mut Driver) -> TestResult<SeenRequest> {
    let pattern = ab_trouter::registration()?;
    let denied = driver
        .wait_for_seen(
            "the retry is denied",
            "a registration the fence blocked",
            0,
            Duration::from_secs(10),
            |seen| {
                pattern.matches(&seen.method, &seen.url)
                    && seen
                        .failed
                        .as_deref()
                        .is_some_and(|error| error.contains("ERR_BLOCKED_BY_CLIENT"))
            },
        )
        .await?;
    assert_eq!(denied.status, None, "{denied:?}");
    Ok(denied)
}

/// A wait for a registration answered with a 2xx, from the `since`th request on, runs out.
async fn no_success_yet(driver: &mut Driver, since: usize) -> TestResult {
    let pattern = ab_trouter::registration()?;
    let error = driver
        .wait_for_seen(
            "only the denied one",
            "a registration answered with a 2xx",
            since,
            Duration::from_millis(500),
            |seen| ab_trouter::registered(&pattern, seen),
        )
        .await
        .err()
        .ok_or("a denied registration, the calling stack's or an earlier document's met the wait")?;
    assert!(matches!(error, StepError::Timeout { .. }), "{error}");
    Ok(())
}
