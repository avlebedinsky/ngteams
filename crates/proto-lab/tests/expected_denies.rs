//! The reviewed expected-deny list of the writing scenarios (`proto_lab::scenario::expected`): it changes only how a
//! run is judged, never a verdict; it covers the classes the M0 acceptance runs showed; and whatever it says, a deny on
//! the scenario's mapped target, a deny that isn't `Unclassified` and any class off the list stay unexpected, with the
//! two `ab-chat` probes' forced denies as the only exception. All URLs and ids here are made up.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use proto_lab::classify::Classifier;
use proto_lab::config::Alias;
use proto_lab::fence_http::{Denial, DenyCause, HttpFence, PausedRequest, Verdict};
use proto_lab::fence_ws::FrameDenial;
use proto_lab::scenario::expected::{self, FRAMES, Hosts, REQUESTS, Review, frame_class, request_class};
use proto_lab::scenario::{AccountRun, DenyPolicy, ScenarioName, ScenarioReport, ab_chat, echo_call};
use proto_lab::session::FenceReport;
use serde_json::Value;
use teams_fence::{DenyReason, FailClosed, Fence, FenceLog, FetchFailure, Mri, PartialReason, Roster};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const LAB_A: &str = "8:orgid:0000000a-0000-4000-8000-00000000000a";
const LAB_B: &str = "8:orgid:0000000b-0000-4000-8000-00000000000b";
/// The a–b chat of the two made-up lab accounts.
const AB_CHAT: &str = "19:0000000a-0000-4000-8000-00000000000a_0000000b-0000-4000-8000-00000000000b@unq.gbl.spaces";
const AB_CHAT_ENCODED: &str =
    "19%3A0000000a-0000-4000-8000-00000000000a_0000000b-0000-4000-8000-00000000000b%40unq.gbl.spaces";

/// One request of every class the backlog item named (2026-10-20), with a made-up region, partition and ids, on both of
/// the web client's origins where it used both.
fn backlog_classes() -> Vec<(&'static str, String)> {
    let mut classes = Vec::new();
    for origin in ["https://teams.microsoft.com", "https://teams.cloud.microsoft"] {
        classes.push(("PUT", format!("{origin}/api/chatsvc/emea/v1/users/ME/properties")));
        for lookup in [
            "beta/users/fetchShortProfile",
            "beta/users/apps/aggregatedEntitlements",
            "beta/users/apps/eligibilities",
            "beta/users/apps/batchedDefinitions",
            "beta/teams/apps/aggregatedEntitlements",
            "beta/users/fetch",
            "beta/users/fetchfeaturesettings",
            "beta/users/useraggregatesettings",
            "beta/users/effectivePolicies",
            "beta/users/0000000c-0000-4000-8000-00000000000c/cookiev2",
            "beta/me/files/availability",
            "v1/mcps/initArtifactFolder",
            "v1/sharetoteams/installApp",
        ] {
            classes.push(("POST", format!("{origin}/api/mt/part/emea-02/{lookup}")));
        }
        for apps in [
            "users/apps/aggregatedEntitlements",
            "users/apps/eligibilities",
            "users/apps/batchedDefinitions",
            "teams/apps/aggregatedEntitlements",
        ] {
            classes.push(("POST", format!("{origin}/api/apps/part/emea-02/beta/{apps}")));
        }
        classes.push((
            "PUT",
            format!("{origin}/api/mt/part/emea-02/beta/me/engagementSurfaces/actions"),
        ));
        classes.push(("POST", format!("{origin}/ups/emea/v1/presence/getpresence/")));
        classes.push((
            "POST",
            format!("{origin}/api/tps/api/v1/teams/users/me/metadata/pushupdates"),
        ));
        classes.push((
            "POST",
            format!("{origin}/api/csa/emea/api/v1/teams/users/me/chatsAndChannelsByIds"),
        ));
    }
    for (method, url) in [
        (
            "POST",
            "https://eu-prod.asyncgw.teams.microsoft.com/v1/0000000d-0000-4000-8000-00000000000d/aadtokenauth",
        ),
        ("POST", "https://eu-prod.asyncgw.teams.microsoft.com/v1/skypetokenauth"),
        ("POST", "https://eu-api.asm.skype.com/v1/skypetokenauth"),
        ("POST", "https://substrate.office.com/search/api/v1/suggestions"),
        ("POST", "https://editor.svc.cloud.microsoft/NLEditor/Config/V2"),
        (
            "POST",
            "https://editor.svc.cloud.microsoft/NLEditor/api/V1/LanguageInfo",
        ),
        ("POST", "https://augloop.office.com/"),
        ("POST", "https://admin.microsoft.com/api/instrument/logclient"),
        ("POST", "https://browser.pipe.aria.microsoft.com/Collector/3.0/"),
        ("POST", "https://browser.events.data.microsoft.com/OneCollector/1.0/"),
        ("POST", "https://teams.events.data.microsoft.com/OneCollector/1.0/"),
        ("POST", "https://ic3.events.data.microsoft.com/OneCollector/1.0/"),
    ] {
        classes.push((method, url.to_owned()));
    }
    classes
}

