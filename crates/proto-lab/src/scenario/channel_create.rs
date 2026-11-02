//! The `channel-create` scenario: what the web client's "Create a channel" dialog offers and what it sends for each
//! channel type and layout, recorded for the protocol notes (`docs/protocol/channels.md`, "Creating a channel"), with
//! nothing ever sent: the lab's teams hold people outside the lab, so every write that could make a channel is denied
//! by a forced deny ([`CHANNEL_WRITE`]) from before the page loads, on any host, and the capture holds each one whole
//! (its URL, headers and body).
//!
//! 1. a opens the web client under the fail-closed fence and waits for its teams to show.
//! 2. a hovers each team's line in turn and opens its "..." menu ([`open_team_menu`]) until one offers "Add channel"
//!    (an archived team's menu doesn't), naming the menu's entries in the notes.
//! 3. a opens the dialog, shows it, opens the channel type's dropdown and names its options; then for each of
//!    [`ATTEMPTS`] it fills the dialog and presses Create: the write must be denied under [`CHANNEL_WRITE`], and the
//!    dialog's error is shown and left.
//!
//! 4. a opens a channel of the team and starts a mention: the people search the add-people step's box asks of a team, read
//!    only; the compose box is emptied again.
//!
//! Every step saves a screenshot in the capture dir. The run fails when no team offers the dialog or when Create sends
//! nothing it could deny; every deny is expected.
//!
//! **Writing** (`PROTO_LAB_CHANNEL_WRITES=1`, on a team of the lab's own named by `PROTO_LAB_TEAM`): nothing is forced
//! but the enforcement probe, and the team's members are read live, so a channel is made only when the lab rule lets it
//! through ([`teams_fence::Fence::check`]: everyone on the team a lab account, Teams' own apps not counted). A first
//! channel named with [`PROBE`] is force-denied and must fail in the page; then each of [`ATTEMPTS`] is made for real,
//! and the private one's add-people step adds b ([`MEMBER`]). The run fails when a creation or the add isn't answered
//! with a success. It deletes nothing: the team is deleted after the run (`team-manage`).

use std::time::Duration;

use crate::config::{Alias, LabPaths};
use crate::fence_http::ForcedDeny;
use crate::session::{LabSession, SessionOptions};

use super::ab_chat::{COMPOSE, load, save_screenshot_as};
use super::ab_chat_full::clear_compose;
use super::ab_trouter::size;
use super::dsl::{Driver, Key, RequestPattern, Selector, js_string};
use super::{DenyPolicy, RunOptions, ScenarioError, ScenarioName, ScenarioReport};

/// The account that drives the dialog.
pub const ACCOUNT: Alias = Alias::A;

/// The account a writing run adds to the private channel it makes.
pub const MEMBER: Alias = Alias::B;

/// What a writing run's first channel name holds, force-denied: the enforcement probe.
pub const PROBE: &str = "lab channel probe";

/// The label of the probe's forced deny.
pub const PROBE_LABEL: &str = "the channel probe";

/// What a writing run's private channel's add-people step sends.
const MEMBERS_URL: &str = r"(?i)/channels/[^/]+/users([?]|$)";

/// How a writing run goes: the team it writes on and the address of the account it adds.
struct Writing {
    team: String,
    member: String,
}

/// The label of the forced deny of every write that could make a channel.
pub const CHANNEL_WRITE: &str = "a channel's creation";

/// What makes a write one that could make a channel, on any host: the middle tier's and the provisioning service's
/// `…/teams/<team>/channels`, `…/channelsWarmup`, `…/groups/<group>/standardchannel`, `…/standalonechannels`, a
/// channel's `…/members`.
pub const CHANNEL_URL: &str = r"(?i)/channels?(warmup)?([/?]|$)|/standardchannel|/standalonechannels";

/// The methods denied on [`CHANNEL_URL`]: every one but a read.
const WRITE_METHODS: [&str; 4] = ["POST", "PUT", "PATCH", "DELETE"];

