//! The `team-manage` scenario: a look at the web client's team management (`docs/protocol/teams.md`), one account's page
//! under the armed fence, driven step by step from outside over a Unix socket, so someone reading the page can decide the
//! next step from what the last one showed. Its reports and screenshots go to the capture dir; nothing of them enters the
//! repo.
//!
//! `PROTO_LAB_STEPS_SOCKET` names the socket. A connection sends lines and half-closes; each line is one step, and
//! what it found is answered before the connection ends:
//!
//! - `wait <seconds>`, `shot <name>` (a screenshot in the capture dir), `press <escape|enter|backspace|space>`;
//! - `go <url>`: loads `url` in the page, a `beforeunload` dialog holding it accepted (a page a script's navigation
//!   can't leave while that dialog shows);
//! - `js <code>`: evaluates `code` in the page, answering its value;
//! - `click <css> <regex>`: clicks the first visible element `css` selects whose text or label matches `regex`;
//! - `point <css>`: clicks the element `css` selects as a pointer would;
//! - `hover <css>`, `type <css> <text>`, `insert <text>`;
//! - `seen`: the requests that aren't plain reads since the last `seen`, each with its status and its body read;
//! - `hold <method> <regex> [+text|-text]`: from now on every request of `method` whose URL `regex` finds a match in
//!   (and whose body holds, or lacks, `text`) is force-denied;
//! - `quit`: ends the run.
//!
//! **What it may send.** By default it runs as `load` does, fail-closed, and every write on a team, a group or a channel
//! is held back by a forced deny from before the page loads ([`TEAM_WRITE`]): pressing a button shows what it would send
//! and sends nothing. With `PROTO_LAB_TEAM_WRITES=1` nothing is forced and members are read live, so a write on a team
//! passes only by the lab rule: a new private team, which names nobody, or a team whose members, read fresh and in full,
//! are all lab accounts; a new team named with [`PROBE`] proves the fence first by a forced deny. Every deny is expected
//! either way: a denied write only shows what the page tried.
//!
//! `PROTO_LAB_ACCOUNT` picks the account (`a`, `b` or `c`; `a` when unset).

use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::config::{Alias, LabPaths};
use crate::fence_http::ForcedDeny;
use crate::session::{LabSession, SessionOptions};

use super::ab_chat::{load, save_screenshot_as};
use super::ab_trouter::size;
use super::channel_create::click_named;
use super::dsl::{Driver, Key, Selector};
use super::{DenyPolicy, RunOptions, ScenarioError, ScenarioName, ScenarioReport};

/// What a new team's name holds to be force-denied, as the enforcement probe of a run that writes.
pub const PROBE: &str = "lab forced deny probe";

/// The label of a forced deny a `hold` step added.
pub const HELD_BY_A_STEP: &str = "held back by a step";

/// The label of the forced deny of every write on a team, a group or a channel.
pub const TEAM_WRITE: &str = "a team's write";

/// What makes a write one on a team, a group or a channel, on any host.
pub const TEAM_URL: &str = r"(?i)/teams?(/|\?|$)|/groups?(/|\?|$)|/channels?(warmup)?([/?]|$)|/standardchannel|/standalonechannels|/tags?(/|\?|$)|/teamwork";

/// The methods denied on [`TEAM_URL`]: every one but a read.
const WRITE_METHODS: [&str; 4] = ["POST", "PUT", "PATCH", "DELETE"];

/// The services `seen` looks at: the middle tier, the provisioning service, CSA and the chat service on a Teams origin.
const TEAM_SERVICES: [&str; 4] = [
    "teams.cloud.microsoft/api/mt/",
    "teams.cloud.microsoft/fabric/",
    "teams.cloud.microsoft/api/csa/",
    "teams.cloud.microsoft/api/chatsvc/",
];

/// What makes `seen` leave a body out: tokens never reach stdout or a socket's answer.
const TOKEN_WORDS: [&str; 3] = ["token", "bearer", "eyj"];

/// The rules the run forces when it holds writes back.
///
/// # Errors
/// Never: the patterns are fixed.
pub fn team_rules() -> Result<Vec<ForcedDeny>, regex::Error> {
    WRITE_METHODS
        .iter()
        .map(|method| Ok(ForcedDeny::new(method, TEAM_URL)?.labelled(TEAM_WRITE)))
        .collect()
}

/// With writes let through, what proves the fence holds first: a new team named with [`PROBE`] is force-denied, and must
/// fail in the page.
///
/// # Errors
/// Never: the pattern is fixed.
pub fn probe_rule() -> Result<ForcedDeny, regex::Error> {
    Ok(ForcedDeny::new("POST", r"/beta/teams/create")?
        .with_body_containing(PROBE)
        .labelled(PROBE))
}

