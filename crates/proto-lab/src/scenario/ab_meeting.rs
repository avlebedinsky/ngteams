//! The `ab-meeting` scenario (`docs/protocol/calendar.md`, "Writing an event"): account a schedules a Teams meeting
//! with b from the web client's scheduling form, changes its title, then cancels it; what each step sends is
//! recorded. Only a's browser runs: b gets the invitation, the update and the cancellation in its mailbox.
//!
//! Each write goes through the lab rule as any write does: the event's creation is judged by the attendees it invites
//! (a and b's addresses, the lab's), its change and its cancellation by the event's organizer and attendees, read
//! fresh from the middle tier, and by any attendee the change adds.
//!
//! The steps:
//! 1. Before anything loads, a's fence force-denies every write carrying the probe marker, as in `ab-chat`.
//! 2. a opens the web client, opens the chat with b and proves enforcement live with this run's probe (`ab-chat`'s
//!    step 4).
//! 3. a opens the scheduling form by the web client's new-meeting deep link (title, b as attendee, a half hour later
//!    today), saves it, and the event's creation must be answered with a 2xx.
//! 4. a opens the calendar, opens the meeting, changes its title and sends the update, which must be answered with a
//!    2xx.
//! 5. a cancels the meeting, which must be answered with a 2xx.
//!
//! Every step saves a screenshot in the capture dir, and a step that can't find a control notes the controls the page
//! shows (their names, no page text but the form's labels). Judged as `ab-chat` is, every calendar write mapped.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::browser::Media;
use crate::config::{Alias, LabConfig, LabLocations, LabPaths};
use crate::fence_http::without_query;
use crate::session::{LabSession, SessionOptions};

use super::ab_chat::{
    CHAT_OPEN, COMPOSE, EXPECTED_FORCED, chat_link, chat_url, deny_probes, enforce, load, nonce, one_to_one_id,
    probe_text, save_screenshot, save_screenshot_as, setup,
};
use super::ab_trouter::size;
use super::dsl::{Driver, Key, RequestPattern, SeenRequest, StepError, js_string};
use super::expected::Review;
use super::{DenyPolicy, RunOptions, ScenarioError, ScenarioName, ScenarioReport};

/// The event's creation on the middle tier.
pub const CREATE: &str = r"/api/mt/part/[^/]+/v2\.0/me/calendars/default/events\?";

/// A change of an event.
pub const UPDATE: &str = r"/api/mt/part/[^/]+/v2\.0/me/calendars/events/[^/?]+\?";

/// A cancellation of an event.
pub const CANCEL: &str = r"/api/mt/part/[^/]+/v2\.0/me/calendars/events/[^/?]+/cancel";

/// The writes of the account's events: a deny of one fails the run, the list notwithstanding; the notes count them.
const CALENDAR_WRITES: &str = r"/api/mt/part/[^/]+/v2\.0/me/calendars/(default/)?events";

/// How long a form or a card gets to show.
const FORM: Duration = Duration::from_secs(45);

/// How long a write gets to be answered once its button was clicked.
const ANSWERED: Duration = Duration::from_secs(45);

/// How long the calendar gets to show the meeting.
const SHOWN: Duration = Duration::from_secs(60);

/// The meeting's length.
const LENGTH: Duration = Duration::from_mins(30);

