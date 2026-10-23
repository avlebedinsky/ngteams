//! The `own-presence-*` scenarios (M2 Task 18, `docs/protocol/trouter.md`, "Own presence"): what makes account a show
//! online as b sees it, and what a manual status sends, under the user's exception of 2026-10-21 (CLAUDE.md: a lab
//! account may set its own availability, in its captured shapes, availability values only).
//!
//! Only account a's browser runs. b's view is read outside the scenario, by `teams-headless b presence` (UPS's
//! presence read, under b's own fence) while a's page is open and after it closes (`scripts/own-presence-run.sh`):
//! the scenario notes the Unix time of each step, so the reads can be lined up with it.
//!
//! Each runs with the **messaging set** of `ab-trouter` (the write mapping with members read live, the Trouter session's
//! own frames, the messaging stack's registration), and differs in what reports a's availability ([`Report`]):
//! - `own-presence-activity`: the Trouter `user.activity` frame alone; UPS's presence endpoint `PUT` stays denied (as in
//!   `ab-trouter`).
//! - `own-presence-endpoint`: the endpoint `PUT` alone ([`crate::availability`], on the endpoint the session registered);
//!   every `user.activity` held back (as in `ab-trouter-quiet`).
//! - `own-presence-both`: both.
//! - `own-presence-manual-held`: both, and a sets its status by hand (Busy, Do not disturb, Away, Available, then a
//!   reset) with the request each sends denied, to capture it.
//! - `own-presence-manual`: as `own-presence-manual-held`, the manual status let through once it is mapped.
//!
//! The steps:
//! 1. Before anything loads, a's fence force-denies every write carrying the probe marker, as in `ab-chat`.
//! 2. a opens the web client and must register its messaging endpoint, then opens the chat with b and proves
//!    enforcement live with this run's probe (`ab-chat`'s step 4).
//! 3. What reported a's availability is noted: with the endpoint `PUT` allowed, one must be answered with a 2xx within
//!    [`REPORT`] (in the manual runs none is only noted: the web client didn't send one in every run); with it denied,
//!    a denied one is noted (none is no failure).
//! 4. The manual runs set each status of [`STATUSES`] and then reset it, from the account menu, each held for
//!    [`STATUS_HOLD`]; the others hold the page open, nothing driven, for [`OBSERVE`].
//! 5. The notes count the endpoint `PUT`s and the manual status requests, answered and denied. Judged as `ab-trouter`
//!    ([`super::ab_trouter::trouter_policy`]), with what the account menu sends when it opens
//!    ([`OWN_PRESENCE_REQUESTS`]); the held manual run also expects the manual status denies ([`MANUAL_HELD_REQUESTS`]).
//!
//! Nothing in the run answers a call, sets a status note, out-of-office or a work location.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::browser::Media;
use crate::classify::Classifier;
use crate::config::{Alias, LabConfig, LabLocations, LabPaths};
use crate::fence_http::without_query;
use crate::fence_ws::FrameClassifier;
use crate::session::{LabSession, SessionOptions};

use super::ab_chat::{
    CHAT_OPEN, COMPOSE, EXPECTED_FORCED, chat_link, chat_url, deny_probes, enforce, load, nonce, one_to_one_id,
    probe_text, save_screenshot, setup,
};
use super::ab_trouter::{TROUTER_RUN_REQUESTS, size, wait_registered};
use super::dsl::{Driver, RequestPattern, SeenRequest, Selector};
use super::expected::{ExpectedRequest, Review, TEAMS};
use super::{DenyPolicy, RunOptions, ScenarioError, ScenarioName, ScenarioReport};

/// What reports a's availability in a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Report {
    /// The `user.activity` frame alone (`own-presence-activity`).
    Activity,
    /// The endpoint `PUT` alone (`own-presence-endpoint`).
    Endpoint,
    /// Both (`own-presence-both`).
    Both,
    /// Both, and a manual status set by hand, its requests denied (`own-presence-manual-held`).
    ManualHeld,
    /// Both, and a manual status set by hand, let through (`own-presence-manual`).
    Manual,
}

impl Report {
    /// The scenario's name.
    #[must_use]
    pub fn scenario(self) -> ScenarioName {
        match self {
            Self::Activity => ScenarioName::OwnPresenceActivity,
            Self::Endpoint => ScenarioName::OwnPresenceEndpoint,
            Self::Both => ScenarioName::OwnPresenceBoth,
            Self::ManualHeld => ScenarioName::OwnPresenceManualHeld,
            Self::Manual => ScenarioName::OwnPresenceManual,
        }
    }

