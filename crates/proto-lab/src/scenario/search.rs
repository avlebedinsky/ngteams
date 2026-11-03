//! The `search` scenario: account a searches in the web client's title bar, as a person does, and what each step reads
//! is recorded (`docs/protocol/search.md`). Searching only reads: nothing is written and nobody is reached, so the
//! session is fail-closed (as `load`'s) and every deny it sees is expected.
//!
//! The steps:
//! 1. a opens the web client and waits for it to load.
//! 2. Ctrl+Alt+G opens the box's "go to" mode (chats and channels by name), shown with the query typed, then Escape.
//! 3. Ctrl+E focuses the search box (a click on the box when the key doesn't reach it) and a types the query
//!    ([`QUERY`], or `PROTO_LAB_SEARCH_QUERY` when set: a name finds people) a character at a time, as a person
//!    types: each character may ask the suggestions.
//! 4. Enter shows the results: the query is asked of the search service.
//! 5. a opens each of the results' tabs ([`TABS`]) the page shows, each asking again for its kind.
//! 6. On the Messages tab a opens the first message found: its chat opens at the message.
//! 7. Escape closes the search.
//!
//! Every step saves a screenshot in the capture dir, and the notes count the search requests and name the tabs the
//! page showed.

use std::time::Duration;

use crate::config::{Alias, LabPaths};
use crate::session::{LabSession, SessionOptions};

use super::ab_chat::{load, save_screenshot_as};
use super::ab_trouter::size;
use super::dsl::{Driver, Key, RequestPattern, StepError, js_string};
use super::{RunOptions, ScenarioError, ScenarioName, ScenarioReport};

/// The account that searches.
pub const ACCOUNT: Alias = Alias::A;

/// What a searches for: a word the lab's messages hold (its probes, its meetings), the lab accounts' names among the
/// people it may find.
pub const QUERY: &str = "lab";

/// The query typed: `PROTO_LAB_SEARCH_QUERY`, else [`QUERY`].
fn query() -> String {
    std::env::var("PROTO_LAB_SEARCH_QUERY")
        .ok()
        .filter(|query| !query.trim().is_empty())
        .unwrap_or_else(|| QUERY.to_owned())
}

/// The suggestions asked as the query is typed.
pub const SUGGESTIONS: &str = r"substrate\.office\.com/search/api/v1/suggestions";

/// The results asked once the query is entered, and for each tab.
pub const QUERY_URL: &str = r"substrate\.office\.com/search(service)?/api/v2/query";

/// Every search request.
const ANY_SEARCH: &str = r"substrate\.office\.com/search(service)?/api/";

/// The results' tabs a opens, as Teams names them.
pub const TABS: [&str; 6] = [
    "Messages",
    "People",
    "Files",
    "Group chats",
    "Teams and Channels",
    "All",
];

/// How long an answer gets.
const ANSWERED: Duration = Duration::from_secs(30);

/// A step's pause: the page showing what it was answered.
const SETTLE: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SearchWaits {
    settle: Duration,
    shortcut: Duration,
    character: Duration,
    answered: Duration,
}

impl Default for SearchWaits {
    fn default() -> Self {
        Self {
            settle: SETTLE,
            shortcut: Duration::from_millis(800),
            character: Duration::from_millis(400),
            answered: ANSWERED,
        }
    }
}

/// Ctrl+E: Teams' key for the search box.
const CTRL_E: Key = Key {
    key: "e",
    code: "KeyE",
    virtual_key: 69,
    text: None,
    modifiers: 2,
};

/// Ctrl+Alt+G: Teams' key for the search box's "go to" mode.
const CTRL_ALT_G: Key = Key {
    key: "g",
    code: "KeyG",
    virtual_key: 71,
    text: None,
    modifiers: 3,
};

/// Runs `search` (see the module docs).
///
/// # Errors
/// When the run can't start: no Chrome, a browser whose capture setup fails.
pub async fn run(paths: &LabPaths, options: RunOptions) -> Result<ScenarioReport, ScenarioError> {
    let session_options = SessionOptions {
        capture: Some(ScenarioName::Search.as_str().to_owned()),
        capture_request_bodies: true,
        display: options.display,
        ..SessionOptions::default()
    };
    let a = LabSession::open_with(paths, ACCOUNT, session_options).await?;
    Ok(finish(a, options, SearchWaits::default()).await)
}

async fn finish(mut a: LabSession, options: RunOptions, waits: SearchWaits) -> ScenarioReport {
    let mut report = ScenarioReport::new(ScenarioName::Search);
    let mut drive = a.driver();
    if let Err(error) = steps(&mut a, &mut drive, &mut report, options, waits).await {
        report.failures.push(error.to_string());
        shot(&a, &mut report, "search-failure.png").await;
    }
    drive.seen_so_far();
    for (what, pattern) in [
        ("suggestions", SUGGESTIONS),
        ("queries", QUERY_URL),
        ("search requests", ANY_SEARCH),
    ] {
        if let Ok(pattern) = RequestPattern::new(None, pattern) {
            let tally = super::own_presence::tally(&format!("a: {what}"), &pattern, drive.requests());
            report.notes.push(tally);
        }
    }
    report.close_account(ACCOUNT, a).await;
    report
}