/// Runs `ab-meeting` (see the module docs).
///
/// # Errors
/// When the run can't start: no lab config, no Chrome, no fence, a browser that can't be armed.
pub async fn run(paths: &LabPaths, options: RunOptions) -> Result<ScenarioReport, ScenarioError> {
    super::require_lab_scenario(super::ScenarioName::AbMeeting)?;
    let config = LabConfig::load(&LabLocations::from_env()?)?;
    let session_options = SessionOptions {
        live_members: true,
        capture: Some(ScenarioName::AbMeeting.as_str().to_owned()),
        capture_request_bodies: true,
        display: options.display,
        media: Media::System,
        user_agent: None,
        ..SessionOptions::fail_closed()
    };
    let a = LabSession::open_with(paths, Alias::A, session_options).await?;
    let nonce = nonce();
    let mri = |alias| config.account(alias).mri.as_deref().unwrap_or_default();
    let chat_id = one_to_one_id(mri(Alias::A), mri(Alias::B));
    let steps = Steps {
        mri_b: mri(Alias::B),
        upn_b: &config.account(Alias::B).upn,
        chat_id: chat_id.as_deref(),
        nonce: &nonce,
        options,
        pauses: FormPauses::default(),
    };
    Ok(finish(a, &steps).await)
}

/// Runs and settles an already admitted session; every failed step is diagnosed before the browser closes.
async fn finish(mut a: LabSession, steps: &Steps<'_>) -> ScenarioReport {
    let mut report = ScenarioReport::new(ScenarioName::AbMeeting);
    if let Err(error) = deny_probes(&a, Some(&probe_text(steps.nonce))) {
        report.failures.push(error.to_string());
    }
    let policy = policy(steps.chat_id).unwrap_or_else(|error| {
        report
            .failures
            .push(format!("can't tell the a–b chat or a calendar write in a URL: {error}"));
        DenyPolicy::Reviewed(Review::new().expecting_forced(&EXPECTED_FORCED))
    });
    if let Err(error) = steps.run(&mut a, &mut report).await {
        report.failures.push(error.to_string());
        let mut drive = a.driver();
        report.notes.push(controls_note(&mut drive).await);
        if let Some(note) = save_screenshot(Alias::A, &a).await {
            report.notes.push(note);
        }
    }
    let mut drive = a.driver();
    drive.seen_so_far();
    if let Ok(writes) = RequestPattern::new(None, CALENDAR_WRITES) {
        report.notes.push(super::own_presence::tally(
            "a: calendar writes",
            &writes,
            drive.requests(),
        ));
    }
    report.close_account(Alias::A, a, &policy).await;
    report
}

/// How a run judges its denies: `ab-chat`'s forced denies and mapped chat, and every write of the account's events
/// mapped too (a deny of one fails the run, the expected-deny list notwithstanding).
///
/// # Errors
/// Never for an id from [`one_to_one_id`]; when a pattern doesn't compile.
pub fn policy(chat_id: Option<&str>) -> Result<DenyPolicy, regex::Error> {
    let mut review = Review::new().expecting_forced(&EXPECTED_FORCED);
    if let Some(chat_id) = chat_id {
        review = review.with_mapped_target(chat_url(chat_id)?);
    }
    review = review.with_mapped_target(regex::Regex::new(CALENDAR_WRITES)?);
    Ok(DenyPolicy::Reviewed(review))
}

/// Original pauses while the attendee picker and cancellation form settle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FormPauses {
    suggestion: Duration,
    invited: Duration,
    cancel: Duration,
}

impl Default for FormPauses {
    fn default() -> Self {
        Self {
            suggestion: Duration::from_secs(2),
            invited: Duration::from_secs(2),
            cancel: Duration::from_secs(3),
        }
    }
}

struct Steps<'a> {
    mri_b: &'a str,
    upn_b: &'a str,
    chat_id: Option<&'a str>,
    nonce: &'a str,
    options: RunOptions,
    pauses: FormPauses,
}