    /// The report of the scenario `name`, if it is one of these.
    #[must_use]
    pub fn of(name: ScenarioName) -> Option<Self> {
        [
            Self::Activity,
            Self::Endpoint,
            Self::Both,
            Self::ManualHeld,
            Self::Manual,
        ]
        .into_iter()
        .find(|report| report.scenario() == name)
    }

    /// The request classifier: the messaging set, and the own availability but for [`Report::Activity`].
    #[must_use]
    pub fn classifier(self) -> Classifier {
        match self {
            Self::Activity => Classifier::messaging(),
            _ => Classifier::messaging().with_own_availability(),
        }
    }

    /// The frame classifier: the messaging set's, every `user.activity` held back for [`Report::Endpoint`].
    #[must_use]
    pub fn frames(self) -> FrameClassifier {
        match self {
            Self::Endpoint => FrameClassifier::messaging().without_activity(),
            _ => FrameClassifier::messaging(),
        }
    }

    /// Whether the endpoint `PUT` goes out.
    #[must_use]
    pub fn endpoint(self) -> bool {
        self != Self::Activity
    }

    /// Whether a sets its status by hand.
    #[must_use]
    pub fn manual(self) -> bool {
        matches!(self, Self::ManualHeld | Self::Manual)
    }
}

/// How long the page stays open after the report, nothing driven: longer than the web client's presence poll (about
/// four minutes), so b's reads see the state settle.
pub const OBSERVE: Duration = Duration::from_mins(5);

/// How long each manual status is held before the next.
pub const STATUS_HOLD: Duration = Duration::from_secs(75);

/// How long an allowed endpoint `PUT` gets to be answered once the chat is open. The web client sends it on a change of
/// its push channel's state, after its presence subscriptions (denied) were tried: 11 s to 60 s after the capture
/// began in the runs of 2026-10-22, and none within 70 s in one (`own-presence-both`, run 3), so the wait is long.
pub const REPORT: Duration = Duration::from_mins(3);

/// How long a manual status request gets to go out after its menu entry is clicked.
const STATUS_SENT: Duration = Duration::from_secs(20);

/// How long a control of the account menu gets to show.
const MENU: Duration = Duration::from_secs(15);

/// The statuses a sets by hand, in order: each one's name and the `data-tid` of its entry in the account menu's status
/// list (the client of 2026-10-23 named them `me_control_presence_availability_<status>`; the web bundle
/// `26260.1717.5229.7049` had `set-presence-status-popover-<category>` instead).
pub const STATUSES: [(&str, &str); 4] = [
    ("Busy", "me_control_presence_availability_busy"),
    ("DoNotDisturb", "me_control_presence_availability_do_not_disturb"),
    ("Away", "me_control_presence_availability_appear_away"),
    ("Available", "me_control_presence_availability_available"),
];

/// How long a step of the menu looks for a control that may already show (the menu left open by the last choice).
const GLANCE: Duration = Duration::from_secs(2);

/// UPS's presence endpoint `PUT`.
pub const ENDPOINT_PUT: &str = r"/ups/[a-z]+/v1/me/endpoints/$";

/// The manual status request, as the web bundle names it (`v1/me/forceavailability/`), by any method.
pub const MANUAL_STATUS: &str = r"/ups/[a-z]+/v1/me/forceavailability/?$";

/// The account menu's trigger, its status entry and its reset (the web bundle `26260.1717.5229.7049`).
const ACCOUNT_MENU: Selector<'static> = Selector::Tid("me-control-avatar-trigger");
const STATUS_MENU: Selector<'static> = Selector::Tid("set-presence-status-menu-item");
const RESET_STATUS: Selector<'static> = Selector::Tid("set-availability-reset-status");

/// What the account menu sends when it opens (seen first in `own-presence-manual-held` of 2026-10-23): the client
/// noting which of its own prompts it showed the account. It stays denied: the scenario needs no prompt.
const PROMPT_SHOWN: ExpectedRequest = ExpectedRequest {
    class: "engagement surfaces shown",
    hosts: TEAMS,
    method: "PUT",
    path: "/api/mt/part/*/beta/me/engagementSurfaces/displayed",
    reason: "records which of the client's own prompts it showed the account (the account menu's): the scenario \
             needs no prompt",
};

/// The denied classes these runs expect besides the reviewed list: `ab-trouter`'s ([`TROUTER_RUN_REQUESTS`]) and what
/// the account menu sends when it opens ([`PROMPT_SHOWN`]).
pub const OWN_PRESENCE_REQUESTS: [ExpectedRequest; 4] = [
    TROUTER_RUN_REQUESTS[0],
    TROUTER_RUN_REQUESTS[1],
    TROUTER_RUN_REQUESTS[2],
    PROMPT_SHOWN,
];