/// The channel type and the layout of each attempt, as the dialog names them.
pub const ATTEMPTS: [(&str, &str); 3] = [("Standard", "Threads"), ("Standard", "Posts"), ("Private", "Threads")];

/// How long Create gets to send its write.
const SENT: Duration = Duration::from_secs(30);

/// A step's pause: the page showing what changed.
const SETTLE: Duration = Duration::from_secs(3);

/// Tags the `n`th team's own line under "Teams and channels" (not its channels below it) with `data-lab-team`, and says
/// how many teams there are, or `null` past the last.
fn tag_team(n: usize) -> String {
    format!(
        "(() => {{ document.querySelectorAll('[data-lab-team]').forEach(e => e.removeAttribute('data-lab-team')); \
         const teams = [...document.querySelectorAll('[data-testid=\"list-item-teams-and-channels\"]')]\
         .filter(e => e.getClientRects().length); const team = teams[{n}]; if (!team) return null; \
         const line = team.firstElementChild || team; line.setAttribute('data-lab-team', '1'); \
         line.scrollIntoView({{block: 'center'}}); return teams.length; }})()"
    )
}

/// Clicks the first visible element a `query` selects whose text or label reads `name` (a regular expression); whether
/// one did.
pub(super) fn click_named(query: &str, name: &str) -> String {
    format!(
        "(() => {{ const norm = e => ((e.innerText || '') + ' ' + (e.getAttribute('aria-label') || '')).replace(/\\s+/g, ' ').trim(); \
         const found = [...document.querySelectorAll({})].filter(e => e.getClientRects().length && new RegExp({}, 'i').test(norm(e)))[0]; \
         if (!found) return false; found.click(); return true; }})()",
        js_string(query),
        js_string(name)
    )
}

/// The texts of the visible elements `query` selects.
fn texts(query: &str) -> String {
    format!(
        "(() => [...document.querySelectorAll({})].filter(e => e.getClientRects().length).map(e => \
         (e.innerText || e.getAttribute('aria-label') || '').replace(/\\s+/g, ' ').trim()).join(' | '))()",
        js_string(query)
    )
}

/// Runs `channel-create` (see the module docs).
///
/// # Errors
/// When the run can't start: no Chrome, no fence, a browser that can't be armed.
pub async fn run(paths: &LabPaths, options: RunOptions) -> Result<ScenarioReport, ScenarioError> {
    let writing = writing()?;
    let session_options = SessionOptions {
        capture: Some(ScenarioName::ChannelCreate.as_str().to_owned()),
        capture_request_bodies: true,
        display: options.display,
        live_members: writing.is_some(),
        ..SessionOptions::fail_closed()
    };
    let mut a = LabSession::open_with(paths, ACCOUNT, session_options).await?;
    let mut report = ScenarioReport::new(ScenarioName::ChannelCreate);
    let mut drive = a.driver();
    let forced = if writing.is_some() {
        probe_rule().map(|rule| vec![rule])
    } else {
        channel_rules()
    }
    .map_err(|error| ScenarioError::Setup(error.to_string()));
    match forced {
        Ok(rules) => rules.into_iter().for_each(|rule| a.force_deny(rule)),
        Err(error) => {
            report.failures.push(error.to_string());
            report.close_account(ACCOUNT, a, &DenyPolicy::AllExpected).await;
            return Ok(report);
        }
    }
    let ran = match &writing {
        Some(writing) => write_steps(&mut a, &mut drive, &mut report, options, writing).await,
        None => steps(&mut a, &mut drive, &mut report, options).await,
    };
    if let Err(error) = ran {
        report.failures.push(error.to_string());
        shot(&a, &mut report, "channel-failure.png").await;
    }
    report.close_account(ACCOUNT, a, &DenyPolicy::AllExpected).await;
    Ok(report)
}