fn denial(method: &str, url: &str, cause: DenyCause) -> Denial {
    Denial::new(method, url, cause)
}

fn frame(url: &str, cause: DenyCause) -> FrameDenial {
    FrameDenial {
        url: url.to_owned(),
        cause,
    }
}

fn ab_chat_policy() -> TestResult<DenyPolicy> {
    Ok(ab_chat::policy(Some(AB_CHAT))?)
}

#[test]
fn every_class_the_backlog_item_named_is_expected() -> TestResult {
    let policies = [ab_chat_policy()?, echo_call::policy()];
    for (method, url) in backlog_classes() {
        let denied = denial(method, &url, DenyCause::Unclassified);
        assert!(request_class(&denied).is_some(), "no class for {method} {url}");
        for policy in &policies {
            assert!(policy.expects(&denied), "{method} {url}");
        }
        // the query is gone from a kept deny: it changes nothing
        let queried = denial(method, &format!("{url}?x=1"), DenyCause::Unclassified);
        assert!(policies[0].expects(&queried), "{method} {url}?x=1");
    }
    Ok(())
}

#[test]
fn the_compose_boxs_bot_invokes_are_expected_and_nothing_like_them() -> TestResult {
    // seen in ab-chat-full (2026-10-20): `composeExtension/fetchCommands` to two bots when an editor opened
    let policies = [ab_chat_policy()?, echo_call::policy()];
    let invoke =
        "https://teams.cloud.microsoft/api/chatsvc/amer/v1/agents/28:00000000-0000-4000-8000-0000000000b0/invoke";
    for policy in &policies {
        assert!(policy.expects(&denial("POST", invoke, DenyCause::Unclassified)));
    }
    assert_unlisted(&[
        ("PUT", invoke),
        (
            "POST",
            "https://teams.cloud.microsoft/api/chatsvc/amer/v1/agents/28:00000000-0000-4000-8000-0000000000b0/invoke/x",
        ),
        (
            "POST",
            "https://teams.cloud.microsoft/api/chatsvc/amer/v1/agents/invoke",
        ),
        ("POST", "https://example.test/api/chatsvc/amer/v1/agents/28:x/invoke"),
    ])
}

