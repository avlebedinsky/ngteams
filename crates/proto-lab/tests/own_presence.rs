//! The `own-presence-*` scenarios' set-up (`proto_lab::scenario::own_presence`, M2 Task 18): which classifiers each
//! runs with, and which denies each expects. No browser: the live runs are the lab's (`docs/protocol/trouter.md`).

use clap::ValueEnum;
use proto_lab::classify::Classification;
use proto_lab::fence_http::{Denial, DenyCause};
use proto_lab::fence_ws::{Frame, Payload};
use proto_lab::scenario::ScenarioName;
use proto_lab::scenario::own_presence::{Report, policy};

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
fn only_the_activity_run_holds_the_endpoint_back_and_only_the_endpoint_run_the_activity() {
    let frame = Frame {
        url: TROUTER.to_owned(),
        payload: Payload::Text(ACTIVITY.to_owned()),
    };
    for report in REPORTS {
        let activity = matches!(report.frames().classify(&frame), Classification::Own(_));
        assert_eq!(activity, report != Report::Endpoint, "{report:?}");
        assert_eq!(report.endpoint(), report != Report::Activity, "{report:?}");
        assert_eq!(
            report.manual(),
            matches!(report, Report::ManualHeld | Report::Manual),
            "{report:?}"
        );
    }
}

#[test]
fn only_the_held_manual_run_expects_the_manual_status_denied() -> Result<(), regex::Error> {
    for report in REPORTS {
        let policy = policy(report, None)?;
        let manual = Denial::new("PUT", MANUAL_PUT, DenyCause::Unclassified);
        assert_eq!(policy.expects(&manual), report == Report::ManualHeld, "{report:?}");
        // the endpoint PUT is on ab-trouter's list in every run: the activity run denies it
        assert!(
            policy.expects(&Denial::new("PUT", ENDPOINT_PUT, DenyCause::Unclassified)),
            "{report:?}"
        );
        // what the account menu sends when it opens is expected in every run
        let shown = "https://teams.microsoft.com/api/mt/part/amer-03/beta/me/engagementSurfaces/displayed";
        assert!(
            policy.expects(&Denial::new("PUT", shown, DenyCause::Unclassified)),
            "{report:?}"
        );
        // a status note is never expected: a run whose client sent one fails
        assert!(
            !policy.expects(&Denial::new("PUT", NOTE_PUT, DenyCause::Unclassified)),
            "{report:?}"
        );
    }
    Ok(())
}