/// A writing run's settings, from `PROTO_LAB_CHANNEL_WRITES`, `PROTO_LAB_TEAM` and `lab.toml`; `None` for a run that
/// writes nothing.
fn writing() -> Result<Option<Writing>, ScenarioError> {
    if std::env::var("PROTO_LAB_CHANNEL_WRITES").as_deref() != Ok("1") {
        return Ok(None);
    }
    let team = std::env::var("PROTO_LAB_TEAM")
        .ok()
        .filter(|team| !team.trim().is_empty())
        .ok_or_else(|| {
            ScenarioError::Setup("a writing run needs PROTO_LAB_TEAM, a team of the lab's own".to_owned())
        })?;
    let config =
        crate::config::LabConfig::load(&crate::config::LabLocations::from_env().map_err(setup)?).map_err(setup)?;
    Ok(Some(Writing {
        team,
        member: config.account(MEMBER).upn.clone(),
    }))
}

fn setup(error: impl std::fmt::Display) -> ScenarioError {
    ScenarioError::Setup(error.to_string())
}

/// A writing run's forced deny: a channel named with [`PROBE`].
///
/// # Errors
/// Never: the pattern is fixed.
pub fn probe_rule() -> Result<ForcedDeny, regex::Error> {
    Ok(ForcedDeny::new("POST", CHANNEL_URL)?
        .with_body_containing(PROBE)
        .labelled(PROBE_LABEL))
}

/// A writing run (see the module docs).
async fn write_steps(
    a: &mut LabSession,
    drive: &mut Driver,
    report: &mut ScenarioReport,
    options: RunOptions,
    writing: &Writing,
) -> Result<(), ScenarioError> {
    load(ACCOUNT, a, options.load_timeout).await?;
    size(a).await?;
    tokio::time::sleep(Duration::from_secs(10)).await;
    shot(a, report, "channel-loaded.png").await;
    open_managed_dialog(a, drive, report, &writing.team).await?;
    let nonce = super::ab_chat::nonce();
    // the probe: held back, the page must fail it
    fill_named(drive, report, 9, &format!("{PROBE} {nonce}"), ("Standard", "Threads")).await?;
    let probe = created(drive, report, 9, CHANNEL_URL).await?;
    if probe.status.is_some_and(|status| (200..300).contains(&status)) {
        report
            .failures
            .push(format!("the probe went through: {:?}", probe.status));
        return Ok(());
    }
    tokio::time::sleep(Duration::from_secs(8)).await;
    shot(a, report, "channel-probe.png").await;
    leave_error(drive, report, 9).await?;
    for (at, attempted) in ATTEMPTS.into_iter().enumerate() {
        make_channel(a, drive, report, at, attempted, writing).await?;
    }
    Ok(())
}

/// One channel made for real: the dialog filled, Create answered with a success, a private one's member added.
async fn make_channel(
    a: &LabSession,
    drive: &mut Driver,
    report: &mut ScenarioReport,
    at: usize,
    (kind, layout): (&str, &str),
    writing: &Writing,
) -> Result<(), ScenarioError> {
    open_managed_dialog(a, drive, report, &writing.team).await?;
    let name = format!("lab {kind} {layout} {at}");
    fill_named(drive, report, at, &name, (kind, layout)).await?;
    let made = created(drive, report, at, CHANNEL_URL).await?;
    if !made.status.is_some_and(|status| (200..300).contains(&status)) {
        report.failures.push(format!(
            "attempt {at} ({kind}, {layout}): {} {:?}",
            made.url, made.status
        ));
    }
    tokio::time::sleep(Duration::from_secs(8)).await;
    shot(a, report, &format!("channel-made-{at}.png")).await;
    if kind == "Private" {
        add_member(a, drive, report, at, &writing.member).await?;
    }
    leave_error(drive, report, at).await
}