/// The denied classes the held manual run expects: [`OWN_PRESENCE_REQUESTS`] and the manual status (a set, or its
/// reset, the same `PUT` with no body), held back to capture it before it was mapped.
pub const MANUAL_HELD_REQUESTS: [ExpectedRequest; 5] = [
    TROUTER_RUN_REQUESTS[0],
    TROUTER_RUN_REQUESTS[1],
    TROUTER_RUN_REQUESTS[2],
    PROMPT_SHOWN,
    ExpectedRequest {
        class: "manual status",
        hosts: TEAMS,
        method: "PUT",
        path: "/ups/*/v1/me/forceavailability/",
        reason: "the request a manual status sends: held back in `own-presence-manual-held` to capture its shape \
                 before it was mapped (M2 Task 18)",
    },
];

/// How a run with `report` judges its denies: `ab-trouter`'s forced denies and mapped target, with
/// [`OWN_PRESENCE_REQUESTS`] ([`MANUAL_HELD_REQUESTS`] for [`Report::ManualHeld`]).
///
/// # Errors
/// Never for an id from [`one_to_one_id`]; when the chat's pattern doesn't compile.
pub fn policy(report: Report, chat_id: Option<&str>) -> Result<DenyPolicy, regex::Error> {
    let requests: &'static [ExpectedRequest] = if report == Report::ManualHeld {
        &MANUAL_HELD_REQUESTS
    } else {
        &OWN_PRESENCE_REQUESTS
    };
    let mut review = Review::new()
        .expecting_forced(&EXPECTED_FORCED)
        .expecting_requests(requests);
    if let Some(chat_id) = chat_id {
        review = review.with_mapped_target(chat_url(chat_id)?);
    }
    Ok(DenyPolicy::Reviewed(review))
}

/// The seconds since the Unix epoch now, for the notes: what b's reads are lined up with.
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// What `requests` hold of `pattern`'s (by method and URL), for the notes: each URL without its query, with how many
/// were answered with a 2xx, failed in the page or were answered otherwise, and got neither yet.
#[must_use]
pub fn tally(what: &str, pattern: &RequestPattern, requests: &[SeenRequest]) -> String {
    let mut counts = BTreeMap::<(String, &str), [usize; 3]>::new();
    for request in requests
        .iter()
        .filter(|request| pattern.matches(&request.method, &request.url))
    {
        let path = url::Url::parse(&request.url)
            .map_or_else(|_| without_query(&request.url).to_owned(), |url| url.path().to_owned());
        let count = counts.entry((path, request.method.as_str())).or_default();
        match (request.status, &request.failed) {
            (Some(200..=299), _) => count[0] += 1,
            (Some(_), _) | (None, Some(_)) => count[1] += 1,
            (None, None) => count[2] += 1,
        }
    }
    if counts.is_empty() {
        return format!("{what}: none");
    }
    let counts: Vec<String> = counts
        .iter()
        .map(|((path, method), [answered, failed, pending])| {
            format!(
                "{method} {path}: {answered} answered 2xx, {failed} failed or answered otherwise, {pending} unanswered"
            )
        })
        .collect();
    format!("{what}: {}", counts.join("; "))
}

/// Runs the `own-presence-*` scenario `name` ([`run`]).
///
/// # Errors
/// As [`run`]; [`ScenarioError::Setup`] for a scenario that isn't one of them.
pub async fn run_named(
    paths: &LabPaths,
    options: RunOptions,
    name: ScenarioName,
) -> Result<ScenarioReport, ScenarioError> {
    let report_with = Report::of(name).ok_or_else(|| setup(format!("{name} is no own-presence scenario")))?;
    run(paths, options, report_with).await
}

