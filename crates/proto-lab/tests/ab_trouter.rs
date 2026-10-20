//! The `ab-trouter` scenario's offline parts (M1 Task 14, `docs/protocol/trouter.md`): its names, the frame classifier
//! of each variant, how it judges its denies, and its registration checks (also on a local page in a throwaway armed
//! browser). All URLs and ids here are made up.

mod common;

use std::sync::Arc;
use std::time::Duration;

use proto_lab::arming::{ArmOptions, ArmedBrowser};
use proto_lab::browser::{Browser, LaunchOptions};
use proto_lab::cdp::SessionId;
use proto_lab::classify::{Classification, Classifier, OwnKind, OwnRule};
use proto_lab::fence_http::{Denial, DenyCause, ForcedDeny, HttpFence};
use proto_lab::fence_ws::{Frame, FrameClassifier, FrameDenial, GuardScript, Payload, WsFence};
use proto_lab::scenario::ab_trouter::{self, Activity, TROUTER_RUN_REQUESTS};
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
fn only_the_quiet_variant_holds_back_the_activity() {
    let frame = Frame {
        url: REGIONAL.to_owned(),
        payload: Payload::Text(ACTIVITY.to_owned()),
    };
    assert_eq!(
        ab_trouter::frames(Activity::Reported).classify(&frame),
        Classification::Own(OwnKind::TrouterSession)
    );
    assert_eq!(
        ab_trouter::frames(Activity::HeldBack).classify(&frame),
        Classification::Unknown
    );
    assert_eq!(ab_trouter::frames(Activity::Reported), FrameClassifier::messaging());
}

#[test]
fn the_run_expects_what_only_the_push_channel_brings_up() -> TestResult {
    let policy = ab_trouter::trouter_policy(Some(AB_CHAT))?;
    for (method, url) in [
        ("POST", "https://teams.microsoft.com/registrar/prod/V2/registrations"),
        ("POST", "https://teams.cloud.microsoft/registrar/prod/V2/registrations"),
        ("PUT", "https://teams.microsoft.com/ups/noam/v1/me/endpoints/"),
        ("POST", "https://teams.microsoft.com/ups/noam/v1/pubsub/subscriptions/x"),
        (
            "PUT",
            "https://teams.cloud.microsoft/api/chatsvc/amer/v1/users/ME/conversations/48%3Anotifications/messages/1700000000000/properties?name=isread",
        ),
        (
            "POST",
            "https://teams.cloud.microsoft/api/chatsvc/amer/v1/agents/28:00000000-0000-4000-8000-0000000000aa/invoke",
        ),
        // the list's classes still hold
        ("POST", "https://teams.microsoft.com/ups/noam/v1/presence/getpresence/"),
    ] {
        assert!(policy.expects(&denial(method, url)), "{method} {url}");
    }
    // and `ab-chat` keeps them unexpected
    let ab_chat = ab_chat::policy(Some(AB_CHAT))?;
    for entry in TROUTER_RUN_REQUESTS {
        let url = format!("https://teams.microsoft.com{}", entry.path.replace('*', "x"));
        assert!(!ab_chat.expects(&denial(entry.method, &url)), "{}", entry.class);
    }
    Ok(())
}

#[test]
fn the_run_s_own_classes_hold_only_as_the_list_s_do() -> TestResult {
    let policy = ab_trouter::trouter_policy(Some(AB_CHAT))?;
    for (method, url, cause) in [
        // another method, path or host
        (
            "PUT",
            "https://teams.microsoft.com/registrar/prod/V2/registrations",
            DenyCause::Unclassified,
        ),
        (
            "POST",
            "https://teams.microsoft.com/registrar/prod/V2/registrations/x",
            DenyCause::Unclassified,
        ),
        (
            "POST",
            "https://example.com/registrar/prod/V2/registrations",
            DenyCause::Unclassified,
        ),
        (
            "DELETE",
            "https://teams.microsoft.com/ups/noam/v1/me/endpoints/",
            DenyCause::Unclassified,
        ),
        // the feed's read state on a real chat, or an agent's other operations
        (
            "PUT",
            "https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations/19%3Ax%40thread.v2/messages/1/properties",
            DenyCause::Unclassified,
        ),
        (
            "POST",
            "https://teams.microsoft.com/api/chatsvc/amer/v1/agents/28:x/messages",
            DenyCause::Unclassified,
        ),
        // a deny for another cause than an unknown request
        (
            "POST",
            "https://teams.microsoft.com/registrar/prod/V2/registrations",
            DenyCause::Malformed,
        ),
    ] {
        assert!(
            !policy.expects(&Denial::new(method, url, cause)),
            "{method} {url} {cause:?}"
        );
    }
    // a write on the a–b chat stays unexpected
    let chat = format!(
        "https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations/{}/messages",
        AB_CHAT.replace(':', "%3A").replace('@', "%40")
    );
    assert!(!policy.expects(&denial("POST", &chat)));
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
fn every_own_class_says_what_it_is_and_why_it_isnt_needed() {
    assert!(!TROUTER_RUN_REQUESTS.is_empty());
    for entry in TROUTER_RUN_REQUESTS {
        assert!(!entry.class.is_empty() && !entry.reason.is_empty());
        assert!(entry.path.starts_with('/'), "{}", entry.class);
    }
}

#[test]
fn denied_delivery_responses_are_expected_trouter_frames() -> TestResult {
    let policy = ab_trouter::trouter_policy(Some(AB_CHAT))?;
    let denied = FrameDenial {
        url: "wss://pub-ent-zz-01-t.trouter.teams.microsoft.com/v4/c".to_owned(),
        cause: DenyCause::Unclassified,
    };
    assert!(policy.expects_frame(&denied));
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
    }
}