#[test]
fn marking_an_own_feed_item_read_is_expected_and_nothing_like_it() -> TestResult {
    // seen in ab-chat-full (2026-10-20): `?name=isread` with `{"isread":"true"}` once b's reaction reached a's feed
    let policies = [ab_chat_policy()?, echo_call::policy()];
    let feed = "https://teams.cloud.microsoft/api/chatsvc/amer/v1/users/ME/conversations/48%3Anotifications/messages";
    let isread = format!("{feed}/1760000000000/properties");
    // and the mentions feed's, seen from b in ab-image (2026-10-21)
    let mentions = "https://teams.cloud.microsoft/api/chatsvc/amer/v1/users/ME/conversations/48%3Amentions/messages";
    let mention_read = format!("{mentions}/1760000000000/properties");
    for policy in &policies {
        assert!(policy.expects(&denial("PUT", &isread, DenyCause::Unclassified)));
        assert!(policy.expects(&denial("PUT", &mention_read, DenyCause::Unclassified)));
    }
    assert_unlisted(&[
        ("POST", &isread),
        ("PUT", &format!("{feed}/1760000000000")),
        ("POST", feed),
        ("POST", &mention_read),
        ("PUT", &format!("{mentions}/1760000000000")),
        (
            "PUT",
            "https://teams.cloud.microsoft/api/chatsvc/amer/v1/users/ME/conversations/48%3Acalllogs/messages/1/properties",
        ),
        (
            "PUT",
            "https://teams.cloud.microsoft/api/chatsvc/amer/v1/users/ME/conversations/19%3Aother%40thread.v2/messages/1/properties",
        ),
    ])
}

#[test]
fn marking_an_own_mention_read_is_expected_and_nothing_like_it() -> TestResult {
    // seen in ab-chat-full (2026-10-21): the same `isread` put on b's own mentions stream once a mentioned b
    let policies = [ab_chat_policy()?, echo_call::policy()];
    let mentions = "https://teams.cloud.microsoft/api/chatsvc/amer/v1/users/ME/conversations/48%3Amentions/messages";
    let isread = format!("{mentions}/1760000000000/properties");
    for policy in &policies {
        assert!(policy.expects(&denial("PUT", &isread, DenyCause::Unclassified)));
    }
    assert_unlisted(&[
        ("POST", &isread),
        ("DELETE", &isread),
        ("PUT", &format!("{mentions}/1760000000000")),
        ("POST", mentions),
        (
            "PUT",
            "https://teams.cloud.microsoft/api/chatsvc/amer/v1/users/ME/conversations/48%3Amentions/properties",
        ),
    ])
}

#[test]
fn frames_are_expected_by_their_socket_host() -> TestResult {
    let policy = ab_chat_policy()?;
    for url in [
        "wss://go.trouter.teams.microsoft.com/v4/c",
        "wss://pub-ent-xxxx-01-t.trouter.teams.microsoft.com/v4/c",
        "wss://augloop.office.com/",
    ] {
        let denied = frame(url, DenyCause::Unclassified);
        assert!(frame_class(&denied).is_some(), "{url}");
        assert!(policy.expects_frame(&denied), "{url}");
        // a frame sent through another realm's socket stays unexpected
        assert!(!policy.expects_frame(&frame(url, DenyCause::ForeignSocket)), "{url}");
    }
    for url in [
        "wss://teams.microsoft.com/api/socket",
        "wss://trouter.teams.microsoft.com/",
        "wss://augloop.office.com.example.com/",
        "ws://augloop.office.com/",
        "wss://augloop.office.com:8443/",
        "wss://go.trouter.teams.microsoft.com.example.com/",
        "https://augloop.office.com/",
        "not a url",
    ] {
        let denied = frame(url, DenyCause::Unclassified);
        assert!(frame_class(&denied).is_none(), "{url}");
        assert!(!policy.expects_frame(&denied), "{url}");
    }
    assert!(DenyPolicy::AllExpected.expects_frame(&frame("not a url", DenyCause::ForeignSocket)));
    Ok(())
}