async fn steps(
    a: &mut LabSession,
    drive: &mut Driver,
    report: &mut ScenarioReport,
    options: RunOptions,
    waits: SearchWaits,
) -> Result<(), ScenarioError> {
    load(ACCOUNT, a, options.load_timeout).await?;
    size(a).await?;
    pause(drive, waits.settle).await;
    let query = query();
    go_to(a, drive, report, &query, waits).await?;
    focus_box(drive, report, waits.shortcut).await?;
    suggest_and_query(a, drive, report, &query, waits).await?;
    report.notes.push(tabs_note(drive).await);
    for tab in TABS {
        visit_tab(a, drive, report, tab, waits).await?;
    }
    drive.press("a closes the search", Key::ESCAPE).await?;
    pause(drive, waits.settle).await;
    shot(a, report, "search-closed.png").await;
    Ok(())
}

/// The go-to mode (Ctrl+Alt+G) with a chat's name typed, then left, and the box too.
async fn go_to(
    a: &mut LabSession,
    drive: &mut Driver,
    report: &mut ScenarioReport,
    query: &str,
    waits: SearchWaits,
) -> Result<(), ScenarioError> {
    drive.press("a presses Ctrl+Alt+G", CTRL_ALT_G).await?;
    tokio::time::sleep(waits.shortcut).await;
    drive.insert_text("a types a chat's name", query).await?;
    pause(drive, waits.settle).await;
    shot(a, report, "search-goto.png").await;
    drive.press("a leaves the go-to mode", Key::ESCAPE).await?;
    pause(drive, waits.settle).await;
    drive.press("a leaves the search box", Key::ESCAPE).await?;
    pause(drive, waits.settle).await;
    Ok(())
}

/// The query typed a character at a time (the suggestions), then entered (the results).
async fn suggest_and_query(
    a: &mut LabSession,
    drive: &mut Driver,
    report: &mut ScenarioReport,
    query: &str,
    waits: SearchWaits,
) -> Result<(), ScenarioError> {
    let since = drive.seen_so_far();
    for ch in query.chars() {
        drive.insert_text("a types the query", &ch.to_string()).await?;
        tokio::time::sleep(waits.character).await;
    }
    let suggested = answered(drive, "the suggestions", ANY_SEARCH, since, waits.answered).await;
    report.notes.push(note("suggestions", &suggested));
    pause(drive, waits.settle).await;
    shot(a, report, "search-suggestions.png").await;
    let since = drive.seen_so_far();
    drive.press_enter("a enters the query").await?;
    let queried = answered(drive, "the query", QUERY_URL, since, waits.answered).await;
    report.notes.push(note("query", &queried));
    queried?;
    pause(drive, waits.settle).await;
    shot(a, report, "search-results.png").await;
    Ok(())
}

/// One results tab clicked and what it asked; on Messages, the first message opened and the query entered again.
async fn visit_tab(
    a: &mut LabSession,
    drive: &mut Driver,
    report: &mut ScenarioReport,
    tab: &str,
    waits: SearchWaits,
) -> Result<(), ScenarioError> {
    let since = drive.seen_so_far();
    if !click_tab(drive, tab).await? {
        report.notes.push(format!("a: no {tab} tab shown"));
        return Ok(());
    }
    let asked = answered(drive, tab, QUERY_URL, since, waits.answered).await;
    report.notes.push(note(tab, &asked));
    pause(drive, waits.settle).await;
    shot(
        a,
        report,
        &format!("search-tab-{}.png", tab.replace(' ', "-").to_lowercase()),
    )
    .await;
    if tab == "Messages" {
        open_first_message(a, drive, report, waits.settle).await;
        pause(drive, waits.settle).await;
        focus_box(drive, report, waits.shortcut).await?;
        drive.press_enter("a enters the query again").await?;
        pause(drive, waits.settle).await;
    }
    Ok(())
}

/// Ctrl+Alt+G, then the box clicked when the key left the focus elsewhere.
async fn focus_box(drive: &mut Driver, report: &mut ScenarioReport, shortcut: Duration) -> Result<(), ScenarioError> {
    drive.press("a presses Ctrl+E", CTRL_E).await?;
    tokio::time::sleep(shortcut).await;
    let focused = drive
        .evaluate(
            "the search box has the focus",
            "(() => { const e = document.activeElement; return !!e && e.tagName === 'INPUT' && \
             /search|ctrl/i.test((e.placeholder || '') + ' ' + (e.getAttribute('aria-label') || '') + ' ' + \
             (e.id || '')); })()",
        )
        .await?;
    if focused.as_bool() == Some(true) {
        report.notes.push("a: Ctrl+E focused the search box".to_owned());
        return Ok(());
    }
    let clicked = drive
        .evaluate(
            "a clicks the search box",
            "(() => { const e = [...document.querySelectorAll('input')].find(e => e.getClientRects().length > 0 && \
             /search|ctrl/i.test((e.placeholder || '') + ' ' + (e.getAttribute('aria-label') || '') + ' ' + \
             (e.id || ''))); if (!e) return false; e.focus(); e.click(); return true; })()",
        )
        .await?;
    if clicked.as_bool() == Some(true) {
        report.notes.push("a: the search box was clicked".to_owned());
        Ok(())
    } else {
        Err(ScenarioError::Setup("no search box to focus".to_owned()))
    }
}