const REGISTRAR: &str = "https://teams.microsoft.com/registrar/prod/V2/registrations";

#[test]
fn only_a_registration_answered_with_a_2xx_counts_as_registered() -> TestResult {
    let pattern = ab_trouter::registration()?;
    assert!(ab_trouter::registered(
        &pattern,
        &seen("POST", REGISTRAR, Some(202), None)
    ));
    for request in [
        // the calling stack's retries, denied: they fail in the page
        seen("POST", REGISTRAR, None, Some("net::ERR_BLOCKED_BY_CLIENT")),
        // not answered yet, or answered otherwise
        seen("POST", REGISTRAR, None, None),
        seen("POST", REGISTRAR, Some(400), None),
        // another method or path
        seen("GET", REGISTRAR, Some(200), None),
        seen("POST", &format!("{REGISTRAR}/x"), Some(202), None),
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
        seen("POST", REGISTRAR, None, blocked),
        seen("POST", REGISTRAR, None, blocked),
        seen("POST", other, None, blocked),
        seen("POST", REGISTRAR, None, None),
        seen("POST", "https://teams.microsoft.com/api/other", None, blocked),
    ];
    assert_eq!(
        ab_trouter::registrations_note("b", &requests),
        format!(
            "b: endpoint registrations: 1 answered with a 2xx; failed or answered otherwise: 1 × {other}, 2 × \
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

/// Posts a registration holding `body` from `page`, waiting for the page's fetch to end (answered or failed).
async fn post_registration(armed: &ArmedBrowser, page: &SessionId, body: &str) -> TestResult {
    let expression = format!(
        "fetch({}, {{method: 'POST', body: {}}}).then(r => r.status, e => String(e))",
        Value::String(LOCAL_REGISTRAR.to_owned()),
        Value::String(body.to_owned())
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

/// b's registration waits on a local page: an earlier document's registration answered `202`, then a calling-stack
/// registration denied by the fence, then the new document's answered `202`. A wait from the new document's start
/// passes over the first two and meets only the third; while only the denied one is there, it runs out.
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
    let own = Classifier::seeded().with_own(OwnRule::new(
        OwnKind::EndpointRegistration,
        "POST",
        &server.uri(),
        LOCAL_REGISTRAR,
    ));
    let members: Arc<dyn MembershipSource + Send> = Arc::new(FailClosed);
    let http = HttpFence::new(own, Arc::clone(&fence), Arc::clone(&members));
    let ws = WsFence::new(FrameClassifier::seeded(), fence, members, GuardScript::generate()?);
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
    armed.force_deny(ForcedDeny::new("POST", "/registrar/")?.with_body_containing("SkypeSpacesWeb"));
    driver
        .navigate("open the page", &format!("{}/page", server.uri()))
        .await?;

    post_registration(armed, &page, "TeamsCDLWebWorker first").await?;
    let first = ab_trouter::wait_registered(&mut driver, "the first document registers", 0).await?;
    assert_eq!(first.status, Some(202), "{first:?}");
    post_registration(armed, &page, "SkypeSpacesWeb").await?;
    let denied = denied_registration(&mut driver).await?;
    // the new document begins with the denied retry: neither it nor the earlier success meets the wait
    let since = driver
        .requests()
        .iter()
        .position(|seen| seen.request_id == denied.request_id)
        .ok_or("the denied registration wasn't seen")?;
    no_success_yet(&mut driver, since).await?;

    post_registration(armed, &page, "TeamsCDLWebWorker second").await?;
    let second = ab_trouter::wait_registered(&mut driver, "the new document registers", since).await?;
    assert_eq!(second.status, Some(202), "{second:?}");
    assert_ne!(second.request_id, first.request_id);
    assert_ne!(second.request_id, denied.request_id);
    let note = ab_trouter::registrations_note("b", driver.requests());
    assert!(
        note.contains("2 answered with a 2xx") && note.contains(&format!("1 × {}{LOCAL_REGISTRAR}", server.uri())),
        "{note}"
    );
    Ok(())
}

/// Waits for the calling stack's registration to fail in the page as the fence blocked it, and gives it. (A fetch
/// answered `202` here was also reported failed, `net::ERR_ABORTED`: it still counts as answered, and isn't this.)
async fn denied_registration(driver: &mut Driver) -> TestResult<SeenRequest> {
    let pattern = ab_trouter::registration()?;
    let denied = driver
        .wait_for_seen(
            "the calling stack's is denied",
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
        .ok_or("a denied registration or an earlier document's met the wait")?;
    assert!(matches!(error, StepError::Timeout { .. }), "{error}");
    Ok(())
}