#[test]
fn a_listed_request_denied_for_another_cause_is_unexpected() -> TestResult {
    let policy = ab_chat_policy()?;
    let url = "https://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/properties";
    for cause in [
        DenyCause::Fenced(DenyReason::Partial(PartialReason::NoSource)),
        DenyCause::Fenced(DenyReason::Partial(PartialReason::FetchFailed(FetchFailure::Malformed))),
        DenyCause::Fenced(DenyReason::Outsiders { count: 1 }),
        DenyCause::LogUnavailable,
        DenyCause::Malformed,
        DenyCause::ForeignSocket,
        DenyCause::Forced,
    ] {
        let denied = denial("PUT", url, cause);
        assert!(request_class(&denied).is_some());
        assert!(!policy.expects(&denied), "{cause:?}");
    }
    Ok(())
}

#[test]
fn a_mapped_write_stays_unexpected_whatever_the_list_says() -> TestResult {
    let policy = ab_chat_policy()?;
    let chat_service = "https://teams.microsoft.com/api/chatsvc/emea/v1";
    let on_the_chat = [
        // a message, an edit, a read receipt on the a–b chat: unclassified or refused
        (
            "POST",
            format!("{chat_service}/users/ME/conversations/{AB_CHAT_ENCODED}/messages"),
        ),
        (
            "PUT",
            format!("{chat_service}/users/ME/conversations/{AB_CHAT_ENCODED}/messages/1"),
        ),
        (
            "PUT",
            format!("{chat_service}/users/ME/conversations/{AB_CHAT}/properties"),
        ),
        (
            "PUT",
            format!("{chat_service}/threads/{AB_CHAT_ENCODED}/members/{LAB_B}"),
        ),
        // a listed path that names the a–b chat all the same
        (
            "POST",
            format!("https://eu-prod.asyncgw.teams.microsoft.com/v1/{AB_CHAT_ENCODED}/aadtokenauth"),
        ),
    ];
    for (method, url) in &on_the_chat {
        for cause in [
            DenyCause::Unclassified,
            DenyCause::Fenced(DenyReason::Partial(PartialReason::NoSource)),
        ] {
            let denied = denial(method, url, cause);
            assert!(!policy.expects(&denied), "{method} {url} {cause:?}");
        }
    }
    // the listed path itself is expected on any other id: the chat's name is what makes it unexpected
    let elsewhere = "https://eu-prod.asyncgw.teams.microsoft.com/v1/0000000d-0000-4000-8000-00000000000d/aadtokenauth";
    assert!(policy.expects(&denial("POST", elsewhere, DenyCause::Unclassified)));
    // without the chat's id the scenario can't run, and the list alone still leaves its message post off
    let unmapped = ab_chat::policy(None)?;
    assert!(!unmapped.expects(&denial("POST", &on_the_chat[0].1, DenyCause::Unclassified)));
    Ok(())
}

#[test]
fn only_ab_chats_own_probes_are_expected_among_forced_denies() -> TestResult {
    let policy = ab_chat_policy()?;
    let post =
        format!("https://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/conversations/{AB_CHAT_ENCODED}/messages");
    let forced = |label: Option<&str>| Denial {
        label: label.map(str::to_owned),
        ..denial("POST", &post, DenyCause::Forced)
    };
    assert_eq!(
        ab_chat::EXPECTED_FORCED,
        [ab_chat::THIS_RUN_PROBE, ab_chat::EARLIER_RUN_PROBE]
    );
    assert_ne!(ab_chat::THIS_RUN_PROBE, ab_chat::EARLIER_RUN_PROBE);
    // on the a–b chat, and expected all the same: the two probes
    assert!(policy.expects(&forced(Some(ab_chat::THIS_RUN_PROBE))));
    assert!(policy.expects(&forced(Some(ab_chat::EARLIER_RUN_PROBE))));
    // any other forced deny is not
    assert!(!policy.expects(&forced(None)));
    assert!(!policy.expects(&forced(Some("another rule"))));
    assert!(!policy.expects(&forced(Some(""))));
    // a probe's label on an ordinary deny is no exception
    let labelled = Denial {
        label: Some(ab_chat::THIS_RUN_PROBE.to_owned()),
        ..denial(
            "POST",
            &post,
            DenyCause::Fenced(DenyReason::Partial(PartialReason::NoSource)),
        )
    };
    assert!(!policy.expects(&labelled));
    // echo-call forces nothing: no probe of ab-chat's is expected there
    assert!(!echo_call::policy().expects(&forced(Some(ab_chat::THIS_RUN_PROBE))));
    Ok(())
}