/// The dialog opened from the team named `name`'s own page: "Your teams and channels", its row, then the page's "..." and
/// its "Add channel" (a team with a single channel has no "..." on its sidebar line).
async fn open_managed_dialog(
    a: &LabSession,
    drive: &mut Driver,
    report: &mut ScenarioReport,
    name: &str,
) -> Result<(), ScenarioError> {
    let _ = drive.press("a closes what is open", Key::ESCAPE).await;
    let listed = drive
        .evaluate(
            "a opens the list's options",
            &click_named("button,[role=\"button\"]", "^More list options$"),
        )
        .await?;
    tokio::time::sleep(SETTLE).await;
    let teams = drive
        .evaluate(
            "a opens its teams",
            &click_named("[role=\"menuitem\"]", "^Your teams and channels"),
        )
        .await?;
    tokio::time::sleep(Duration::from_secs(8)).await;
    let row = drive
        .evaluate("a opens the team", &click_named("[role=\"row\"]", name))
        .await?;
    report.notes.push(format!(
        "a: list options {listed}, teams page {teams}, {name}'s row {row}"
    ));
    if row.as_bool() != Some(true) {
        return Err(ScenarioError::Setup(format!("no team named {name}")));
    }
    tokio::time::sleep(Duration::from_secs(8)).await;
    drive
        .pointer_click(
            "a opens the team's menu",
            Selector::Css("[data-tid^=\"team-context-menu-more-button\"]"),
        )
        .await?;
    tokio::time::sleep(SETTLE).await;
    shot(a, report, &format!("channel-team-menu-{}.png", super::ab_chat::nonce())).await;
    let added = drive
        .evaluate(
            "a clicks Add channel",
            &click_named("[role=\"menuitem\"]", "^Add channel$"),
        )
        .await?;
    if added.as_bool() != Some(true) {
        return Err(ScenarioError::Setup(format!("{name}'s menu offers no Add channel")));
    }
    drive
        .wait_for_tid("the dialog opens", "channel-name-input", Duration::from_secs(30))
        .await?;
    tokio::time::sleep(SETTLE).await;
    Ok(())
}

/// The dialog filled with `name`, the type and the layout.
async fn fill_named(
    drive: &mut Driver,
    report: &mut ScenarioReport,
    at: usize,
    name: &str,
    (kind, layout): (&str, &str),
) -> Result<(), ScenarioError> {
    fill_form_named(drive, report, at, name, kind, layout).await
}

/// Create pressed: the write it sends to `url`, once answered (or failed).
async fn created(
    drive: &mut Driver,
    report: &mut ScenarioReport,
    at: usize,
    url: &str,
) -> Result<super::dsl::SeenRequest, ScenarioError> {
    let since = drive.seen_so_far();
    let pressed = drive
        .evaluate(
            "a presses Create",
            &click_named("[data-tid=\"create-and-edit-submit-button\"]", "Create"),
        )
        .await?;
    report.notes.push(format!("a: attempt {at}: Create pressed: {pressed}"));
    let wanted = RequestPattern::new(Some("POST"), url).map_err(setup)?;
    let write = drive
        .wait_for_seen("Create sends a write", "a channel write", since, SENT, |request| {
            wanted.matches_request(request)
                && !request.url.contains("channelsWarmup")
                && (request.status.is_some() || request.failed.is_some())
        })
        .await?;
    report.notes.push(format!(
        "a: attempt {at}: {} {} -> {:?} {:?}",
        write.method, write.url, write.status, write.failed
    ));
    Ok(write)
}

