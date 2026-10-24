//! The `own-presence-*` scenarios' set-up (`proto_lab::scenario::own_presence`): what each holds back with a forced deny,
//! and which denies each expects. No browser: the live runs are the lab's (`docs/protocol/trouter.md`).

use clap::ValueEnum;
use proto_lab::fence_http::{Denial, DenyCause, ForcedDeny, PausedRequest};
use proto_lab::fence_ws::{Frame, FrameDenial, Payload};
use proto_lab::scenario::ScenarioName;
use proto_lab::scenario::ab_trouter::{HELD_ACTIVITY, hold_activity};
use proto_lab::scenario::own_presence::{self, HELD_ENDPOINT_PUT, HELD_MANUAL_STATUS, Hold, Report, policy};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const MANUAL_PUT: &str = "https://teams.microsoft.com/ups/noam/v1/me/forceavailability/";
const ENDPOINT_PUT: &str = "https://teams.microsoft.com/ups/noam/v1/me/endpoints/";
const NOTE_PUT: &str = "https://teams.microsoft.com/ups/noam/v1/me/publishnote";
const ACTIVITY: &str = r#"5:3+::{"name":"user.activity","args":[{"state":"active","cv":"made-up-cv"}]}"#;
const TROUTER: &str = "wss://pub-ent-zz-01-t.trouter.teams.microsoft.com/v4/c";

const REPORTS: [Report; 5] = [
    Report::Activity,
    Report::Endpoint,
    Report::Both,
    Report::ManualHeld,
    Report::Manual,
];

#[test]
fn each_report_is_its_own_scenario() {
    for report in REPORTS {
        assert_eq!(Report::of(report.scenario()), Some(report));
    }
    let named = ScenarioName::value_variants()
        .iter()
        .filter(|name| Report::of(**name).is_some())
        .count();
    assert_eq!(named, REPORTS.len());
    assert_eq!(Report::of(ScenarioName::AbTrouter), None);
}

#[test]
fn each_run_holds_back_only_what_it_leaves_out() {
    for report in REPORTS {
        let hold = report.hold();
        assert_eq!(
            hold == Some(Hold::EndpointPut),
            report == Report::Activity,
            "{report:?}"
        );
        assert_eq!(hold == Some(Hold::Activity), report == Report::Endpoint, "{report:?}");
        assert_eq!(
            hold == Some(Hold::ManualStatus),
            report == Report::ManualHeld,
            "{report:?}"
        );
        assert_eq!(report.endpoint(), report != Report::Activity, "{report:?}");
        assert_eq!(
            report.manual(),
            matches!(report, Report::ManualHeld | Report::Manual),
            "{report:?}"
        );
    }
}

#[test]
fn the_activity_hold_denies_every_user_activity_and_nothing_else() -> Result<(), regex::Error> {
    let rule = hold_activity()?;
    let frame = |url: &str, text: &str| Frame {
        url: url.to_owned(),
        payload: Payload::Text(text.to_owned()),
    };
    assert!(rule.matches(&frame(TROUTER, ACTIVITY)));
    assert!(rule.matches(&frame("wss://go.trouter.teams.microsoft.com/v4/c/?x=1", ACTIVITY)));
    for (url, text) in [
        (TROUTER, r#"5:4+::{"name":"ping"}"#),
        (TROUTER, "2::"),
        ("wss://augloop.office.com/", ACTIVITY),
        ("wss://trouter.teams.microsoft.com.example.com/v4/c", ACTIVITY),
    ] {
        assert!(!rule.matches(&frame(url, text)), "{url} {text}");
    }
    assert!(!rule.matches(&Frame {
        url: TROUTER.to_owned(),
        payload: Payload::Binary("AAAA".to_owned()),
    }));
    Ok(())
}

/// A forced deny of `url` under `label`.
fn forced(url: &str, label: &str) -> Denial {
    Denial {
        label: Some(label.to_owned()),
        ..Denial::new("PUT", url, DenyCause::Forced)
    }
}

#[test]
fn each_run_expects_only_its_own_hold_denied() -> Result<(), regex::Error> {
    for report in REPORTS {
        let policy = policy(report, None)?;
        assert_eq!(
            policy.expects(&forced(MANUAL_PUT, HELD_MANUAL_STATUS)),
            report == Report::ManualHeld,
            "{report:?}"
        );
        assert_eq!(
            policy.expects(&forced(ENDPOINT_PUT, HELD_ENDPOINT_PUT)),
            report == Report::Activity,
            "{report:?}"
        );
        let activity = FrameDenial {
            label: Some(HELD_ACTIVITY.to_owned()),
            ..FrameDenial::new(TROUTER, DenyCause::Forced)
        };
        assert_eq!(
            policy.expects_frame(&activity),
            report == Report::Endpoint,
            "{report:?}"
        );
        // presence passes under the lab rule: a deny of it, or of a status note, is never expected
        for url in [MANUAL_PUT, ENDPOINT_PUT, NOTE_PUT] {
            assert!(
                !policy.expects(&Denial::new("PUT", url, DenyCause::Unclassified)),
                "{report:?} {url}"
            );
        }
    }
    Ok(())
}

#[test]
fn the_holds_match_the_presence_requests_and_the_status_note_is_never_held() -> TestResult {
    let paused = |url: &str| PausedRequest {
        request_id: "1".to_owned(),
        method: "PUT".to_owned(),
        url: url.to_owned(),
        headers: Vec::new(),
        body: Some(b"{}".to_vec()),
        body_unread: false,
        network_id: None,
    };
    let endpoint = ForcedDeny::new("PUT", own_presence::ENDPOINT_PUT)?;
    let manual = ForcedDeny::new("PUT", own_presence::MANUAL_STATUS)?;
    assert!(endpoint.matches(&paused(ENDPOINT_PUT)));
    assert!(manual.matches(&paused(MANUAL_PUT)));
    assert!(!endpoint.matches(&paused(MANUAL_PUT)));
    assert!(!manual.matches(&paused(ENDPOINT_PUT)));
    for rule in [&endpoint, &manual] {
        assert!(!rule.matches(&paused(NOTE_PUT)));
    }
    Ok(())
}