impl Steps<'_> {
    async fn run(&self, a: &mut LabSession, report: &mut ScenarioReport) -> Result<(), ScenarioError> {
        let chat_id = self
            .chat_id
            .ok_or_else(|| setup("lab.toml has no MRI for a or b: run `proto-lab login` for them".to_owned()))?;
        let mut drive = a.driver();
        load(Alias::A, a, self.options.load_timeout).await?;
        size(a).await?;
        drive
            .navigate("a opens the chat with b", &chat_link(self.mri_b))
            .await?;
        drive
            .wait_for("a's compose box", COMPOSE, self.options.load_timeout.max(CHAT_OPEN))
            .await?;
        let names_chat = chat_url(chat_id).map_err(|error| setup(error.to_string()))?;
        enforce(a, &mut drive, &probe_text(self.nonce), &names_chat).await?;
        report
            .notes
            .push("enforcement: a's probe post was denied by the fence and failed in a's page".to_owned());
        let title = format!("lab meeting {}", self.nonce);
        let chat = self.create(a, &mut drive, report, &title).await?;
        let changed = format!("{title} (moved)");
        self.change(a, &mut drive, report, &chat, &changed).await?;
        self.cancel(a, &mut drive, report).await
    }

    /// Step 3: the scheduling form by the new-meeting deep link, saved; the meeting's chat, as its creation named it.
    async fn create(
        &self,
        a: &LabSession,
        drive: &mut Driver,
        report: &mut ScenarioReport,
        title: &str,
    ) -> Result<String, ScenarioError> {
        let created = self.created(a, drive, report, title).await?;
        meeting_chat(created.body.as_deref())
            .ok_or_else(|| setup("the event's creation named no meeting chat".to_owned()))
    }

    /// The scheduling form saved: the creation, its body read.
    async fn created(
        &self,
        a: &LabSession,
        drive: &mut Driver,
        report: &mut ScenarioReport,
        title: &str,
    ) -> Result<SeenRequest, ScenarioError> {
        let offset = drive
            .evaluate("the page's time zone", "-new Date().getTimezoneOffset()")
            .await?
            .as_i64()
            .unwrap_or_default();
        let (start, end) = slot(offset);
        let link = new_meeting_link(title, self.upn_b, &start, &end);
        drive.navigate("a opens the scheduling form", &link).await?;
        wait_for_button(
            drive,
            "the scheduling form's Send",
            &["Send", "Save"],
            self.options.load_timeout.max(FORM),
        )
        .await?;
        self.invite_b(drive).await?;
        shot(a, report, "meeting-form.png").await;
        report.notes.push(controls_note(drive).await);
        let since = drive.seen_so_far();
        click_button(drive, "a sends the meeting", &["Send", "Save"]).await?;
        let created = answered(drive, "the event's creation", "POST", CREATE, since).await;
        shot(a, report, "meeting-sent.png").await;
        report.notes.push(answer_note("creation", &created));
        expect_success("the event's creation", &created)?;
        match created {
            Ok(seen) => Ok(drive.read_body("the creation's body", &seen).await?),
            Err(error) => Err(error),
        }
    }

    /// Types b's address into the form's attendees and takes the suggestion the picker offers for it.
    async fn invite_b(&self, drive: &mut Driver) -> Result<(), ScenarioError> {
        focus(drive, "a focuses the attendees", ATTENDEES_INPUT).await?;
        drive.insert_text("a types b's address", self.upn_b).await?;
        let suggestion = "[...document.querySelectorAll('[role=\"option\"], [role=\"listbox\"] li')].some(e => \
                          e.getClientRects().length > 0)";
        drive
            .wait_until_true(
                "the picker suggests b",
                suggestion,
                "a suggestion for b's address",
                FORM,
            )
            .await?;
        tokio::time::sleep(self.pauses.suggestion).await;
        drive.press("a takes the suggestion", Key::ENTER).await?;
        tokio::time::sleep(self.pauses.invited).await;
        Ok(())
    }

    /// Step 4: the meeting's details opened from its chat, its title changed, the update sent.
    async fn change(
        &self,
        a: &mut LabSession,
        drive: &mut Driver,
        report: &mut ScenarioReport,
        chat: &str,
        changed: &str,
    ) -> Result<(), ScenarioError> {
        open_details(a, drive, report, chat, self.options.load_timeout).await?;
        wait_for(drive, "the form's title", TITLE_INPUT, FORM).await?;
        focus(drive, "a focuses the title", TITLE_INPUT).await?;
        drive.press("a selects the title", Key::SELECT_ALL).await?;
        drive.insert_text("a types the new title", changed).await?;
        shot(a, report, "meeting-edited.png").await;
        report.notes.push(controls_note(drive).await);
        let since = drive.seen_so_far();
        click_button(drive, "a sends the update", &["Send update", "Save", "Send"]).await?;
        let updated = answered(drive, "the event's update", "PUT", UPDATE, since).await;
        shot(a, report, "meeting-updated.png").await;
        report.notes.push(answer_note("update", &updated));
        expect_success("the event's update", &updated)
    }

    /// Step 5: the meeting cancelled from its details.
    async fn cancel(
        &self,
        a: &LabSession,
        drive: &mut Driver,
        report: &mut ScenarioReport,
    ) -> Result<(), ScenarioError> {
        tokio::time::sleep(self.pauses.cancel).await;
        wait_for_button(drive, "the details' Cancel meeting", &["Cancel meeting"], FORM).await?;
        click_button(
            drive,
            "a asks to cancel the meeting",
            &["Cancel meeting", "Cancel event"],
        )
        .await?;
        wait_for(drive, "the cancellation dialog", DIALOG, FORM).await?;
        shot(a, report, "meeting-cancel-dialog.png").await;
        report.notes.push(controls_note(drive).await);
        let since = drive.seen_so_far();
        click_in_dialog(
            drive,
            "a confirms the cancellation",
            &["Cancel meeting", "Cancel event", "Yes"],
        )
        .await?;
        let cancelled = answered(drive, "the event's cancellation", "PUT", CANCEL, since).await;
        shot(a, report, "meeting-cancelled.png").await;
        report.notes.push(answer_note("cancellation", &cancelled));
        expect_success("the event's cancellation", &cancelled)
    }
}