/// Each of `unlisted` is no class of the list, and neither scenario expects it.
fn assert_unlisted(unlisted: &[(&str, &str)]) -> TestResult {
    let policies = [ab_chat_policy()?, echo_call::policy()];
    for &(method, url) in unlisted {
        let denied = denial(method, url, DenyCause::Unclassified);
        assert!(request_class(&denied).is_none(), "{method} {url}");
        for policy in &policies {
            assert!(!policy.expects(&denied), "{method} {url}");
        }
    }
    Ok(())
}

#[test]
fn classes_off_the_list_stay_unexpected() -> TestResult {
    assert_unlisted(&[
        // writes on another conversation (a tenant team's thread)
        (
            "PUT",
            "https://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/conversations/19%3Aother%40thread.tacv2/properties",
        ),
        (
            "PUT",
            "https://teams.microsoft.com/api/chatsvc/emea/v1/threads/19%3Aother%40thread.tacv2/members/8%3Aorgid%3Ax",
        ),
        // a chat create and a message elsewhere
        ("POST", "https://teams.microsoft.com/api/chatsvc/emea/v1/threads"),
        (
            "POST",
            "https://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/conversations/19%3Aother%40thread.v2/messages",
        ),
        // the calling stack's own state and the call's follow-ups (echo-call needs them)
        ("POST", "https://teams.microsoft.com/registrar/prod/V2/registrations"),
        ("PUT", "https://teams.microsoft.com/ups/emea/v1/me/endpoints/"),
        ("POST", "https://teams.microsoft.com/ups/emea/v1/pubsub/subscriptions/x"),
        (
            "POST",
            "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-x.conv.skype.com/conv/abc/add",
        ),
        (
            "POST",
            "https://api.flightproxy.teams.microsoft.com/api/v2/ep/cc-x.cc.skype.com/cc/v1/callAcceptance/x/acknowledge",
        ),
        // the chat service's endpoints
        (
            "PUT",
            "https://teams.microsoft.com/api/chatsvc/emea/v2/users/ME/endpoints/x",
        ),
    ])
}

#[test]
fn lookalikes_of_listed_classes_stay_unexpected() -> TestResult {
    // another method, host, scheme, port, depth, case, or an empty segment
    assert_unlisted(&[
        (
            "PATCH",
            "https://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/properties",
        ),
        (
            "DELETE",
            "https://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/properties",
        ),
        (
            "PUT",
            "https://teams.microsoft.com.example.com/api/chatsvc/emea/v1/users/ME/properties",
        ),
        (
            "PUT",
            "https://example.com/teams.microsoft.com/api/chatsvc/emea/v1/users/ME/properties",
        ),
        (
            "PUT",
            "http://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/properties",
        ),
        (
            "PUT",
            "https://teams.microsoft.com:8443/api/chatsvc/emea/v1/users/ME/properties",
        ),
        ("PUT", "https://teams.microsoft.com/api/chatsvc//v1/users/ME/properties"),
        (
            "PUT",
            "https://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/properties/x",
        ),
        (
            "PUT",
            "https://teams.microsoft.com/api/chatsvc/emea/x/v1/users/ME/properties",
        ),
        (
            "PUT",
            "https://teams.microsoft.com/api/chatsvc/emea/v1/users/me/properties",
        ),
        (
            "POST",
            "https://teams.microsoft.com/api/mt/part/emea-02/beta/users/fetchShortProfile/x",
        ),
        ("POST", "https://xevents.data.microsoft.com/OneCollector/1.0/"),
        ("POST", "https://events.data.microsoft.com/OneCollector/1.0/"),
        (
            "POST",
            "https://notevents.data.microsoft.com.example.com/OneCollector/1.0/",
        ),
        ("POST", "https://browser.events.data.microsoft.com/OneCollector/2.0/"),
        ("POST", "https://asyncgw.teams.microsoft.com/v1/skypetokenauth"),
        ("POST", "https://augloop.office.com/x"),
        ("POST", "not a url"),
    ])
}

