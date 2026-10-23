//! The reviewed expected-deny list of the writing scenarios (`proto_lab::scenario::expected`): it changes only how a
//! run is judged, never a verdict; it holds only contacting-service classes the fence denies under the lab rule (bot
//! invokes, the account's own channel entry and channel properties, Trouter delivery responses); and whatever it says,
//! a deny on the scenario's mapped target, a deny that isn't `Unclassified`, a deny of what the lab rule lets through
//! and any class off the list stay unexpected, the scenario's own labelled forced denies the only exception. All URLs
//! and ids here are made up.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use proto_lab::classify::{Classification, Classifier};
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
/// A made-up team channel, plain and as the web client encodes it.
const CHANNEL: &str = "19:0123456789abcdef0123456789abcdef@thread.tacv2";
const CHANNEL_ENCODED: &str = "19%3A0123456789abcdef0123456789abcdef%40thread.tacv2";

/// One request of every class on the list, with a made-up region and ids, on both of the web client's origins.
fn listed_classes() -> Vec<(&'static str, String)> {
    let mut classes = Vec::new();
    for origin in ["https://teams.microsoft.com", "https://teams.cloud.microsoft"] {
        let chat_service = format!("{origin}/api/chatsvc/emea/v1");
        classes.push(("POST", format!("{chat_service}/agents/28:made-up-bot/invoke")));
        for channel in [CHANNEL, CHANNEL_ENCODED] {
            classes.push(("PUT", format!("{chat_service}/threads/{channel}/members/{LAB_A}")));
            classes.push((
                "PUT",
                format!("{chat_service}/users/ME/conversations/{channel}/properties"),
            ));
        }
    }
    classes
}

fn denial(method: &str, url: &str, cause: DenyCause) -> Denial {
    Denial::new(method, url, cause)
}

fn frame(url: &str, cause: DenyCause) -> FrameDenial {
    FrameDenial::new(url, cause)
}

fn ab_chat_policy() -> TestResult<DenyPolicy> {
    Ok(ab_chat::policy(Some(AB_CHAT))?)
}

#[test]
fn every_listed_class_is_expected() -> TestResult {
    let policies = [ab_chat_policy()?, echo_call::policy()];
    for (method, url) in listed_classes() {
        let denied = denial(method, &url, DenyCause::Unclassified);
        assert!(request_class(&denied).is_some(), "no class for {method} {url}");
        for policy in &policies {
            assert!(policy.expects(&denied), "{method} {url}");
        }
        // the query is gone from a kept deny: it changes nothing
        let queried = denial(
            method,
            &format!("{url}?returnThreadMetadata=true"),
            DenyCause::Unclassified,
        );
        assert!(policies[0].expects(&queried), "{method} {url}?…");
    }
    Ok(())
}

/// The list holds only what the fence denies: every listed request, with any query or body, is unknown to the lab
/// rule, so a class can't stand in for one the rule lets through.
#[test]
fn every_listed_class_is_one_the_lab_rule_denies() {
    for (method, url) in listed_classes() {
        for query in ["", "?returnThreadMetadata=true", "?name=sortOrder"] {
            let url = format!("{url}{query}");
            assert_eq!(
                Classifier::new().classify(method, &url, &[], Some(b"{}")),
                Classification::Unknown,
                "{method} {url}"
            );
        }
    }
}

#[test]
fn the_channel_classes_take_a_channel_s_id_only() -> TestResult {
    let policy = ab_chat_policy()?;
    let chat_service = "https://teams.microsoft.com/api/chatsvc/emea/v1";
    for id in [
        // a group chat, a meeting chat and another 1:1 chat aren't channels
        "19%3A0123456789abcdef0123456789abcdef%40thread.v2",
        "19%3Ameeting_made-up%40thread.v2",
        "19%3A0000000a-0000-4000-8000-00000000000a_0000000d-0000-4000-8000-00000000000d%40unq.gbl.spaces",
        // nor is an id that only holds the suffix, or nothing before it
        "%40thread.tacv2",
        "19%3Ax%40thread.tacv2.example",
    ] {
        for url in [
            format!("{chat_service}/threads/{id}/members/{LAB_A}"),
            format!("{chat_service}/users/ME/conversations/{id}/properties"),
        ] {
            let denied = denial("PUT", &url, DenyCause::Unclassified);
            assert!(request_class(&denied).is_none(), "{url}");
            assert!(!policy.expects(&denied), "{url}");
        }
    }
    // the channel's id in another case is the channel all the same
    let upper = format!("{chat_service}/threads/19%3AX%40THREAD.TACV2/members/{LAB_A}");
    assert!(policy.expects(&denial("PUT", &upper, DenyCause::Unclassified)));
    // other methods and paths on a channel are no class of the list
    for (method, url) in [
        (
            "POST",
            format!("{chat_service}/users/ME/conversations/{CHANNEL_ENCODED}/messages"),
        ),
        (
            "DELETE",
            format!("{chat_service}/threads/{CHANNEL_ENCODED}/members/{LAB_A}"),
        ),
        ("PUT", format!("{chat_service}/threads/{CHANNEL_ENCODED}/members")),
    ] {
        assert!(
            !policy.expects(&denial(method, &url, DenyCause::Unclassified)),
            "{method} {url}"
        );
    }
    Ok(())
}