/// Schedules a meeting of a's with the account `upn`, as [`Steps::create`] does, for a run that goes on with it: the
/// form's creation's request body once it was answered with a 2xx.
pub(super) async fn schedule(
    a: &LabSession,
    drive: &mut Driver,
    report: &mut ScenarioReport,
    title: &str,
    upn: &str,
    options: RunOptions,
) -> Result<Option<String>, ScenarioError> {
    let steps = Steps {
        mri_b: "",
        upn_b: upn,
        chat_id: None,
        nonce: "",
        options,
        pauses: FormPauses::default(),
    };
    steps.created(a, drive, report, title).await.map(|seen| seen.body)
}

/// Cancels a's meeting whose chat is `chat` from its details, as [`Steps::cancel`] does, for a run that made it.
pub(super) async fn cancel_scheduled(
    a: &mut LabSession,
    drive: &mut Driver,
    report: &mut ScenarioReport,
    chat: &str,
    options: RunOptions,
) -> Result<(), ScenarioError> {
    // from the meeting's page the chat link's load can hang unanswered (seen once: its document request allowed, no
    // response in 90 s); one more try from a blank page, so the meeting isn't left behind
    if let Err(error) = open_details(a, drive, report, chat, options.load_timeout).await {
        report.notes.push(format!(
            "a: the meeting's details didn't open ({error}); trying again from a blank page"
        ));
        a.navigate("about:blank").await?;
        open_details(a, drive, report, chat, options.load_timeout).await?;
    }
    let mut drive = a.driver();
    let steps = Steps {
        mri_b: "",
        upn_b: "",
        chat_id: None,
        nonce: "",
        options,
        pauses: FormPauses::default(),
    };
    steps.cancel(a, &mut drive, report).await
}

/// The meeting chat a creation's request body names, for a run that goes on with it.
pub(super) fn created_chat(body: Option<&str>) -> Option<String> {
    meeting_chat(body)
}