/// The private channel's add-people step: `member` typed, picked and added; the add must be answered with a success.
async fn add_member(
    a: &LabSession,
    drive: &mut Driver,
    report: &mut ScenarioReport,
    at: usize,
    member: &str,
) -> Result<(), ScenarioError> {
    let words = drive
        .evaluate(
            "the step's words",
            "(() => { const d = [...document.querySelectorAll('[role=\"dialog\"]')].pop(); return d ? d.innerText.replace(/\\s+/g, ' ') : 'none'; })()",
        )
        .await?;
    report
        .notes
        .push(format!("a: attempt {at}: the add-people step: {words}"));
    let typed = drive
        .evaluate(
            "a finds the people box",
            "(() => { const d = [...document.querySelectorAll('[role=\"dialog\"]')].pop(); const input = d && [...d.querySelectorAll('input')].filter(e => e.getClientRects().length && e.type !== 'checkbox' && e.type !== 'radio')[0]; if (!input) return false; input.setAttribute('data-lab-people', '1'); input.focus(); return true; })()",
        )
        .await?;
    if typed.as_bool() != Some(true) {
        report.failures.push(format!("attempt {at}: no people box"));
        return Ok(());
    }
    let local = member.split('@').next().unwrap_or(member);
    drive
        .type_into("a types the member", Selector::Css("[data-lab-people]"), local)
        .await?;
    tokio::time::sleep(Duration::from_secs(6)).await;
    shot(a, report, &format!("channel-people-{at}.png")).await;
    let picked = drive
        .evaluate("a picks the member", &click_named("[role=\"option\"]", local))
        .await?;
    report.notes.push(format!("a: attempt {at}: picked {local}: {picked}"));
    tokio::time::sleep(SETTLE).await;
    let since = drive.seen_so_far();
    let added = drive
        .evaluate(
            "a presses Add",
            "(() => { const d = [...document.querySelectorAll('[role=\"dialog\"]')].pop(); const add = d && [...d.querySelectorAll('button')].filter(e => e.getClientRects().length && /^\\s*Add\\s*$/i.test(e.innerText || ''))[0]; if (!add) return false; add.click(); return true; })()",
        )
        .await?;
    report.notes.push(format!("a: attempt {at}: Add pressed: {added}"));
    let wanted = RequestPattern::new(Some("POST"), MEMBERS_URL).map_err(setup)?;
    match drive
        .wait_for_seen("Add sends the member", "the member's add", since, SENT, |request| {
            wanted.matches_request(request) && (request.status.is_some() || request.failed.is_some())
        })
        .await
    {
        Ok(write) => {
            report.notes.push(format!(
                "a: attempt {at}: {} {} -> {:?} {:?}",
                write.method, write.url, write.status, write.failed
            ));
            if !write.status.is_some_and(|status| (200..300).contains(&status)) {
                report
                    .failures
                    .push(format!("attempt {at}: the member's add: {:?}", write.status));
            }
        }
        Err(error) => report.failures.push(format!("attempt {at}: {error}")),
    }
    tokio::time::sleep(Duration::from_secs(8)).await;
    shot(a, report, &format!("channel-added-{at}.png")).await;
    Ok(())
}

/// The rules the run forces.
///
/// # Errors
/// Never: the patterns are fixed.
pub fn channel_rules() -> Result<Vec<ForcedDeny>, regex::Error> {
    WRITE_METHODS
        .iter()
        .map(|method| Ok(ForcedDeny::new(method, CHANNEL_URL)?.labelled(CHANNEL_WRITE)))
        .collect()
}

async fn steps(
    a: &mut LabSession,
    drive: &mut Driver,
    report: &mut ScenarioReport,
    options: RunOptions,
) -> Result<(), ScenarioError> {
    load(ACCOUNT, a, options.load_timeout).await?;
    size(a).await?;
    tokio::time::sleep(Duration::from_secs(10)).await;
    shot(a, report, "channel-loaded.png").await;
    let team = find_team(a, drive, report).await?;
    open_dialog(a, drive, report, team).await?;
    show_dropdown(a, drive, report).await?;
    for (at, attempted) in ATTEMPTS.into_iter().enumerate() {
        attempt(a, drive, report, team, at, attempted).await?;
    }
    people_search(a, drive, report).await
}