/// Clicks the results' tab named `name`; false when the page shows none.
async fn click_tab(drive: &mut Driver, name: &str) -> Result<bool, StepError> {
    let expression = format!(
        "(() => {{ const norm = s => (s || '').replace(/\\s+/g, ' ').trim(); const n = {}; \
         const e = [...document.querySelectorAll('[role=\"tab\"], [role=\"menuitemradio\"], button')] \
         .filter(e => e.getClientRects().length > 0) \
         .find(e => norm(e.innerText).toLowerCase() === n.toLowerCase() || \
         norm(e.getAttribute('aria-label')).toLowerCase() === n.toLowerCase()); \
         if (!e) return false; e.click(); return true; }})()",
        js_string(name)
    );
    Ok(drive
        .evaluate(&format!("a opens the {name} tab"), &expression)
        .await?
        .as_bool()
        == Some(true))
}

/// Opens the first message the Messages tab lists, and notes what it read.
async fn open_first_message(a: &LabSession, drive: &mut Driver, report: &mut ScenarioReport, settle: Duration) {
    let since = drive.seen_so_far();
    let expression = "(() => { const items = [...document.querySelectorAll('[role=\"listitem\"], [role=\"option\"], \
                      [role=\"row\"], [data-tid*=\"search\"] [role=\"button\"]')].filter(e => \
                      e.getClientRects().length > 0 && e.closest('[role=\"main\"], main, body')); \
                      const e = items.find(e => (e.innerText || '').trim().length > 0); if (!e) return false; e.click(); \
                      return true; })()";
    match drive.evaluate("a opens the first message found", expression).await {
        Ok(value) if value.as_bool() == Some(true) => {
            tokio::time::sleep(settle).await;
            drive.seen_so_far();
            let reads: Vec<String> = drive
                .requests()
                .iter()
                .skip(since)
                .map(|seen| format!("{} {}", seen.method, crate::capture_hold::without_query(&seen.url)))
                .filter(|line| line.contains("/messages") || line.contains("/conversations"))
                .take(12)
                .collect();
            report
                .notes
                .push(format!("a: opening a message found read: {}", reads.join(" ; ")));
            shot(a, report, "search-message-opened.png").await;
        }
        Ok(_) => report.notes.push("a: no message found to open".to_owned()),
        Err(error) => report.notes.push(format!("a: can't open a message found: {error}")),
    }
}

/// The tabs the results show, by name.
async fn tabs_note(drive: &mut Driver) -> String {
    let expression = "(() => { const norm = s => (s || '').replace(/\\s+/g, ' ').trim(); \
                      return [...document.querySelectorAll('[role=\"tab\"]')].filter(e => \
                      e.getClientRects().length > 0).map(e => norm(e.innerText) || norm(e.getAttribute('aria-label')))\
                      .filter(n => n && n.length < 40).join(' | '); })()";
    match drive.evaluate("the results' tabs", expression).await {
        Ok(value) => format!("a: the results' tabs: {}", value.as_str().unwrap_or("?")),
        Err(error) => format!("a: can't list the results' tabs: {error}"),
    }
}

/// Waits for the first request matching `pattern` since `since` to be answered or to fail.
async fn answered(
    drive: &mut Driver,
    what: &str,
    pattern: &str,
    since: usize,
    timeout: Duration,
) -> Result<super::dsl::SeenRequest, ScenarioError> {
    let wanted = RequestPattern::new(Some("POST"), pattern).map_err(|error| ScenarioError::Setup(error.to_string()))?;
    drive
        .wait_for_seen(
            &format!("{what} is answered"),
            &format!("{what}, answered or failed"),
            since,
            timeout,
            |seen| wanted.matches(&seen.method, &seen.url) && (seen.status.is_some() || seen.failed.is_some()),
        )
        .await
        .map_err(ScenarioError::from)
}

fn note(what: &str, outcome: &Result<super::dsl::SeenRequest, ScenarioError>) -> String {
    match outcome {
        Ok(seen) => format!(
            "a: {what}: {} {} answered {:?}, failed {:?}",
            seen.method,
            crate::capture_hold::without_query(&seen.url),
            seen.status,
            seen.failed
        ),
        Err(error) => format!("a: {what}: {error}"),
    }
}

async fn pause(drive: &mut Driver, settle: Duration) {
    tokio::time::sleep(settle).await;
    drive.seen_so_far();
}

async fn shot(a: &LabSession, report: &mut ScenarioReport, name: &str) {
    if let Some(note) = save_screenshot_as(ACCOUNT, a, name).await {
        report.notes.push(note);
    }
}

#[cfg(test)]
#[path = "../../tests/support/scenario/search_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "../../tests/support/scenario/search_driver_tests.rs"]
mod driver_tests;