/// The join link a creation's request body names (`schedulingServiceEvent.links.join`).
pub(super) fn created_join_link(body: Option<&str>) -> Option<String> {
    let body: serde_json::Value = serde_json::from_str(body?).ok()?;
    body.pointer("/schedulingServiceEvent/links/join")?
        .as_str()
        .map(str::to_owned)
}

/// The scheduling form's title box.
const TITLE_INPUT: &str = r#"input[placeholder*="title" i], input[aria-label*="title" i]"#;

/// The scheduling form's attendees box.
const ATTENDEES_INPUT: &str = r#"input[placeholder*="attendees" i], input[aria-label*="attendees" i]"#;

/// A dialog.
const DIALOG: &str = r#"[role="dialog"], [role="alertdialog"]"#;

/// Opens the meeting's chat `chat` (the web client loaded anew on its deep link, so the chat made since is known) and
/// its details: the scheduling form on the meeting, with Send update and Cancel meeting. The lab's calendar app is
/// Outlook's, which the scenario doesn't drive.
async fn open_details(
    a: &mut LabSession,
    drive: &mut Driver,
    report: &mut ScenarioReport,
    chat: &str,
    timeout: Duration,
) -> Result<(), ScenarioError> {
    let link = format!(
        "https://teams.microsoft.com/l/chat/{}/0",
        url::form_urlencoded::byte_serialize(chat.as_bytes()).collect::<String>()
    );
    super::ab_chat::load_at(Alias::A, a, &link, timeout).await?;
    if wait_for_button(
        drive,
        "the meeting chat's Details",
        &["Details", "Meeting details"],
        SHOWN,
    )
    .await
    .is_err()
    {
        shot(a, report, "meeting-chat.png").await;
        report.notes.push(controls_note(drive).await);
        return Err(setup("the meeting's chat shows no Details".to_owned()));
    }
    click_button(drive, "a opens the meeting's details", &["Details", "Meeting details"]).await?;
    if wait_for(drive, "the details' title", TITLE_INPUT, FORM).await.is_err() {
        shot(a, report, "meeting-details.png").await;
        report.notes.push(controls_note(drive).await);
        return Err(setup("the meeting's details show no title".to_owned()));
    }
    shot(a, report, "meeting-details.png").await;
    Ok(())
}

/// The meeting chat a creation's body names (`schedulingServiceEvent.groupContext.threadId`).
fn meeting_chat(body: Option<&str>) -> Option<String> {
    let body: serde_json::Value = serde_json::from_str(body?).ok()?;
    body.pointer("/schedulingServiceEvent/groupContext/threadId")?
        .as_str()
        .filter(|thread| thread.starts_with("19:meeting_"))
        .map(str::to_owned)
}

/// The meeting's start and end, in UTC (`2026-10-30T07:00:00+00:00`): 10:00 to 10:30 in the page's time zone (`offset`
/// minutes east of UTC) on the day the calendar's work week shows it, today, or on a weekend the Friday before.
fn slot(offset: i64) -> (String, String) {
    const DAY: i64 = 86_400;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let local = i64::try_from(now).unwrap_or_default() + offset * 60;
    let mut day = local.div_euclid(DAY);
    // 1970-01-01 was a Thursday: Monday is 0
    match (day + 3).rem_euclid(7) {
        5 => day -= 1,
        6 => day -= 2,
        _ => {}
    }
    let start = day * DAY + 10 * 3600 - offset * 60;
    let start = u64::try_from(start).unwrap_or_default();
    (iso_utc(start), iso_utc(start + LENGTH.as_secs()))
}

/// `secs` since the Unix epoch as an ISO date and time in UTC with its offset.
fn iso_utc(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or_default();
    let rest = secs % 86_400;
    // the civil date of a day count (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:00+00:00",
        rest / 3600,
        rest % 3600 / 60
    )
}