/// What finds people on a team: a's mention in a channel of the team asks the search the add-people step's box asks; it
/// is only read, and the box is emptied again.
async fn people_search(a: &LabSession, drive: &mut Driver, report: &mut ScenarioReport) -> Result<(), ScenarioError> {
    let opened = drive
        .evaluate(
            "a opens the team's channel",
            "(() => { const row = document.querySelector('[data-testid^=\"sc-channel-list-item-\"]'); if (!row) return false; row.click(); return true; })()",
        )
        .await?;
    report.notes.push(format!("a: the team's channel opened: {opened}"));
    tokio::time::sleep(Duration::from_secs(10)).await;
    let since = drive.seen_so_far();
    drive.type_into("a starts a mention", COMPOSE, "@").await?;
    tokio::time::sleep(Duration::from_secs(1)).await;
    drive.insert_text("a types a name", "ca").await?;
    let wanted = RequestPattern::new(Some("POST"), r"(?i)/users/profilesearch|/members/search")
        .map_err(|error| ScenarioError::Setup(error.to_string()))?;
    let found = drive
        .wait_for_seen("the search goes", "a people search", since, SENT, |request| {
            wanted.matches_request(request) && request.status.is_some()
        })
        .await;
    match found {
        Ok(found) => report.notes.push(format!(
            "a: people search: {} {} -> {:?}",
            found.method, found.url, found.status
        )),
        Err(error) => report.notes.push(format!("a: no people search: {error}")),
    }
    shot(a, report, "channel-mention.png").await;
    let _ = drive.press("a closes the picker", Key::ESCAPE).await;
    let _ = clear_compose(drive).await;
    Ok(())
}

/// The first team whose "..." menu offers "Add channel", its menu left closed.
async fn find_team(a: &LabSession, drive: &mut Driver, report: &mut ScenarioReport) -> Result<usize, ScenarioError> {
    for team in 0..8 {
        if open_team_menu(drive, team).await?.is_none() {
            break;
        }
        tokio::time::sleep(SETTLE).await;
        shot(a, report, &format!("channel-team-{team}-menu.png")).await;
        let menu = drive
            .evaluate("the menu's entries", &texts("[role=\"menuitem\"]"))
            .await?;
        let menu = menu.as_str().unwrap_or("?").to_owned();
        report.notes.push(format!("a: team {team}'s menu: {menu}"));
        drive.press("a closes the menu", Key::ESCAPE).await?;
        tokio::time::sleep(SETTLE).await;
        if menu.contains("Add channel") {
            return Ok(team);
        }
    }
    Err(ScenarioError::Setup("no team's menu offers Add channel".to_owned()))
}