/// With writes let through, the forced denies: this run's probe ([`probe_rule`]), and any earlier chat scenario's probe
/// the web client still holds unsent and sends again once it loads ([`super::ab_chat::probe_rules`]): with members read
/// live it would otherwise go through to its lab chat (seen 2026-11-02: a's probe posted to the a–b chat by a writing run).
///
/// # Errors
/// Never: the patterns are fixed.
pub fn written_rules() -> Result<Vec<ForcedDeny>, regex::Error> {
    let mut rules = vec![probe_rule()?];
    rules.extend(super::ab_chat::probe_rules(None)?);
    Ok(rules)
}

fn account() -> Alias {
    match std::env::var("PROTO_LAB_ACCOUNT").as_deref() {
        Ok("b") => Alias::B,
        Ok("c") => Alias::C,
        _ => Alias::A,
    }
}

/// Runs `team-manage` (see the module docs).
///
/// # Errors
/// When the run can't start: no Chrome, no fence, a browser that can't be armed, no socket named.
pub async fn run(paths: &LabPaths, options: RunOptions) -> Result<ScenarioReport, ScenarioError> {
    super::require_lab_scenario(super::ScenarioName::TeamManage)?;
    let writes = std::env::var("PROTO_LAB_TEAM_WRITES").as_deref() == Ok("1");
    let session_options = SessionOptions {
        capture: Some(ScenarioName::TeamManage.as_str().to_owned()),
        capture_request_bodies: true,
        display: options.display,
        live_members: writes,
        ..SessionOptions::fail_closed()
    };
    let alias = account();
    let mut page = LabSession::open_with(paths, alias, session_options).await?;
    let socket: PathBuf = std::env::var_os("PROTO_LAB_STEPS_SOCKET")
        .map(PathBuf::from)
        .ok_or_else(|| ScenarioError::Setup("PROTO_LAB_STEPS_SOCKET names no socket".to_owned()))?;
    let mut report = ScenarioReport::new(ScenarioName::TeamManage);
    let mut drive = page.driver();
    match if writes { written_rules() } else { team_rules() } {
        Ok(rules) => rules.into_iter().for_each(|rule| page.force_deny(rule)),
        Err(error) => report.failures.push(error.to_string()),
    }
    if report.failures.is_empty()
        && let Err(error) = steps(alias, &mut page, &mut drive, &mut report, &socket, options).await
    {
        report.failures.push(error.to_string());
    }
    report.close_account(alias, page, &DenyPolicy::AllExpected).await;
    let _ = std::fs::remove_file(&socket);
    Ok(report)
}

async fn steps(
    alias: Alias,
    page: &mut LabSession,
    drive: &mut Driver,
    report: &mut ScenarioReport,
    socket: &std::path::Path,
    options: RunOptions,
) -> Result<(), ScenarioError> {
    load(alias, page, options.load_timeout).await?;
    size(page).await?;
    serve(alias, page, drive, report, socket, &TEAM_SERVICES).await
}

/// Takes steps on `socket` until one says `quit`; `seen` looks at the requests to `services`.
pub(crate) async fn serve(
    alias: Alias,
    page: &mut LabSession,
    drive: &mut Driver,
    report: &mut ScenarioReport,
    socket: &std::path::Path,
    services: &[&str],
) -> Result<(), ScenarioError> {
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket).map_err(|error| ScenarioError::Setup(error.to_string()))?;
    let mut seen = Seen {
        at: drive.seen_so_far(),
        services,
    };
    loop {
        let (stream, _) = listener
            .accept()
            .await
            .map_err(|error| ScenarioError::Setup(error.to_string()))?;
        if connection(alias, page, drive, report, stream, &mut seen).await {
            return Ok(());
        }
    }
}

/// Where `seen` is: the requests looked at so far, and the services it looks at.
struct Seen<'a> {
    at: usize,
    services: &'a [&'a str],
}

/// One connection's steps; whether a `quit` ended the run.
async fn connection(
    alias: Alias,
    page: &LabSession,
    drive: &mut Driver,
    report: &mut ScenarioReport,
    stream: UnixStream,
    seen: &mut Seen<'_>,
) -> bool {
    let (reading, mut writing) = stream.into_split();
    let mut lines = BufReader::new(reading).lines();
    let mut quit = false;
    while let Ok(Some(line)) = lines.next_line().await {
        let answer = if line.trim() == "quit" {
            quit = true;
            "bye".to_owned()
        } else {
            step(alias, page, drive, report, &line, seen)
                .await
                .unwrap_or_else(|error| format!("failed: {error}"))
        };
        if writing.write_all(format!("{answer}\n").as_bytes()).await.is_err() {
            break;
        }
    }
    let _ = writing.shutdown().await;
    quit
}