/// The web client's new-meeting deep link: the scheduling form with `title`, `attendee` invited, from `start` to `end`.
fn new_meeting_link(title: &str, attendee: &str, start: &str, end: &str) -> String {
    let query: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("subject", title)
        .append_pair("attendees", attendee)
        .append_pair("startTime", start)
        .append_pair("endTime", end)
        .finish();
    format!("https://teams.microsoft.com/l/meeting/new?{query}")
}

/// Waits for the first request from `since` on of `method` to a URL `pattern` finds that was answered or failed.
async fn answered(
    drive: &mut Driver,
    what: &str,
    method: &str,
    pattern: &str,
    since: usize,
) -> Result<SeenRequest, ScenarioError> {
    let wanted = RequestPattern::new(Some(method), pattern).map_err(|error| setup(error.to_string()))?;
    drive
        .wait_for_seen(
            &format!("{what} is answered"),
            &format!("{what}, answered or failed"),
            since,
            ANSWERED,
            |seen| wanted.matches(&seen.method, &seen.url) && (seen.status.is_some() || seen.failed.is_some()),
        )
        .await
        .map_err(ScenarioError::from)
}

/// A note of a write's outcome.
fn answer_note(what: &str, outcome: &Result<SeenRequest, ScenarioError>) -> String {
    match outcome {
        Ok(seen) => format!(
            "a: the event's {what}: {} {} answered {:?}, failed {:?}",
            seen.method,
            without_query(&seen.url),
            seen.status,
            seen.failed
        ),
        Err(error) => format!("a: the event's {what}: {error}"),
    }
}

/// `Ok` when the write was answered with a 2xx.
fn expect_success(what: &str, outcome: &Result<SeenRequest, ScenarioError>) -> Result<(), ScenarioError> {
    match outcome {
        Ok(seen) if seen.status.is_some_and(|status| (200..300).contains(&status)) => Ok(()),
        Ok(seen) => Err(setup(format!(
            "{what} was answered {:?}, failed {:?}",
            seen.status, seen.failed
        ))),
        Err(error) => Err(setup(format!("{what}: {error}"))),
    }
}

/// Saves a screenshot of a step, its note in the report.
async fn shot(a: &LabSession, report: &mut ScenarioReport, name: &str) {
    if let Some(note) = save_screenshot_as(Alias::A, a, name).await {
        report.notes.push(note);
    }
}

/// The page expression of the visible buttons (and menu items, tabs, links), whose name (`aria-label`, else its text,
/// spaces folded) is one of `names` (or, failing that, begins with one), in that order of preference.
fn find_button(scope: &str, names: &[&str]) -> String {
    let names = names.iter().map(|name| js_string(name)).collect::<Vec<_>>().join(",");
    format!(
        "(() => {{ const names = [{names}]; const norm = s => (s || '').replace(/\\s+/g, ' ').trim(); \
         const root = {scope}; if (!root) return null; \
         const all = [...root.querySelectorAll('button, [role=\"button\"], [role=\"menuitem\"], [role=\"tab\"], a')] \
         .filter(e => e.getClientRects().length > 0 && !e.disabled); \
         const name = e => norm(e.getAttribute('aria-label')) || norm(e.innerText); \
         for (const n of names) {{ const e = all.find(e => name(e) === n || norm(e.innerText) === n); if (e) return e; }} \
         for (const n of names) {{ const e = all.find(e => name(e).startsWith(n)); if (e) return e; }} \
         return null; }})()"
    )
}

/// Waits up to `timeout` for a button named one of `names` ([`find_button`]).
pub(super) async fn wait_for_button(
    drive: &mut Driver,
    step: &str,
    names: &[&str],
    timeout: Duration,
) -> Result<(), StepError> {
    let expression = format!("{} !== null", find_button("document", names));
    drive
        .wait_until_true(step, &expression, &format!("a button named one of {names:?}"), timeout)
        .await
}