/// Hovers the team's line and clicks its "..."; `None` past the last team.
async fn open_team_menu(drive: &mut Driver, team: usize) -> Result<Option<()>, ScenarioError> {
    let count = drive.evaluate("a finds a team's line", &tag_team(team)).await?;
    if count.is_null() {
        return Ok(None);
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    drive
        .hover("a hovers the team", Selector::Css("[data-lab-team]"))
        .await?;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let clicked = drive
        .evaluate(
            "a clicks the team's more options",
            "(() => { const row = document.querySelector('[data-lab-team]'); const buttons = [...row.querySelectorAll('button,[role=\"button\"]')].filter(e => e.getClientRects().length); const more = buttons.filter(e => /more|options|\\.\\.\\./i.test((e.getAttribute('aria-label') || '') + (e.getAttribute('title') || '')))[0] || buttons[buttons.length - 1]; if (!more) return (row.outerHTML || '').slice(0, 1500); more.click(); return true; })()",
        )
        .await?;
    if clicked.as_bool() != Some(true) {
        return Err(ScenarioError::Setup(format!("no more-options button: {clicked}")));
    }
    Ok(Some(()))
}

/// Opens the team's menu (again and again while it doesn't show: a menu opened on a line just left needs the pointer
/// back on the line) and its "Add channel", shown.
async fn open_dialog(
    a: &LabSession,
    drive: &mut Driver,
    report: &mut ScenarioReport,
    team: usize,
) -> Result<(), ScenarioError> {
    let mut clicked = false;
    for _ in 0..4 {
        open_team_menu(drive, team).await?;
        tokio::time::sleep(SETTLE).await;
        let found = drive
            .evaluate(
                "a clicks Add channel",
                &click_named("[role=\"menuitem\"]", "Add channel"),
            )
            .await?;
        if found.as_bool() == Some(true) {
            clicked = true;
            break;
        }
        drive.press("a closes what opened", Key::ESCAPE).await?;
        tokio::time::sleep(SETTLE).await;
    }
    if !clicked {
        return Err(ScenarioError::Setup("the menu's Add channel is gone".to_owned()));
    }
    drive
        .wait_for_tid("the dialog opens", "channel-name-input", Duration::from_secs(30))
        .await?;
    tokio::time::sleep(SETTLE).await;
    shot(a, report, &format!("channel-dialog-{}.png", super::ab_chat::nonce())).await;
    Ok(())
}

/// The channel type's dropdown open, its options named, then closed.
async fn show_dropdown(a: &LabSession, drive: &mut Driver, report: &mut ScenarioReport) -> Result<(), ScenarioError> {
    let opened = drive
        .evaluate(
            "a opens the channel type",
            &click_named("[data-tid=\"channel-type-dropdown\"]", "."),
        )
        .await?;
    report
        .notes
        .push(format!("a: the channel type's dropdown opened: {opened}"));
    tokio::time::sleep(SETTLE).await;
    shot(a, report, "channel-types.png").await;
    let options = drive.evaluate("the types", &texts("[role=\"option\"]")).await?;
    report
        .notes
        .push(format!("a: the channel types: {}", options.as_str().unwrap_or("?")));
    let labels = drive
        .evaluate(
            "the dialog's words",
            "(() => { const d = document.querySelector('[role=\"dialog\"]'); return d ? d.innerText.replace(/\\s+/g, ' ') : ''; })()",
        )
        .await?;
    report
        .notes
        .push(format!("a: the dialog says: {}", labels.as_str().unwrap_or("?")));
    let closed = drive
        .evaluate(
            "a closes the dropdown",
            &click_named("[data-tid=\"channel-type-dropdown\"]", "."),
        )
        .await?;
    report
        .notes
        .push(format!("a: the channel type's dropdown closed: {closed}"));
    tokio::time::sleep(SETTLE).await;
    Ok(())
}

/// One attempt: the dialog filled for its channel type and layout, Create pressed, the write denied, the error shown and
/// left.
async fn attempt(
    a: &LabSession,
    drive: &mut Driver,
    report: &mut ScenarioReport,
    team: usize,
    at: usize,
    (kind, layout): (&str, &str),
) -> Result<(), ScenarioError> {
    let open = drive
        .evaluate(
            "the dialog is open",
            "!!document.querySelector('[data-tid=\"channel-name-input\"]')",
        )
        .await?;
    if open.as_bool() != Some(true) {
        open_dialog(a, drive, report, team).await?;
    }
    fill_form(drive, report, at, kind, layout).await?;
    shot(a, report, &format!("channel-filled-{at}.png")).await;
    press_create(drive, report, at).await?;
    tokio::time::sleep(Duration::from_secs(8)).await;
    shot(a, report, &format!("channel-error-{at}.png")).await;
    leave_error(drive, report, at).await
}

/// The dialog's fields, for one attempt.
async fn fill_form(
    drive: &mut Driver,
    report: &mut ScenarioReport,
    at: usize,
    kind: &str,
    layout: &str,
) -> Result<(), ScenarioError> {
    fill_form_named(drive, report, at, &format!("lab dialog shape {at}"), kind, layout).await
}

/// The dialog's fields: `name`, a description, the type and the layout.
async fn fill_form_named(
    drive: &mut Driver,
    report: &mut ScenarioReport,
    at: usize,
    name: &str,
    kind: &str,
    layout: &str,
) -> Result<(), ScenarioError> {
    drive
        .type_into("a names the channel", Selector::Tid("channel-name-input"), name)
        .await?;
    drive
        .type_into(
            "a describes the channel",
            Selector::Tid("channel-description-text-area"),
            "lab description",
        )
        .await?;
    let opened = drive
        .evaluate(
            "a opens the channel type",
            &click_named("[data-tid=\"channel-type-dropdown\"]", "."),
        )
        .await?;
    report
        .notes
        .push(format!("a: attempt {at}: the type's dropdown opened: {opened}"));
    tokio::time::sleep(SETTLE).await;
    let picked = drive
        .evaluate(
            "a picks the channel type",
            &click_named("[role=\"option\"]", &format!("^{kind}")),
        )
        .await?;
    report.notes.push(format!("a: attempt {at}: picked {kind}: {picked}"));
    tokio::time::sleep(Duration::from_secs(1)).await;
    let radio = if layout == "Posts" {
        "post_reply_modality_radio"
    } else {
        "conversational_modality_radio"
    };
    let layout_picked = drive
        .evaluate(
            "a picks the layout",
            &format!(
                "(() => {{ const r = document.querySelector('[data-testid=\"{radio}\"]'); if (!r) return false; \
                 const input = r.matches('input') ? r : r.querySelector('input'); (input || r).click(); return true; }})()"
            ),
        )
        .await?;
    report
        .notes
        .push(format!("a: attempt {at}: picked {layout}: {layout_picked}"));
    tokio::time::sleep(Duration::from_secs(1)).await;
    Ok(())
}

/// Create pressed: the write it sends must come, held back by the forced deny.
async fn press_create(drive: &mut Driver, report: &mut ScenarioReport, at: usize) -> Result<(), ScenarioError> {
    let since = drive.seen_so_far();
    let created = drive
        .evaluate(
            "a presses Create",
            &click_named("[data-tid=\"create-and-edit-submit-button\"]", "Create"),
        )
        .await?;
    report.notes.push(format!("a: attempt {at}: Create pressed: {created}"));
    let wanted = RequestPattern::new(None, CHANNEL_URL).map_err(|error| ScenarioError::Setup(error.to_string()))?;
    let write = drive
        .wait_for_seen("Create sends a write", "a channel write", since, SENT, |request| {
            request.method != "GET" && wanted.matches_request(request)
        })
        .await;
    match write {
        Ok(write) => report
            .notes
            .push(format!("a: attempt {at}: {} {}", write.method, write.url)),
        Err(error) => report.failures.push(format!("attempt {at}: {error}")),
    }
    Ok(())
}

/// What the dialog says of the write denied, noted, and the dialog closed.
async fn leave_error(drive: &mut Driver, report: &mut ScenarioReport, at: usize) -> Result<(), ScenarioError> {
    let words = drive
        .evaluate(
            "the dialog's words",
            "(() => { const d = document.querySelector('[data-testid=\"x-button\"]'); const s = d && d.closest('.fui-DialogSurface'); return s ? s.innerText.replace(/\\s+/g, ' ') : 'closed'; })()",
        )
        .await?;
    report
        .notes
        .push(format!("a: attempt {at}: after: {}", words.as_str().unwrap_or("?")));
    let closed = drive
        .evaluate(
            "a closes the dialog",
            "(() => { const close = document.querySelector('[data-testid=\"x-button\"], [data-tid=\"cancel-create-channel\"]'); if (!close) return false; close.click(); return true; })()",
        )
        .await?;
    report.notes.push(format!("a: attempt {at}: closed: {closed}"));
    tokio::time::sleep(SETTLE).await;
    Ok(())
}

async fn shot(a: &LabSession, report: &mut ScenarioReport, name: &str) {
    if let Some(note) = save_screenshot_as(ACCOUNT, a, name).await {
        report.notes.push(note);
    }
}