/// Runs one of the `own-presence-*` scenarios (see the module docs).
///
/// # Errors
/// When the run can't start: no lab config, no Chrome, no fence, a browser that can't be armed.
pub async fn run(paths: &LabPaths, options: RunOptions, report_with: Report) -> Result<ScenarioReport, ScenarioError> {
    let config = LabConfig::load(&LabLocations::from_env()?)?;
    let name = report_with.scenario();
    let session_options = SessionOptions {
        classifier: report_with.classifier(),
        frames: report_with.frames(),
        live_members: true,
        capture: Some(name.as_str().to_owned()),
        capture_request_bodies: true,
        display: options.display,
        media: Media::System,
        user_agent: None,
    };
    let mut a = LabSession::open_with(paths, Alias::A, session_options).await?;
    let mut report = ScenarioReport::new(name);
    let nonce = nonce();
    if let Err(error) = deny_probes(&a, Some(&probe_text(&nonce))) {
        report.failures.push(error.to_string());
    }
    let mri = |alias| config.account(alias).mri.as_deref().unwrap_or_default();
    let chat_id = one_to_one_id(mri(Alias::A), mri(Alias::B));
    let policy = policy(report_with, chat_id.as_deref()).unwrap_or_else(|error| {
        report
            .failures
            .push(format!("can't tell the a–b chat in a URL: {error}"));
        DenyPolicy::Reviewed(
            Review::new()
                .expecting_forced(&EXPECTED_FORCED)
                .expecting_requests(&OWN_PRESENCE_REQUESTS),
        )
    });
    let steps = Steps {
        mri_b: mri(Alias::B),
        chat_id: chat_id.as_deref(),
        nonce: &nonce,
        options,
        report_with,
    };
    if let Err(error) = steps.run(&mut a, &mut report).await {
        report.failures.push(error.to_string());
        if let Some(note) = save_screenshot(Alias::A, &a).await {
            report.notes.push(note);
        }
    }
    report.notes.push(format!("a: the page closes at unix {}", unix_now()));
    report.close_account(Alias::A, a, &policy).await;
    report
        .notes
        .push(format!("a: the browser closed at unix {}", unix_now()));
    Ok(report)
}

struct Steps<'a> {
    mri_b: &'a str,
    chat_id: Option<&'a str>,
    nonce: &'a str,
    options: RunOptions,
    report_with: Report,
}