/// Clicks the button named one of `names` ([`find_button`]) in the page.
pub(super) async fn click_button(drive: &mut Driver, step: &str, names: &[&str]) -> Result<(), ScenarioError> {
    click_found(drive, step, &find_button("document", names), names).await
}

/// Clicks the button named one of `names` in the dialog the page shows (the last one), or in the page when none shows.
async fn click_in_dialog(drive: &mut Driver, step: &str, names: &[&str]) -> Result<(), ScenarioError> {
    let scope = format!(
        "([...document.querySelectorAll({})].pop() || document)",
        js_string(DIALOG)
    );
    click_found(drive, step, &find_button(&scope, names), names).await
}

async fn click_found(drive: &mut Driver, step: &str, found: &str, names: &[&str]) -> Result<(), ScenarioError> {
    let expression = format!(
        "(() => {{ const e = {found}; if (!e) return false; e.scrollIntoView({{block: 'center'}}); e.click(); \
         return true; }})()"
    );
    match drive.evaluate(step, &expression).await? {
        serde_json::Value::Bool(true) => Ok(()),
        _ => Err(setup(format!("{step}: no button named one of {names:?}"))),
    }
}

/// Waits up to `timeout` for an element `css` finds.
async fn wait_for(drive: &mut Driver, step: &str, css: &str, timeout: Duration) -> Result<(), StepError> {
    let expression = format!("document.querySelector({}) !== null", js_string(css));
    drive.wait_until_true(step, &expression, css, timeout).await
}

/// Focuses the element `css` finds.
async fn focus(drive: &mut Driver, step: &str, css: &str) -> Result<(), ScenarioError> {
    let expression = format!(
        "(() => {{ const e = document.querySelector({}); if (!e) return false; e.focus(); return true; }})()",
        js_string(css)
    );
    match drive.evaluate(step, &expression).await? {
        serde_json::Value::Bool(true) => Ok(()),
        _ => Err(setup(format!("{step}: nothing matches {css}"))),
    }
}

/// For a step that failed or a form just opened: the names of the buttons the page shows, and its `data-tid`s that name
/// the calendar's controls (no ids).
async fn controls_note(drive: &mut Driver) -> String {
    let expression = "(() => { const norm = s => (s || '').replace(/\\s+/g, ' ').trim(); \
                      const buttons = [...new Set([...document.querySelectorAll('button, [role=\"button\"], \
                      [role=\"menuitem\"]')].filter(e => e.getClientRects().length > 0).map(e => \
                      norm(e.getAttribute('aria-label')) || norm(e.innerText)).filter(n => n && n.length < 40))]; \
                      const tids = [...new Set([...document.querySelectorAll('[data-tid]')].map(e => \
                      e.dataset.tid))].filter(t => /calendar|schedul|meeting|event|form|title|attendee|peek|card|\
                      cancel|edit|send|save/i.test(t) && !/[0-9a-f]{8}-|19:|8:orgid|@/.test(t)); \
                      return 'buttons: ' + buttons.join(' | ') + ' ;; tids: ' + tids.join(' '); })()";
    match drive.evaluate("the page's controls", expression).await {
        Ok(value) => format!("a: {}", value.as_str().unwrap_or("?")),
        Err(error) => format!("a: can't list the page's controls: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_written_in_utc() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00+00:00");
        assert_eq!(iso_utc(1_792_800_000 + 15 * 3600 + 1800), "2026-10-24T15:30:00+00:00");
    }

    #[test]
    fn the_deep_link_carries_the_meeting() {
        let link = new_meeting_link("lab meeting x", "b@lab.example.com", "s", "e");
        assert!(link.starts_with("https://teams.microsoft.com/l/meeting/new?subject=lab+meeting+x&attendees=b%40lab"));
    }
}

#[cfg(test)]
#[path = "../../tests/support/scenario/ab_meeting_driver_tests.rs"]
mod driver_tests;