/// One step's answer.
async fn step(
    alias: Alias,
    page: &LabSession,
    drive: &mut Driver,
    report: &mut ScenarioReport,
    line: &str,
    seen: &mut Seen<'_>,
) -> Result<String, ScenarioError> {
    let (verb, rest) = line.trim().split_once(' ').unwrap_or((line.trim(), ""));
    match verb {
        "wait" => {
            tokio::time::sleep(Duration::from_secs(rest.parse().unwrap_or(1))).await;
            Ok("waited".to_owned())
        }
        "shot" => Ok(save_screenshot_as(alias, page, rest)
            .await
            .unwrap_or_else(|| "no screenshot".to_owned())),
        "press" => {
            let key = match rest {
                "enter" => Key::ENTER,
                "backspace" => Key::BACKSPACE,
                "space" => Key::SPACE,
                _ => Key::ESCAPE,
            };
            drive.press("a key", key).await?;
            Ok("pressed".to_owned())
        }
        "go" => {
            drive.navigate("a step's navigation", rest).await?;
            Ok("loaded".to_owned())
        }
        "js" => Ok(drive.evaluate("the page", rest).await?.to_string()),
        "click" => {
            let (css, name) = rest.split_once(' ').unwrap_or((rest, "."));
            Ok(drive.evaluate("a click", &click_named(css, name)).await?.to_string())
        }
        "point" => {
            drive.pointer_click("a pointer's click", Selector::Css(rest)).await?;
            Ok("clicked".to_owned())
        }
        "hover" => {
            drive.hover("a hover", Selector::Css(rest)).await?;
            Ok("hovered".to_owned())
        }
        "type" => {
            let (css, text) = rest.split_once(' ').unwrap_or((rest, ""));
            drive.type_into("typing", Selector::Css(css), text).await?;
            Ok("typed".to_owned())
        }
        "insert" => {
            drive.insert_text("typing", rest).await?;
            Ok("inserted".to_owned())
        }
        "seen" => Ok(writes_seen(drive, report, &mut seen.at, seen.services).await),
        "hold" => hold(page, rest),
        other => Ok(format!("unknown step {other}")),
    }
}

/// A `hold` step: from now on every request of its method whose URL its pattern finds a match in (and whose body holds,
/// or lacks, its text) is force-denied.
fn hold(page: &LabSession, rest: &str) -> Result<String, ScenarioError> {
    let (method, rest) = rest.split_once(' ').unwrap_or(("POST", rest));
    let (pattern, body) = rest.split_once(' ').unwrap_or((rest, ""));
    let mut rule = ForcedDeny::new(method, pattern).map_err(|error| ScenarioError::Setup(error.to_string()))?;
    if let Some(text) = body.strip_prefix('+') {
        rule = rule.with_body_containing(text);
    } else if let Some(text) = body.strip_prefix('-') {
        rule = rule.with_body_lacking(text);
    }
    page.force_deny(rule.labelled(HELD_BY_A_STEP));
    Ok("held".to_owned())
}

/// The requests since `seen` on the services team management uses (never another host, whose bodies can carry a token),
/// each as `<method> <url> -> <status>`, and the body of one that isn't a plain read unless it holds anything like a token.
async fn writes_seen(drive: &mut Driver, report: &mut ScenarioReport, seen: &mut usize, services: &[&str]) -> String {
    let now = drive.seen_so_far();
    let requests: Vec<_> = drive.requests().get(*seen..now).unwrap_or_default().to_vec();
    *seen = now;
    let mut lines = Vec::new();
    for request in requests {
        let on_a_service = services.iter().any(|service| request.url.contains(service));
        if !on_a_service || matches!(request.method.as_str(), "OPTIONS" | "HEAD") {
            continue;
        }
        if request.method == "GET" {
            lines.push(format!(
                "GET {} -> {:?}",
                teams_fence::log_url(&request.url),
                request.status
            ));
            continue;
        }
        let read = drive.read_body("a body", &request).await.unwrap_or(request);
        let body = read.body.as_deref().unwrap_or("");
        let body = if TOKEN_WORDS.iter().any(|word| body.to_ascii_lowercase().contains(word)) {
            "(a body naming a token, left out)"
        } else {
            body
        };
        let url = teams_fence::log_url(&read.url);
        report.notes.push(format!("seen: {} {url}", read.method));
        lines.push(format!("{} {url} -> {:?} {body}", read.method, read.status));
    }
    lines.join("\n")
}