#[test]
fn every_entry_says_what_it_is_and_why_it_isnt_needed() {
    let mut seen = std::collections::BTreeSet::new();
    for entry in REQUESTS {
        assert!(
            !entry.class.trim().is_empty() && !entry.reason.trim().is_empty(),
            "{entry:?}"
        );
        assert!(seen.insert(entry.class), "class {:?} twice", entry.class);
        assert_eq!(entry.method, entry.method.to_ascii_uppercase(), "{entry:?}");
        assert!(
            !["GET", "HEAD", "OPTIONS"].contains(&entry.method),
            "a read is never denied: {entry:?}"
        );
        assert!(entry.path.starts_with('/'), "{entry:?}");
        assert!(!entry.path.contains("**") && !entry.path.contains('?'), "{entry:?}");
        assert_hosts(entry.hosts);
    }
    for entry in FRAMES {
        assert!(
            !entry.class.trim().is_empty() && !entry.reason.trim().is_empty(),
            "{entry:?}"
        );
        assert!(seen.insert(entry.class), "class {:?} twice", entry.class);
        assert_hosts(entry.hosts);
    }
}

fn assert_hosts(hosts: Hosts) {
    match hosts {
        Hosts::Exact(hosts) => {
            assert!(!hosts.is_empty());
            for host in hosts {
                assert!(host.contains('.') && !host.starts_with('.'), "{host}");
            }
        }
        // a whole public suffix would be far too wide
        Hosts::Under(domain) => assert!(domain.matches('.').count() >= 2, "{domain}"),
    }
}