#[test]
fn trouter_frames_are_expected_by_their_socket_host_and_nothing_else_is() -> TestResult {
    let policy = ab_chat_policy()?;
    for url in [
        "wss://go.trouter.teams.microsoft.com/v4/c",
        "wss://pub-ent-zz-01-t.trouter.teams.microsoft.com/v4/c",
    ] {
        let denied = frame(url, DenyCause::Unclassified);
        assert_eq!(
            frame_class(&denied).map(|entry| entry.class),
            Some("Trouter delivery responses")
        );
        assert!(policy.expects_frame(&denied), "{url}");
        assert!(!policy.expects_frame(&frame(url, DenyCause::ForeignSocket)), "{url}");
        assert!(!policy.expects_frame(&frame(url, DenyCause::Forced)), "{url}");
    }
    for url in [
        // the lab rule lets these through: a deny of one means something is off
        "wss://augloop.office.com/",
        "wss://teams.microsoft.com/api/socket",
        "wss://trouter.teams.microsoft.com/v4/c",
        "wss://go.trouter.teams.microsoft.com.example.com/v4/c",
        "ws://go.trouter.teams.microsoft.com/v4/c",
        "wss://go.trouter.teams.microsoft.com:8443/v4/c",
        "not a url",
    ] {
        assert!(!policy.expects_frame(&frame(url, DenyCause::Unclassified)), "{url}");
    }
    assert!(DenyPolicy::AllExpected.expects_frame(&frame("not a url", DenyCause::ForeignSocket)));
    Ok(())
}

#[test]
fn a_forced_frame_deny_is_expected_only_under_a_label_the_scenario_names() -> TestResult {
    let held = |label: Option<&str>| FrameDenial {
        label: label.map(str::to_owned),
        ..frame("wss://go.trouter.teams.microsoft.com/v4/c", DenyCause::Forced)
    };
    let review = Review::new().expecting_forced(&["held"]);
    assert!(review.expects_frame(&held(Some("held"))));
    assert!(!review.expects_frame(&held(Some("other"))));
    assert!(!review.expects_frame(&held(None)));
    assert!(!ab_chat_policy()?.expects_frame(&held(Some("held"))));
    // a forced deny whose log entry was lost keeps its label, and fails the run all the same
    let lost = FrameDenial {
        cause: DenyCause::LogUnavailable,
        ..held(Some("held"))
    };
    assert!(!review.expects_frame(&lost));
    let lost_request = Denial {
        label: Some("held".to_owned()),
        ..denial("POST", "https://teams.microsoft.com/", DenyCause::LogUnavailable)
    };
    assert!(!review.expects(&lost_request));
    Ok(())
}