impl Steps<'_> {
    async fn run(&self, a: &mut LabSession, report: &mut ScenarioReport) -> Result<(), ScenarioError> {
        let chat_id = self
            .chat_id
            .ok_or_else(|| setup("lab.toml has no MRI for a or b: run `proto-lab login` for them".to_owned()))?;
        let mut drive = a.driver();
        load(Alias::A, a, self.options.load_timeout).await?;
        report
            .notes
            .push(format!("a: the web client loaded at unix {}", unix_now()));
        wait_registered(&mut drive, "a registers its messaging endpoint", 0).await?;
        size(a).await?;
        drive
            .navigate("a opens the chat with b", &chat_link(self.mri_b))
            .await?;
        drive
            .wait_for("a's compose box", COMPOSE, self.options.load_timeout.max(CHAT_OPEN))
            .await?;
        let names_chat = chat_url(chat_id).map_err(|error| setup(error.to_string()))?;
        enforce(a, &mut drive, &probe_text(self.nonce), &names_chat).await?;
        report.notes.push(format!(
            "enforcement: a's probe post was denied by the fence and failed in a's page (unix {})",
            unix_now()
        ));
        self.reported(&mut drive, report).await?;
        if self.report_with.manual() {
            self.set_statuses(&mut drive, report).await?;
        } else {
            report.notes.push(format!(
                "observing from unix {} for {:?}, nothing driven",
                unix_now(),
                OBSERVE
            ));
            tokio::time::sleep(OBSERVE).await;
        }
        drive.seen_so_far();
        let endpoint = RequestPattern::new(Some("PUT"), ENDPOINT_PUT).map_err(|error| setup(error.to_string()))?;
        let manual = RequestPattern::new(None, MANUAL_STATUS).map_err(|error| setup(error.to_string()))?;
        report
            .notes
            .push(tally("a: presence endpoint PUTs", &endpoint, drive.requests()));
        report
            .notes
            .push(tally("a: manual status requests", &manual, drive.requests()));
        report
            .notes
            .push(format!("a: {} WebSocket frame(s) denied", a.frame_denials().len()));
        Ok(())
    }

    /// Step 3: with the endpoint `PUT` allowed, one must be answered with a 2xx; with it denied, a denied one is noted.
    async fn reported(&self, drive: &mut Driver, report: &mut ScenarioReport) -> Result<(), ScenarioError> {
        let endpoint = RequestPattern::new(Some("PUT"), ENDPOINT_PUT).map_err(|error| setup(error.to_string()))?;
        if self.report_with.endpoint() {
            let answered = drive
                .wait_for_seen(
                    "a's presence endpoint PUT is answered",
                    "a presence endpoint PUT answered with a 2xx",
                    0,
                    REPORT,
                    |seen| {
                        endpoint.matches(&seen.method, &seen.url)
                            && seen.status.is_some_and(|status| (200..300).contains(&status))
                    },
                )
                .await;
            match answered {
                Ok(answered) => report.notes.push(format!(
                    "a: a presence endpoint PUT was answered {:?} (sent at unix ms {:?}; noted at unix {})",
                    answered.status,
                    answered.sent_ms,
                    unix_now()
                )),
                // a manual status is set whether or not the client sent one (it didn't in some runs)
                Err(error) if self.report_with.manual() => report
                    .notes
                    .push(format!("a: no presence endpoint PUT was answered: {error}")),
                Err(error) => return Err(error.into()),
            }
        } else {
            let denied = drive
                .wait_for_seen(
                    "a's presence endpoint PUT is denied",
                    "a presence endpoint PUT that failed",
                    0,
                    REPORT,
                    |seen| endpoint.matches(&seen.method, &seen.url) && seen.failed.is_some(),
                )
                .await;
            report.notes.push(match denied {
                Ok(seen) => format!("a: a presence endpoint PUT was denied ({:?})", seen.failed),
                Err(error) => format!("a: no presence endpoint PUT was seen denied: {error}"),
            });
        }
        Ok(())
    }

    /// Step 4 (manual runs): each of [`STATUSES`], then a reset, from the account menu, each held for
    /// [`STATUS_HOLD`]; each choice's request is noted with its outcome.
    async fn set_statuses(&self, drive: &mut Driver, report: &mut ScenarioReport) -> Result<(), ScenarioError> {
        for (status, tid) in STATUSES {
            self.choose(drive, report, status, Selector::Tid(tid)).await?;
        }
        self.choose(drive, report, "reset", RESET_STATUS).await
    }

    /// Opens the account menu and its status entry (each only when it doesn't show already), clicks `entry` (the
    /// status `what`), notes the request it sends, and holds for [`STATUS_HOLD`].
    async fn choose(
        &self,
        drive: &mut Driver,
        report: &mut ScenarioReport,
        what: &str,
        entry: Selector<'_>,
    ) -> Result<(), ScenarioError> {
        let manual = RequestPattern::new(None, MANUAL_STATUS).map_err(|error| setup(error.to_string()))?;
        let opened = async {
            if drive.wait_for("the entry, already shown", entry, GLANCE).await.is_ok() {
                return Ok(());
            }
            if drive
                .wait_for("the status entry, already shown", STATUS_MENU, GLANCE)
                .await
                .is_err()
            {
                drive.wait_for("the account menu", ACCOUNT_MENU, MENU).await?;
                drive.click("a opens the account menu", ACCOUNT_MENU).await?;
                drive.wait_for("the status entry", STATUS_MENU, MENU).await?;
            }
            drive.click("a opens the status entry", STATUS_MENU).await?;
            drive.wait_for(&format!("the {what} entry"), entry, MENU).await?;
            Ok::<_, ScenarioError>(())
        };
        if let Err(error) = opened.await {
            report.notes.push(menu_tids(drive).await);
            return Err(error);
        }
        let since = drive.seen_so_far();
        drive.click(&format!("a chooses {what}"), entry).await?;
        let chosen = unix_now();
        let sent = drive
            .wait_for_seen(
                &format!("a's {what} request"),
                "a manual status request, answered or failed",
                since,
                STATUS_SENT,
                |seen| manual.matches(&seen.method, &seen.url) && (seen.status.is_some() || seen.failed.is_some()),
            )
            .await;
        report.notes.push(match sent {
            Ok(seen) => format!(
                "a: {what} chosen at unix {chosen}: {} {} answered {:?}, failed {:?}",
                seen.method,
                without_query(&seen.url),
                seen.status,
                seen.failed
            ),
            Err(error) => format!("a: {what} chosen at unix {chosen}: no manual status request settled: {error}"),
        });
        tokio::time::sleep(STATUS_HOLD).await;
        Ok(())
    }
}

/// For a menu step that failed: the `data-tid`s the page shows that name the account menu, presence or a status (the
/// client's control names, no page text).
async fn menu_tids(driver: &Driver) -> String {
    let expression = "[...new Set([...document.querySelectorAll('[data-tid]')].map(e => e.dataset.tid))].filter(t => \
                      /me-control|presence|status|availability/i.test(t) && !/[0-9a-f]{8}-|19:|8:orgid/.test(t)).join(' ')";
    match driver.evaluate("the page's account menu controls", expression).await {
        Ok(value) => format!(
            "a: account menu controls on the page: {}",
            value.as_str().unwrap_or("?")
        ),
        Err(error) => format!("a: can't list the account menu controls: {error}"),
    }
}