#[test]
fn hosts_match_on_a_dot_boundary() {
    let under = Hosts::Under("events.data.microsoft.com");
    assert!(under.covers("browser.events.data.microsoft.com"));
    assert!(under.covers("BROWSER.Events.Data.Microsoft.com"));
    for host in [
        "events.data.microsoft.com",
        ".events.data.microsoft.com",
        "xevents.data.microsoft.com",
        "events.data.microsoft.com.example.com",
        "",
    ] {
        assert!(!under.covers(host), "{host:?}");
    }
    let exact = Hosts::Exact(&["teams.microsoft.com"]);
    assert!(exact.covers("teams.microsoft.com") && exact.covers("Teams.Microsoft.com"));
    assert!(!exact.covers("api.teams.microsoft.com") && !exact.covers("teams.microsoft.com.example.com"));
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

fn log_entries(path: &Path) -> TestResult<Vec<Value>> {
    let text = std::fs::read_to_string(path)?;
    Ok(text
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<Vec<Value>, _>>()?)
}

fn paused(method: &str, url: &str, body: Option<&str>) -> PausedRequest {
    PausedRequest {
        request_id: "interception-job-1.0".to_owned(),
        headers: Vec::new(),
        method: method.to_owned(),
        url: url.to_owned(),
        body: body.map(|body| body.as_bytes().to_vec()),
        network_id: None,
    }
}

/// The list is the scenario's judgement only: the fence (`ab-chat`'s classifier) still denies, keeps and logs every
/// request on it, and a mapped write refused for a partial membership is denied and unexpected.
#[tokio::test]
async fn the_list_changes_only_the_judgement_never_the_verdict() -> TestResult {
    let lab = lab()?;
    let http = HttpFence::new(Classifier::mapped(), Arc::clone(&lab.fence), Arc::new(FailClosed));
    let classes = backlog_classes();
    for (method, url) in &classes {
        assert_eq!(
            http.decide(&paused(method, url, Some("{}"))).await,
            Verdict::Deny(DenyCause::Unclassified),
            "{method} {url}"
        );
    }
    let post =
        format!("https://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/conversations/{AB_CHAT_ENCODED}/messages");
    let verdict = http
        .decide(&paused("POST", &post, Some(r#"{"messagetype":"Text","content":"hi"}"#)))
        .await;
    assert!(
        matches!(verdict, Verdict::Deny(DenyCause::Fenced(DenyReason::Partial(_)))),
        "{verdict:?}"
    );

    let denials = http.denials();
    assert_eq!(denials.len(), classes.len() + 1);
    let entries = log_entries(&lab.log_path)?;
    assert_eq!(entries.len(), denials.len(), "every deny is in fence.log");
    assert!(entries.iter().all(|entry| entry["verdict"] == "deny"), "{entries:?}");
    for (method, url) in &classes {
        assert!(
            entries
                .iter()
                .any(|entry| entry["method"] == *method && entry["url"] == *url),
            "{method} {url} not logged"
        );
    }

    // judged: the listed ones expected, the mapped write not
    let policy = ab_chat_policy()?;
    let unexpected = policy.unexpected(&denials);
    assert_eq!(unexpected.len(), 1, "{unexpected:?}");
    assert_eq!(unexpected[0].url, post);
    let mut report = ScenarioReport::new(ScenarioName::AbChat);
    report.add_account(
        AccountRun {
            alias: Alias::A,
            fence: FenceReport::default(),
            denials,
            frame_denials: vec![frame(
                "wss://go.trouter.teams.microsoft.com/v4/c",
                DenyCause::Unclassified,
            )],
            capture: None,
        },
        &policy,
    );
    assert_eq!(report.failures.len(), 1, "{:?}", report.failures);
    assert!(report.failures[0].contains("/messages"), "{:?}", report.failures);
    Ok(())
}

/// A log sink that takes nothing.
struct Refuses;

impl std::io::Write for Refuses {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("disk full"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A listed request whose deny `fence.log` won't take is still denied, but as `LogUnavailable`: the lost audit entry
/// fails the run instead of hiding behind the list.
#[tokio::test]
async fn a_listed_deny_the_log_refuses_is_unexpected() -> TestResult {
    let fence = Arc::new(Fence::new(
        Roster::new([Mri::new(LAB_A), Mri::new(LAB_B)], []),
        FenceLog::from_writer(Refuses),
    ));
    let http = HttpFence::new(Classifier::mapped(), fence, Arc::new(FailClosed));
    let classes = backlog_classes();
    for (method, url) in &classes {
        assert_eq!(
            http.decide(&paused(method, url, Some("{}"))).await,
            Verdict::Deny(DenyCause::LogUnavailable),
            "{method} {url}"
        );
    }
    let denials = http.denials();
    assert_eq!(denials.len(), classes.len());
    assert_eq!(ab_chat_policy()?.unexpected(&denials).len(), denials.len());
    Ok(())
}

#[test]
fn the_list_alone_expects_no_forced_deny_and_maps_nothing() {
    let review = Review::new();
    let probe = Denial {
        label: Some(ab_chat::THIS_RUN_PROBE.to_owned()),
        ..denial("POST", "https://teams.microsoft.com/", DenyCause::Forced)
    };
    assert!(!review.expects(&probe));
    assert!(review.expects(&denial("POST", "https://augloop.office.com/", DenyCause::Unclassified)));
    // the list's lookups find the same entries as the review
    let denied = denial("POST", "https://augloop.office.com/", DenyCause::Unclassified);
    assert_eq!(
        expected::request_class(&denied).map(|entry| entry.class),
        Some("AugLoop posts")
    );
}