#[test]
fn a_listed_request_denied_for_another_cause_is_unexpected() -> TestResult {
    let policy = ab_chat_policy()?;
    let url = "https://teams.microsoft.com/api/chatsvc/emea/v1/agents/28:made-up-bot/invoke";
    for cause in [
        DenyCause::Fenced(DenyReason::Partial(PartialReason::NoSource)),
        DenyCause::Fenced(DenyReason::Partial(PartialReason::FetchFailed(FetchFailure::Malformed))),
        DenyCause::Fenced(DenyReason::Outsiders { count: 1 }),
        DenyCause::LogUnavailable,
        DenyCause::Malformed,
        DenyCause::ForeignSocket,
        DenyCause::Forced,
    ] {
        let denied = denial("POST", url, cause);
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
        // a message, an edit, a read receipt and a member change on the a–b chat: unclassified or refused
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
            format!("{chat_service}/agents/28:made-up-bot/invoke/{AB_CHAT_ENCODED}"),
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
    // echo-call has no probe of its own: only an earlier run's, sent again on load, is expected there
    assert!(!echo_call::policy().expects(&forced(Some(ab_chat::THIS_RUN_PROBE))));
    assert!(echo_call::policy().expects(&forced(Some(ab_chat::EARLIER_RUN_PROBE))));
    assert!(!echo_call::policy().expects(&forced(None)));
    Ok(())
}

/// What the lab rule lets through is no class of the list: a deny of any of it fails a writing run.
#[test]
fn a_deny_of_what_the_lab_rule_lets_through_is_unexpected() -> TestResult {
    let policies = [ab_chat_policy()?, echo_call::policy()];
    let teams = "https://teams.microsoft.com";
    for (method, url) in [
        (
            "POST",
            "https://browser.events.data.microsoft.com/OneCollector/1.0/".to_owned(),
        ),
        (
            "POST",
            "https://browser.pipe.aria.microsoft.com/Collector/3.0/".to_owned(),
        ),
        (
            "PUT",
            format!("{teams}/api/chatsvc/emea/v1/users/ME/properties?name=userDetails"),
        ),
        (
            "PUT",
            format!(
                "{teams}/api/chatsvc/emea/v1/users/ME/conversations/48%3Anotifications/messages/1/properties?name=isread"
            ),
        ),
        (
            "POST",
            format!("{teams}/api/mt/part/emea-02/beta/users/fetchShortProfile"),
        ),
        (
            "POST",
            format!("{teams}/api/mt/part/emea-02/beta/users/useraggregatesettings"),
        ),
        (
            "POST",
            format!("{teams}/api/csa/emea/api/v1/teams/users/me/chatsAndChannelsByIds"),
        ),
        ("POST", format!("{teams}/ups/emea/v1/presence/getpresence/")),
        ("PUT", format!("{teams}/ups/emea/v1/me/endpoints/")),
        ("PUT", format!("{teams}/ups/emea/v1/me/forceavailability/")),
        ("POST", format!("{teams}/registrar/prod/V2/registrations")),
        (
            "POST",
            "https://substrate.office.com/search/api/v1/suggestions".to_owned(),
        ),
        (
            "POST",
            "https://eu-prod.asyncgw.teams.microsoft.com/v1/skypetokenauth".to_owned(),
        ),
        ("POST", "https://augloop.office.com/".to_owned()),
    ] {
        assert_ne!(
            Classifier::new().classify(method, &url, &[], Some(br#"{"isread":"true"}"#)),
            Classification::Unknown,
            "{method} {url}: the lab rule denies it after all"
        );
        let denied = denial(method, &url, DenyCause::Unclassified);
        assert!(request_class(&denied).is_none(), "{method} {url}");
        for policy in &policies {
            assert!(!policy.expects(&denied), "{method} {url}");
        }
    }
    Ok(())
}

/// Each of `unlisted` is no class of the list, and neither scenario expects it.
fn assert_unlisted(unlisted: &[(&str, String)]) -> TestResult {
    let policies = [ab_chat_policy()?, echo_call::policy()];
    for (method, url) in unlisted {
        let denied = denial(method, url, DenyCause::Unclassified);
        assert!(request_class(&denied).is_none(), "{method} {url}");
        for policy in &policies {
            assert!(!policy.expects(&denied), "{method} {url}");
        }
    }
    Ok(())
}

#[test]
fn classes_off_the_list_and_lookalikes_stay_unexpected() -> TestResult {
    let chat_service = "https://teams.microsoft.com/api/chatsvc/emea/v1";
    assert_unlisted(&[
        // a write on another conversation, a call follow-up, a share, a Graph write: all unknown to the lab rule
        (
            "POST",
            format!("{chat_service}/users/ME/conversations/48%3Anotes/messages"),
        ),
        (
            "POST",
            format!("{chat_service}/users/ME/conversations/{CHANNEL_ENCODED}/messages"),
        ),
        (
            "POST",
            "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv/x/leave".to_owned(),
        ),
        ("POST", "https://graph.microsoft.com/v1.0/me/sendMail".to_owned()),
        // a bot invoke's lookalikes
        ("GET", format!("{chat_service}/agents/28:made-up-bot/invoke")),
        ("POST", format!("{chat_service}/agents/28:made-up-bot/invoke/more")),
        ("POST", format!("{chat_service}/agents/invoke")),
        (
            "POST",
            "https://teams.microsoft.com.example.com/api/chatsvc/emea/v1/agents/28:x/invoke".to_owned(),
        ),
        (
            "POST",
            "http://teams.microsoft.com/api/chatsvc/emea/v1/agents/28:x/invoke".to_owned(),
        ),
        (
            "POST",
            "https://teams.microsoft.com:8443/api/chatsvc/emea/v1/agents/28:x/invoke".to_owned(),
        ),
        ("POST", "not a url".to_owned()),
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
    let under = Hosts::Under("trouter.teams.microsoft.com");
    assert!(under.covers("go.trouter.teams.microsoft.com"));
    assert!(under.covers("GO.Trouter.Teams.Microsoft.com"));
    for host in [
        "trouter.teams.microsoft.com",
        ".trouter.teams.microsoft.com",
        "xtrouter.teams.microsoft.com",
        "trouter.teams.microsoft.com.example.com",
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
        body_unread: false,
        network_id: None,
    }
}

/// The list is the scenario's judgement only: the fence still denies, keeps and logs every request on it, and a mapped
/// write refused for a partial membership is denied and unexpected.
#[tokio::test]
async fn the_list_changes_only_the_judgement_never_the_verdict() -> TestResult {
    let lab = lab()?;
    let http = HttpFence::new(Classifier::new(), Arc::clone(&lab.fence), Arc::new(FailClosed));
    let classes = listed_classes();
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
    let http = HttpFence::new(Classifier::new(), fence, Arc::new(FailClosed));
    let classes = listed_classes();
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
    let invoke = denial(
        "POST",
        "https://teams.microsoft.com/api/chatsvc/emea/v1/agents/28:x/invoke",
        DenyCause::Unclassified,
    );
    assert!(review.expects(&invoke));
    // the list's lookups find the same entries as the review
    assert_eq!(
        expected::request_class(&invoke).map(|entry| entry.class),
        Some("bot invokes")
    );
}
