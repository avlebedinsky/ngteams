//! QA scenes of an own server's administration (task 203, round 1), on the real `ngteams-server` binary
//! (`NGTEAMS_QA_SERVER_BIN`): each one fails when the server is the one in this process.
//!
//! - [`first_run_on_the_binary`]: a fresh server's first administrator made with the setup code the binary logged at
//!   its first start, after a sign-in without the server's certificate authority (refused, said, no entry left) and a
//!   wrong code; a second first administrator refused by the server and not offered by the window; where every
//!   request, password and code went.
//! - [`manage_users_seeded`]: Manage users over a server seeded with synthetic users (more than one page, a long
//!   name): "Show more", search and its empty state, a name the server refuses as too long, a user name taken, an
//!   edit, a typed and a suggested password reset with Copy and with and without "ask for a new password", an
//!   administrator made and unmade, the only administrator neither demoted nor deleted, a deletion cancelled with
//!   Escape and one confirmed, the list loading and failing while the server is stopped, and Try again.
//! - [`roles_while_running`]: a user promoted and demoted while the app runs (what the page shows now and after the
//!   app starts again), and a user who can't sign in any more while their client runs: the client signs out and
//!   the window does not ask for her password again.
//! - [`page_at`]: the page and its dialogs at 480 px and 1280 px, in Teams Light and Dark, keyboard only; at 1280 px
//!   the account menu over the list, whose item over a user's row takes the click alone.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{App, AsyncApp};
use teams_mock::{ChatServiceMock, LAB_A, LAB_B};
use teams_ui::account_prompt::NO_ADMIN_YET;
use ui_sandbox::input::{Modifier, VirtualInput, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};

use super::admin::{offers_manage_users, server_user, tabs};
use super::{
    OWN_CA_ENV, OWN_NAME, OWN_ORIGIN_ENV, OWN_SEGMENT_AT, OWN_UPN, RAIL_ADD_AT, ROOT_ENV, SIGN_IN, SIGN_IN_AGAIN_FILE,
    TraceReader, WAIT, accounts_shown, ctl, ensure, field, own, prompt_active, prompt_line, prompt_now,
    quit_and_finish, say, shot, start_with,
};

/// The administrator's password, typed into the first-run form.
const ADMIN_PASSWORD: &str = "first admin horse";
pub const DANA: &str = "dana.qa@lab.example.test";
const EVAN: &str = "evan.qa@lab.example.test";
const LONG_UPN: &str = "a-very-long-synthetic-user-name-for-overflow-checks@lab.example.test";
/// The user signed in beside the administrator in [`roles_while_running`].
pub const RUNNING: &str = "rita.qa@lab.example.test";
const GIVEN: &str = "temporary horse 1";
const TYPED: &str = "typed reset horse 2";
/// The file in the sandbox dir the parent writes the words to look for in the mock's requests to.
pub const NEEDLES_ASK: &str = "say-mock-needles";
/// How many synthetic users the seeded scenes add (with the administrator, Evan and the long one: past one page).
const SEEDED: usize = 58;

fn long_name() -> String {
    let mut name = "Synthetic Person With A Remarkably Long Display Name".to_owned();
    while name.len() < 200 {
        name.push_str(" And More");
    }
    name
}

/// Keys reach the app slowly in the debug build over a long list (see the report): each wait here gets four of the
/// trace's waits before it fails.
fn patient(mut wait: impl FnMut() -> SandboxResult) -> SandboxResult {
    let mut last = wait();
    for _ in 0..3 {
        if last.is_ok() {
            break;
        }
        last = wait();
    }
    last
}

/// [`super::admin::admin_line`], patiently.
fn admin_line(trace: &mut TraceReader, running: &Running, change: &str, kind: &str) -> SandboxResult {
    patient(|| super::admin::admin_line(trace, running, change, kind))
}

/// [`super::admin::listed`], patiently.
fn listed(trace: &mut TraceReader, running: &Running, server: &own::Own, upns: &[&str]) -> SandboxResult {
    patient(|| super::admin::listed(trace, running, server, upns))
}

fn require_binary(server: &own::Own) -> SandboxResult {
    ensure(server.log().is_some(), || {
        "this scene needs the real ngteams-server binary (NGTEAMS_QA_SERVER_BIN)".to_owned()
    })
}

/// Where this round's evidence goes besides `target/ui-shots/`.
fn evidence(name: &str, text: &str) -> SandboxResult {
    let dir = super::shots_dir()?;
    fs::write(dir.join(name), text)?;
    Ok(())
}

/// The setup code `serve` logged at its first start.
fn first_start_code(server: &own::Own) -> SandboxResult<String> {
    let log = server.log().unwrap_or_default();
    let code = log
        .lines()
        .find_map(|line| line.split_once("setup code: ").map(|(_, code)| code.trim().to_owned()))
        .ok_or_else(|| format!("serve logged no setup code: {log}"))?;
    // the log line may carry colour codes or trailing fields
    Ok(code
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect())
}

/// The display name `user list` gives `upn`.
fn server_name(server: &own::Own, upn: &str) -> SandboxResult<Option<String>> {
    Ok(server
        .cli(&["user", "list"], None)?
        .lines()
        .find_map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            (fields.first() == Some(&upn)).then(|| fields.get(3).map(|name| (*name).to_owned()))
        })
        .flatten())
}

/// The password grant for `upn` with `password`, as the server answers it: `ok`, `must-change`, `disabled`,
/// `wrong`, or the body's error otherwise. Nothing of the answer's tokens is kept.
fn grant(server: &own::Own, upn: &str, password: &str) -> SandboxResult<String> {
    let client_id = teams_auth::token::EntraConfig::teams_native().client_id().to_owned();
    let output = Command::new("curl")
        .args(["-sS", "--cacert"])
        .arg(server.ca_file())
        .args(["-X", "POST", &format!("{}/oauth2/v2.0/token", server.origin())])
        .args(["--data-urlencode", "grant_type=password"])
        .args(["--data-urlencode", &format!("username={upn}")])
        .args(["--data-urlencode", &format!("password={password}")])
        .args(["--data-urlencode", &format!("client_id={client_id}")])
        .args([
            "--data-urlencode",
            "scope=https://api.spaces.skype.com/.default openid profile offline_access",
        ])
        .output()?;
    let body = String::from_utf8_lossy(&output.stdout);
    Ok(if body.contains("\"access_token\"") {
        "ok".to_owned()
    } else if body.contains("user_password_expired") {
        "must-change".to_owned()
    } else if body.contains("AADSTS50057") {
        "disabled".to_owned()
    } else if body.contains("AADSTS50126") {
        "wrong".to_owned()
    } else {
        format!("other: {body}")
    })
}

/// `POST /api/admin/v1/setup` as a second client would send it: the status and the refusal's code.
fn second_setup(server: &own::Own, code: &str) -> SandboxResult<String> {
    let body = serde_json::json!({
        "code": code,
        "upn": "second.admin@lab.example.test",
        "display_name": "Second Admin",
        "password": "second admin horse",
    });
    let output = Command::new("curl")
        .args(["-sS", "-o", "-", "-w", "\nstatus=%{http_code}", "--cacert"])
        .arg(server.ca_file())
        .args(["-H", "content-type: application/json", "-X", "POST"])
        .arg(format!("{}/api/admin/v1/setup", server.origin()))
        .args(["--data", &body.to_string()])
        .output()?;
    Ok(String::from_utf8_lossy(&output.stdout).replace('\n', " "))
}

/// The accounts in `accounts.toml` under `root` that are an own server's: `(upn, server, signed in)`.
fn own_entries(root: &std::path::Path) -> SandboxResult<Vec<(String, String, bool)>> {
    let setup = super::support::runner(root)?.setup;
    Ok(setup
        .accounts()?
        .into_iter()
        .filter_map(|account| {
            let signed = account.is_signed_in();
            account
                .server
                .clone()
                .map(|server| (account.upn.clone(), server, signed))
        })
        .collect())
}

/// Waits for the page's next list of exactly `count` accounts.
fn listed_count(trace: &mut TraceReader, running: &Running, count: usize) -> SandboxResult {
    patient(|| listed_count_once(trace, running, count))
}

fn listed_count_once(trace: &mut TraceReader, running: &Running, count: usize) -> SandboxResult {
    trace
        .next_where(&format!("user-admin listed {count}"), |line| {
            line["view"] == "user-admin"
                && line["change"] == "listed"
                && line["people"].as_array().map_or(0, Vec::len) == count
        })
        .map_err(|error| {
            format!(
                "{error}; the app said {:?}",
                running.line_where(Duration::ZERO, |_| true)
            )
            .into()
        })
}

/// Opens Manage users of `account` once it is offered, and waits for its first page of `count` accounts.
fn page_opened(
    sandbox: &Sandbox,
    trace: &mut TraceReader,
    running: &Running,
    (account, count): (&str, usize),
) -> SandboxResult {
    let start = Instant::now();
    while !offers_manage_users(sandbox, account) {
        ensure(start.elapsed() < WAIT, || {
            format!("{account} isn't offered Manage users")
        })?;
        std::thread::sleep(Duration::from_millis(200));
    }
    // asked again once the account is the one shown: the keyboard is then on its settings' page column
    std::thread::sleep(Duration::from_millis(500));
    ctl(sandbox, &["--account", account, "open", "settings", "users"])?;
    listed_count(trace, running, count)
}

fn backspaces(input: &mut VirtualInput, count: usize) -> SandboxResult {
    for _ in 0..count {
        input.key(keys::BACKSPACE)?;
    }
    Ok(())
}

fn select_all(input: &mut VirtualInput) -> SandboxResult {
    let (a, _) = input.keymap().key_for('a').ok_or("the keymap has no a")?;
    input.key_with(&[Modifier::Ctrl], a)
}

/// The server seeded as the operator would: the administrator, Evan, a user with a long name and user name, one who
/// can't sign in, and [`SEEDED`] more.
fn seeded(server: &own::Own) -> SandboxResult {
    server.add_admin(OWN_UPN, OWN_NAME)?;
    server.add_user(EVAN, "Evan Roe")?;
    server.add_user(LONG_UPN, &long_name())?;
    for at in 1..=SEEDED {
        server.add_user(
            &format!("qa-user-{at:02}@lab.example.test"),
            &format!("Synthetic User {at:02}"),
        )?;
    }
    server.cli(&["user", "disable", "qa-user-03@lab.example.test"], None)?;
    Ok(())
}

/// The words looked for in the mock's requests, and what the child said of them.
fn mock_got_none_of(sandbox: &Sandbox, running: &Running, needles: &[&str]) -> SandboxResult<String> {
    fs::write(sandbox.path(NEEDLES_ASK), needles.join("\n"))?;
    let said = running
        .line_where(WAIT, |line| line.starts_with("mock-needles "))
        .map_err(|error| format!("the mock's requests weren't searched: {error:?}"))?;
    say(&said);
    ensure(
        field(&said, "admin_paths") == Some("0") && field(&said, "hits") == Some("0"),
        || format!("the mock got what it shouldn't: {said}"),
    )?;
    Ok(said)
}

/// The app's debug log of `run` (the child's `app-log-<run>`), all its files.
fn app_log(sandbox: &Sandbox, run: &str) -> SandboxResult<String> {
    let mut text = String::new();
    for entry in fs::read_dir(sandbox.path(&format!("app-log-{run}")))? {
        let path = entry?.path();
        if path.is_file() {
            text.push_str(&fs::read_to_string(&path).unwrap_or_default());
        }
    }
    Ok(text)
}

/// The own server's answers in the app's log: how many, and how many were 401 (a token the server doesn't know: a
/// Microsoft token would be one); and whether any of `secrets` is in the log at all.
fn own_answers(
    sandbox: &Sandbox,
    running: &Running,
    run: &str,
    origin: &str,
    secrets: &[&str],
) -> SandboxResult<String> {
    // the child flushes its log when it sums its requests
    fs::write(sandbox.path(own::REQUESTS_ASK), "")?;
    let requests = running
        .line_where(WAIT, |line| line.starts_with("requests "))
        .map_err(|error| format!("the app's requests weren't said: {error:?}"))?;
    say(&requests);
    let log = app_log(sandbox, run)?;
    let answers: Vec<&str> = log
        .lines()
        .filter_map(|line| line.split_once(" response ").map(|(_, answer)| answer))
        .filter(|answer| answer.split(' ').nth(1).is_some_and(|url| url.starts_with(origin)))
        .collect();
    let unauthorized: Vec<String> = answers
        .iter()
        .filter(|answer| answer.contains(" -> 401 "))
        .map(|answer| answer.split(" headers ").next().unwrap_or_default().to_owned())
        .collect();
    let leaked: Vec<&&str> = secrets.iter().filter(|secret| log.contains(**secret)).collect();
    let summary = format!(
        "own-answers total={} unauthorized={} leaked_in_log={} {requests}\n401s: {unauthorized:?}",
        answers.len(),
        unauthorized.len(),
        leaked.len()
    );
    ensure(leaked.is_empty(), || {
        format!("the app's debug log holds a password or code: {summary}")
    })?;
    ensure(unauthorized.is_empty(), || {
        format!("the own server refused a token it doesn't know: {summary}")
    })?;
    Ok(summary)
}

// --- the first run, on the binary ---

/// See the module docs.
pub fn first_run_on_the_binary(sandbox: &Sandbox) -> SandboxResult {
    let server = own::Own::start()?;
    require_binary(&server)?;
    let code = first_start_code(&server)?;
    say(&format!("qa: serve logged a setup code of {} characters", code.len()));
    let root = tempfile::tempdir()?;
    let kept = [
        (ROOT_ENV, root.path().display().to_string()),
        (OWN_ORIGIN_ENV, server.origin().to_owned()),
        (OWN_CA_ENV, server.ca_file().display().to_string()),
    ];
    let (mut running, mut trace, _) = start_with(sandbox, "admin-qa", &kept)?;
    accounts_shown(&mut trace, &mut running, &[LAB_A, LAB_B])?;
    let mut input = sandbox.input()?;
    untrusted_then_offered(sandbox, &mut input, &running, (&server, root.path()))?;
    admin_created(sandbox, (&mut input, &mut trace), &running, (&server, &code))?;
    let closed = second_admin_refused(sandbox, &mut input, &running, (&server, &code))?;

    // where it all went
    evidence(
        "ngteams-qa-first-run-server-users.txt",
        &server.cli(&["user", "list"], None)?,
    )?;
    let secrets = [ADMIN_PASSWORD, code.as_str()];
    let mock = mock_got_none_of(sandbox, &running, &secrets)?;
    let answers = own_answers(sandbox, &running, "admin-qa", server.origin(), &secrets)?;
    evidence("ngteams-qa-first-run-requests.txt", &format!("{mock}\n{answers}\n"))?;
    fs::copy(
        sandbox.path(own::REQUESTS_FILE),
        super::shots_dir()?.join("ngteams-qa-first-run-request-list.txt"),
    )?;
    evidence("ngteams-qa-first-run-server.log", &server.log().unwrap_or_default())?;
    if !closed.contains("closed") {
        // the window asking for an account is still open: the tray's Quit ends the app anyway
        say("qa: the window asking for an account stayed open after Escape");
    }
    quit_and_finish(sandbox, &mut trace, running)
}

/// Without the server's certificate authority the sign-in is refused and leaves nothing; with it, the window offers
/// to create the administrator. The keyboard is left on the form's setup code.
fn untrusted_then_offered(
    sandbox: &Sandbox,
    input: &mut VirtualInput,
    running: &Running,
    (server, root): (&own::Own, &std::path::Path),
) -> SandboxResult {
    // no certificate authority typed: the server isn't trusted
    input.click(RAIL_ADD_AT.0, RAIL_ADD_AT.1, keys::BTN_LEFT)?;
    prompt_active(sandbox, running)?;
    input.click(OWN_SEGMENT_AT.0, OWN_SEGMENT_AT.1, keys::BTN_LEFT)?;
    input.type_text(server.origin())?;
    input.key(keys::TAB)?;
    input.type_text(OWN_UPN)?;
    input.key(keys::TAB)?;
    input.type_text(ADMIN_PASSWORD)?;
    let before = prompt_now(sandbox, running)?;
    ensure(before.contains(" stage=sign-in "), || {
        format!("an untrusted server's setup was read: {before}")
    })?;
    input.key(keys::ENTER)?;
    let refused = prompt_line(running, SIGN_IN, "the untrusted server's reason", |line| {
        line.contains("signing=false") && !line.contains("problem=-")
    })?;
    say(&format!("qa: without the CA file: {refused}"));
    evidence("ngteams-qa-no-ca-said.txt", &refused)?;
    // the server is there, its certificate untrusted: the form's own field is where to fix it
    ensure(
        refused.contains("certificate isn't trusted")
            && refused.contains("Certificate authority file")
            && !refused.contains("accounts.toml"),
        || format!("the untrusted server's reason: {refused}"),
    )?;
    shot(sandbox, "ngteams-qa-no-ca.png")?;
    let left = own_entries(root)?;
    ensure(left.is_empty(), || {
        format!("the failed sign-in left an entry: {left:?}")
    })?;

    // the certificate authority's file: the server says it has no administrator
    input.key(keys::TAB)?;
    input.type_text(&server.ca_file().display().to_string())?;
    input.key(keys::TAB)?;
    prompt_line(running, SIGN_IN, "the server without an administrator", |line| {
        line.contains(NO_ADMIN_YET)
    })?;
    shot(sandbox, "ngteams-qa-first-run.png")?;

    // Escape on the form: what it does is recorded (Back is the form's way out)
    input.key(keys::ESC)?;
    let escaped = prompt_now(sandbox, running)?;
    say(&format!("qa: Escape on the administrator's form: {escaped}"));
    evidence("ngteams-qa-setup-escape.txt", &escaped)?;
    ensure(escaped.contains(" stage=setup "), || {
        format!("Escape left the administrator's form: {escaped}")
    })?;
    Ok(())
}

/// A wrong code said, then the code `serve` logged at its first start creates the administrator.
fn admin_created(
    sandbox: &Sandbox,
    (input, trace): (&mut VirtualInput, &mut TraceReader),
    running: &Running,
    (server, code): (&own::Own, &str),
) -> SandboxResult {
    // a wrong code, then the one serve logged at its first start
    input.type_text("BBBBB-BBBBB-BBBBB-BBBBB-BBBBB")?;
    // the user name typed on the sign-in form is carried over: each field's text is replaced
    for typed in [OWN_UPN, OWN_NAME, ADMIN_PASSWORD, ADMIN_PASSWORD] {
        input.key(keys::TAB)?;
        select_all(input)?;
        input.type_text(typed)?;
    }
    shot(sandbox, "ngteams-qa-first-run-filled.png")?;
    input.key(keys::ENTER)?;
    let wrong = prompt_line(running, SIGN_IN, "the wrong setup code's reason", |line| {
        line.contains("problem=The setup code isn't right")
    });
    if wrong.is_err() {
        shot(sandbox, "ngteams-qa-first-run-wrong-code-failed.png")?;
        say(&format!("qa: after the wrong code: {}", prompt_now(sandbox, running)?));
    }
    wrong?;
    ensure(server.users()?.is_empty(), || "a wrong code made an account".to_owned())?;
    input.type_text(code)?;
    input.key(keys::ENTER)?;
    let mri = super::own_account_signed_in(sandbox, trace, running)?;
    say(&format!("qa: the first administrator {mri} is on the rail"));
    ensure(server_user(server, OWN_UPN)? == Some((true, true)), || {
        "the first account isn't an enabled administrator".to_owned()
    })?;
    shot(sandbox, "ngteams-qa-first-admin-on-rail.png")?;
    Ok(())
}

/// A second first administrator: the server refuses it, the window doesn't offer it. What the window holds after
/// Escape.
fn second_admin_refused(
    sandbox: &Sandbox,
    input: &mut VirtualInput,
    running: &Running,
    (server, code): (&own::Own, &str),
) -> SandboxResult<String> {
    // a second first administrator: the server refuses it, the window doesn't offer it
    let again = second_setup(server, code)?;
    say(&format!("qa: a second first administrator: {again}"));
    ensure(again.contains("status=409") && again.contains("AdminExists"), || {
        format!("a second first administrator: {again}")
    })?;
    let fresh = server.setup_code();
    say(&format!(
        "qa: setup-code once an administrator exists: {:?}",
        fresh.as_ref().map(String::len)
    ));
    if let Ok(fresh) = fresh {
        let with_fresh = second_setup(server, &fresh)?;
        say(&format!(
            "qa: a second first administrator with a new code: {with_fresh}"
        ));
        ensure(with_fresh.contains("status=409"), || {
            format!("a second first administrator with a new code: {with_fresh}")
        })?;
    }
    input.click(RAIL_ADD_AT.0, RAIL_ADD_AT.1, keys::BTN_LEFT)?;
    prompt_active(sandbox, running)?;
    input.click(OWN_SEGMENT_AT.0, OWN_SEGMENT_AT.1, keys::BTN_LEFT)?;
    input.type_text(server.origin())?;
    tabs(input, 3)?;
    input.type_text(&server.ca_file().display().to_string())?;
    input.key(keys::TAB)?;
    std::thread::sleep(Duration::from_secs(3));
    let offered = prompt_now(sandbox, running)?;
    ensure(
        offered.contains(" stage=sign-in ") && !offered.contains(NO_ADMIN_YET),
        || format!("the window offers a second administrator: {offered}"),
    )?;
    shot(sandbox, "ngteams-qa-second-admin-not-offered.png")?;
    input.key(keys::ESC)?;
    let closed = prompt_now(sandbox, running)?;
    say(&format!("qa: Escape on the sign-in form: {closed}"));
    Ok(closed)
}

// --- Manage users over a seeded server ---

/// The app with the seeded server's administrator signed in (`mode`: `admin-light` or `admin-dark`).
fn seeded_app(
    sandbox: &Sandbox,
    server: &own::Own,
    mode: &str,
    root: &tempfile::TempDir,
) -> SandboxResult<(Running, TraceReader)> {
    let kept = [
        (ROOT_ENV, root.path().display().to_string()),
        (OWN_ORIGIN_ENV, server.origin().to_owned()),
        (OWN_CA_ENV, server.ca_file().display().to_string()),
    ];
    let (mut running, mut trace, _) = start_with(sandbox, mode, &kept)?;
    accounts_shown(&mut trace, &mut running, &[LAB_A, LAB_B])?;
    // the keyboard goes to the window the pointer was last in
    sandbox.input()?.click(700, 400, keys::BTN_LEFT)?;
    Ok((running, trace))
}

/// From the search field: types `query`, waits for `upns`, and opens the first row (Tab past "Add user", Enter).
fn found_and_opened(
    input: &mut VirtualInput,
    trace: &mut TraceReader,
    running: &Running,
    (server, query, upns): (&own::Own, &str, &[&str]),
) -> SandboxResult {
    input.type_text(query)?;
    listed(trace, running, server, upns)?;
    tabs(input, 2)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "dialog", "edit")
}

/// See the module docs.
pub fn manage_users_seeded(sandbox: &Sandbox) -> SandboxResult {
    let server = own::Own::start()?;
    require_binary(&server)?;
    seeded(&server)?;
    let total = server.users()?.len();
    let root = tempfile::tempdir()?;
    let (running, mut trace) = seeded_app(sandbox, &server, "admin-light", &root)?;
    let mut input = sandbox.input()?;
    let admin = format!("{}/{OWN_UPN}", server.origin());
    page_opened(sandbox, &mut trace, &running, (&admin, 50))?;
    shot(sandbox, "ngteams-qa-users-page.png")?;

    // "Show more": search, Add user, the list, then Show more; each step's focus shot
    for step in 1..=4 {
        input.key(keys::TAB)?;
        shot(sandbox, &format!("ngteams-qa-users-tab-{step}.png"))?;
    }
    input.key(keys::ENTER)?;
    listed_count(&mut trace, &running, total)?;
    shot(sandbox, "ngteams-qa-users-more.png")?;
    // where the keyboard is once "Show more" went (the last page read)
    input.key(keys::TAB)?;
    shot(sandbox, "ngteams-qa-users-more-then-tab.png")?;
    // the keyboard back on the search: the page column again, then Tab
    ctl(sandbox, &["--account", admin.as_str(), "open", "settings", "users"])?;
    std::thread::sleep(Duration::from_millis(500));
    input.key(keys::TAB)?;

    // search, and a search that finds nobody
    input.type_text("synthetic user 05")?;
    listed(&mut trace, &running, &server, &["qa-user-05@lab.example.test"])?;
    backspaces(&mut input, "synthetic user 05".len())?;
    listed_count(&mut trace, &running, 50)?;
    input.type_text("zz-nobody")?;
    listed(&mut trace, &running, &server, &[])?;
    shot(sandbox, "ngteams-qa-users-none.png")?;
    backspaces(&mut input, "zz-nobody".len())?;
    listed_count(&mut trace, &running, 50)?;

    // the long name: its row, its dialog, its reset dialog
    found_and_opened(&mut input, &mut trace, &running, (&server, "remarkably", &[LONG_UPN]))?;
    shot(sandbox, "ngteams-qa-edit-long.png")?;
    tabs(&mut input, 4)?;
    input.key(keys::ENTER)?;
    admin_line(&mut trace, &running, "dialog", "reset")?;
    shot(sandbox, "ngteams-qa-reset-long.png")?;
    input.key(keys::ESC)?;
    shot(sandbox, "ngteams-qa-users-long-row.png")?;
    backspaces(&mut input, "remarkably".len())?;
    listed_count(&mut trace, &running, 50)?;

    added(sandbox, &mut input, &mut trace, &running, &server)?;
    edited_and_reset(sandbox, &mut input, &mut trace, &running, &server)?;
    roles_and_deletes(sandbox, &mut input, &mut trace, &running, &server)?;
    loading_and_failing(sandbox, &mut input, &mut trace, &running, &server)?;

    let secrets = [GIVEN, TYPED];
    let mock = mock_got_none_of(sandbox, &running, &secrets)?;
    let answers = own_answers(sandbox, &running, "admin-light", server.origin(), &secrets)?;
    evidence("ngteams-qa-seeded-requests.txt", &format!("{mock}\n{answers}\n"))?;
    evidence(
        "ngteams-qa-seeded-server-users.txt",
        &server.cli(&["user", "list"], None)?,
    )?;
    quit_and_finish(sandbox, &mut trace, running)
}

/// Dana added (a name too long refused in the dialog first, her password shown once), and a user name taken.
fn added(
    sandbox: &Sandbox,
    input: &mut VirtualInput,
    trace: &mut TraceReader,
    running: &Running,
    server: &own::Own,
) -> SandboxResult {
    input.key(keys::TAB)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "dialog", "add")?;
    shot(sandbox, "ngteams-qa-add-empty.png")?;
    let too_long = "N".repeat(300);
    input.type_text(DANA)?;
    input.key(keys::TAB)?;
    input.type_text(&too_long)?;
    input.key(keys::TAB)?;
    input.type_text(GIVEN)?;
    std::thread::sleep(Duration::from_secs(2));
    shot(sandbox, "ngteams-qa-add-name-too-long-typed.png")?;
    input.key(keys::ENTER)?;
    // refused in the dialog, before anything is sent: how long after Enter it is said is how far the keys lagged
    // behind the typing over the 61 users listed
    let typed_at = Instant::now();
    let refused = admin_line(trace, running, "failed", "long-name");
    let waited = typed_at.elapsed();
    say(&format!(
        "qa: a 300-character display name, refused in the dialog {} ms after Enter",
        waited.as_millis()
    ));
    shot(sandbox, "ngteams-qa-add-name-too-long.png")?;
    refused?;
    evidence(
        "ngteams-qa-add-name-too-long.txt",
        &format!(
            "refused in the dialog (long-name) {} ms after Enter",
            waited.as_millis()
        ),
    )?;
    ensure(waited < Duration::from_secs(30), || {
        format!("the keys reached the dialog {} s after the typing", waited.as_secs())
    })?;
    // a fresh dialog with a short name
    input.key(keys::ESC)?;
    input.key(keys::TAB)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "dialog", "add")?;
    input.type_text(DANA)?;
    input.key(keys::TAB)?;
    input.type_text("Dana Doe")?;
    input.key(keys::TAB)?;
    input.type_text(GIVEN)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "done", "add")?;
    // the password given is shown once, with Copy: Done
    shot(sandbox, "ngteams-qa-added-password.png")?;
    input.key(keys::ENTER)?;
    listed_count(trace, running, 50)?;
    ensure(server_user(server, DANA)? == Some((true, false)), || {
        "Dana isn't on the server as an enabled user".to_owned()
    })?;
    let first = grant(server, DANA, GIVEN)?;
    ensure(first == "must-change", || {
        format!("Dana's first sign-in (the box ticked by default): {first}")
    })?;

    input.key(keys::TAB)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "dialog", "add")?;
    input.type_text(EVAN)?;
    input.key(keys::TAB)?;
    input.type_text("Evan Again")?;
    input.key(keys::TAB)?;
    input.type_text(GIVEN)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "failed", "upn-taken")?;
    shot(sandbox, "ngteams-qa-add-taken.png")?;
    input.key(keys::ESC)?;
    Ok(())
}

/// Evan renamed, his password reset as typed (to change) and as suggested (not to change), Copy each time.
fn edited_and_reset(
    sandbox: &Sandbox,
    input: &mut VirtualInput,
    trace: &mut TraceReader,
    running: &Running,
    server: &own::Own,
) -> SandboxResult {
    found_and_opened(input, trace, running, (server, "evan", &[EVAN]))?;
    shot(sandbox, "ngteams-qa-edit-unchanged.png")?;
    select_all(input)?;
    input.type_text("Evan Renamed")?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "done", "edit")?;
    listed(trace, running, server, &[EVAN])?;
    let name = server_name(server, EVAN)?;
    ensure(name.as_deref() == Some("Evan Renamed"), || {
        format!("Evan's name on the server: {name:?}")
    })?;

    // typed, "ask for a new password" left ticked
    tabs(input, 2)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "dialog", "edit")?;
    tabs(input, 4)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "dialog", "reset")?;
    input.type_text(TYPED)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "done", "reset")?;
    shot(sandbox, "ngteams-qa-reset-typed-done.png")?;
    let held = copied(sandbox, input)?;
    ensure(held.as_deref() == Some(TYPED), || format!("Copy gave {held:?}"))?;
    input.key(keys::ENTER)?;
    listed(trace, running, server, &[EVAN])?;
    let typed = grant(server, EVAN, TYPED)?;
    ensure(typed == "must-change", || {
        format!("Evan's sign-in with the typed password: {typed}")
    })?;

    // suggested, the box unticked
    tabs(input, 2)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "dialog", "edit")?;
    tabs(input, 4)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "dialog", "reset")?;
    // the suggestion's button, then the box
    input.key(keys::TAB)?;
    input.key(keys::ENTER)?;
    input.key(keys::TAB)?;
    input.key(keys::SPACE)?;
    shot(sandbox, "ngteams-qa-reset-suggested.png")?;
    // back to the password's field, Enter
    input.key_with(&[Modifier::Shift], keys::TAB)?;
    input.key_with(&[Modifier::Shift], keys::TAB)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "done", "reset")?;
    shot(sandbox, "ngteams-qa-reset-suggested-done.png")?;
    let suggested = copied(sandbox, input)?.ok_or("Copy put nothing on the clipboard")?;
    ensure(suggested.chars().count() >= 8 && suggested != TYPED, || {
        format!("the suggested password is {} characters", suggested.chars().count())
    })?;
    input.key(keys::ENTER)?;
    listed(trace, running, server, &[EVAN])?;
    let with_suggested = grant(server, EVAN, &suggested)?;
    ensure(with_suggested == "ok", || {
        format!("Evan's sign-in with the suggested password, no new one asked: {with_suggested}")
    })?;
    backspaces(input, "evan".len())?;
    listed_count(trace, running, 50)
}

/// Tab to the shown password's Copy, Space: what the clipboard holds then.
fn copied(sandbox: &Sandbox, input: &mut VirtualInput) -> SandboxResult<Option<String>> {
    sandbox.clear_clipboard()?;
    input.key(keys::TAB)?;
    shot(sandbox, "ngteams-qa-reset-copy-focus.png")?;
    input.key(keys::SPACE)?;
    std::thread::sleep(Duration::from_millis(500));
    let offers = sandbox.read_clipboard()?;
    Ok(offers
        .into_iter()
        .find(|(kind, _)| kind.starts_with("text/plain"))
        .map(|(_, text)| String::from_utf8_lossy(&text).into_owned()))
}

/// Dana made an administrator and not, the only administrator neither demoted nor deleted, Evan's deletion cancelled
/// with Escape, then confirmed.
fn roles_and_deletes(
    sandbox: &Sandbox,
    input: &mut VirtualInput,
    trace: &mut TraceReader,
    running: &Running,
    server: &own::Own,
) -> SandboxResult {
    found_and_opened(input, trace, running, (server, "dana.qa", &[DANA]))?;
    tabs(input, 3)?;
    input.key(keys::SPACE)?;
    tabs(input, 4)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "done", "edit")?;
    listed(trace, running, server, &[DANA])?;
    ensure(server_user(server, DANA)? == Some((true, true)), || {
        "Dana isn't an administrator on the server".to_owned()
    })?;
    shot(sandbox, "ngteams-qa-two-admins.png")?;
    tabs(input, 2)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "dialog", "edit")?;
    tabs(input, 3)?;
    input.key(keys::SPACE)?;
    tabs(input, 4)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "done", "edit")?;
    listed(trace, running, server, &[DANA])?;
    ensure(server_user(server, DANA)? == Some((true, false)), || {
        "Dana is still an administrator on the server".to_owned()
    })?;
    backspaces(input, "dana.qa".len())?;
    listed_count(trace, running, 50)?;

    // the only administrator: no demotion, no deletion
    found_and_opened(input, trace, running, (server, "bot-a", &[OWN_UPN]))?;
    tabs(input, 3)?;
    input.key(keys::SPACE)?;
    tabs(input, 4)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "failed", "last-admin")?;
    shot(sandbox, "ngteams-qa-last-admin-demote.png")?;
    input.key(keys::ESC)?;
    tabs(input, 2)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "dialog", "edit")?;
    tabs(input, 5)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "dialog", "delete")?;
    shot(sandbox, "ngteams-qa-delete-self-confirm.png")?;
    tabs(input, 1)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "failed", "last-admin")?;
    shot(sandbox, "ngteams-qa-last-admin-delete.png")?;
    ensure(server_user(server, OWN_UPN)? == Some((true, true)), || {
        "the only administrator changed".to_owned()
    })?;
    input.key(keys::ESC)?;
    backspaces(input, "bot-a".len())?;
    listed_count(trace, running, 50)?;

    evan_deleted(sandbox, input, trace, running, server)
}

/// Evan: Escape on the confirmation keeps him, so does Enter (the keyboard starts on Cancel); then Delete.
fn evan_deleted(
    sandbox: &Sandbox,
    input: &mut VirtualInput,
    trace: &mut TraceReader,
    running: &Running,
    server: &own::Own,
) -> SandboxResult {
    found_and_opened(input, trace, running, (server, "evan", &[EVAN]))?;
    tabs(input, 5)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "dialog", "delete")?;
    shot(sandbox, "ngteams-qa-delete-confirm.png")?;
    input.key(keys::ESC)?;
    ensure(server_user(server, EVAN)?.is_some(), || {
        "Escape deleted Evan".to_owned()
    })?;
    // the search has the keyboard again
    input.key(keys::BACKSPACE)?;
    listed(trace, running, server, &[EVAN])?;
    input.type_text("n")?;
    tabs(input, 2)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "dialog", "edit")?;
    tabs(input, 5)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "dialog", "delete")?;
    // the keyboard starts on Cancel: Enter keeps him
    input.key(keys::ENTER)?;
    std::thread::sleep(Duration::from_secs(1));
    ensure(server_user(server, EVAN)?.is_some(), || {
        "Enter on the confirmation's first button deleted Evan".to_owned()
    })?;
    tabs(input, 2)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "dialog", "edit")?;
    tabs(input, 5)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "dialog", "delete")?;
    tabs(input, 1)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "done", "delete")?;
    listed(trace, running, server, &[])?;
    ensure(server_user(server, EVAN)?.is_none(), || {
        "Evan is still on the server".to_owned()
    })?;
    shot(sandbox, "ngteams-qa-deleted.png")
}

/// The server stopped while the search changes: the list loads, then says the server can't be reached; Try again
/// once it runs again lists the users. The search holds "evan", which lists nobody now.
fn loading_and_failing(
    sandbox: &Sandbox,
    input: &mut VirtualInput,
    trace: &mut TraceReader,
    running: &Running,
    server: &own::Own,
) -> SandboxResult {
    let pid = server.pid().ok_or("the binary's pid")?.to_string();
    let signal = |name: &str| -> SandboxResult {
        let status = Command::new("kill").args([name, pid.as_str()]).status()?;
        ensure(status.success(), || format!("kill {name}: {status}"))
    };
    signal("-STOP")?;
    let stopped = (|| -> SandboxResult {
        input.type_text("x")?;
        std::thread::sleep(Duration::from_secs(2));
        shot(sandbox, "ngteams-qa-users-loading.png")?;
        let start = Instant::now();
        loop {
            let failed = trace.next_where("the list's failure", |line| {
                line["view"] == "user-admin" && line["change"] == "failed"
            });
            if failed.is_ok() {
                break;
            }
            ensure(start.elapsed() < Duration::from_secs(90), || {
                "the list never said the server can't be reached".to_owned()
            })?;
        }
        let said = trace.seen.last().cloned().unwrap_or_default();
        say(&format!("qa: the list with the server stopped: {said}"));
        shot(sandbox, "ngteams-qa-users-error.png")
    })();
    signal("-CONT")?;
    stopped?;
    // Try again: the search, Add user, Try again
    backspaces(input, "evanx".len())?;
    let recovered = listed_count(trace, running, 50);
    if recovered.is_err() {
        tabs(input, 2)?;
        input.key(keys::ENTER)?;
        listed_count(trace, running, 50)?;
    }
    shot(sandbox, "ngteams-qa-users-recovered.png")
}

// --- roles changed while the app runs ---

/// See the module docs.
pub fn roles_while_running(sandbox: &Sandbox) -> SandboxResult {
    let server = own::Own::start()?;
    require_binary(&server)?;
    server.add_admin(OWN_UPN, OWN_NAME)?;
    let rita_id = server.add_user(RUNNING, "Rita Running")?;
    let rita = format!("{}/{RUNNING}", server.origin());
    let admin = format!("{}/{OWN_UPN}", server.origin());
    let mut notes = Vec::new();
    promoted_while_running(sandbox, &server, (&rita, &admin), &mut notes)?;
    demoted_while_shown(sandbox, &server, &rita, &mut notes)?;
    turned_off_while_running(sandbox, &server, (&rita, &admin, &rita_id), &mut notes)?;
    let joined = notes.join("\n");
    say(&format!("qa: {joined}"));
    evidence("ngteams-qa-roles-while-running.txt", &joined)
}

/// The first run: Rita is no administrator; promoted, she isn't offered the page yet.
fn promoted_while_running(
    sandbox: &Sandbox,
    server: &own::Own,
    (rita, admin): (&str, &str),
    notes: &mut Vec<String>,
) -> SandboxResult {
    let root = tempfile::tempdir()?;
    let (running, mut trace) = seeded_app(sandbox, server, "admin-two", &root)?;
    page_opened(sandbox, &mut trace, &running, (admin, 2))?;
    ensure(!offers_manage_users(sandbox, rita), || {
        "Rita, no administrator, is offered Manage users".to_owned()
    })?;
    server.cli(&["user", "promote", RUNNING], None)?;
    std::thread::sleep(Duration::from_secs(5));
    let now = offers_manage_users(sandbox, rita);
    notes.push(format!("promoted while running: offered now={now}"));
    ctl(sandbox, &["--account", rita, "open", "settings", "general"])?;
    shot(sandbox, "ngteams-qa-promoted-now.png")?;
    quit_and_finish(sandbox, &mut trace, running)
}

/// The app again: Rita has the page; demoted while it shows, she is told and it goes.
fn demoted_while_shown(sandbox: &Sandbox, server: &own::Own, rita: &str, notes: &mut Vec<String>) -> SandboxResult {
    let root = tempfile::tempdir()?;
    let (running, mut trace) = seeded_app(sandbox, server, "admin-two", &root)?;
    let mut input = sandbox.input()?;
    page_opened(sandbox, &mut trace, &running, (rita, 2))?;
    notes.push("promoted, after the app started again: offered".to_owned());
    shot(sandbox, "ngteams-qa-promoted-after-restart.png")?;
    server.cli(&["user", "demote", RUNNING], None)?;
    // a key in the search: the page asks the server, which says she's no administrator; the app then takes the
    // page away, at times before the page itself heard the answer (so its reason isn't always traced): the wait
    // ends at the answer traced or at the page gone, whichever comes first
    input.click(700, 173, keys::BTN_LEFT)?;
    input.type_text("a")?;
    let start = Instant::now();
    let traced = loop {
        let answer = trace.next_within(Duration::from_millis(500), |line| {
            line["view"] == "user-admin" && line["change"] == "failed"
        });
        if answer || !offers_manage_users(sandbox, rita) {
            break answer;
        }
        ensure(start.elapsed() < WAIT, || {
            "the page neither traced the server's answer nor went after the demotion".to_owned()
        })?;
    };
    notes.push(format!(
        "demoted while the page shows: the page traced the server's answer={traced}"
    ));
    shot(sandbox, "ngteams-qa-demoted-now.png")?;
    let start = Instant::now();
    while offers_manage_users(sandbox, rita) {
        ensure(start.elapsed() < WAIT, || {
            "Manage users stayed for Rita after the server said she's no administrator".to_owned()
        })?;
        std::thread::sleep(Duration::from_millis(500));
    }
    shot(sandbox, "ngteams-qa-demoted-page-gone.png")?;
    notes.push("demoted while the page shows: the page goes".to_owned());
    quit_and_finish(sandbox, &mut trace, running)
}

/// The app again: no page for Rita; the administrator turns off her "Can sign in" while her client runs.
fn turned_off_while_running(
    sandbox: &Sandbox,
    server: &own::Own,
    (rita, admin, rita_id): (&str, &str, &str),
    notes: &mut Vec<String>,
) -> SandboxResult {
    // the sandbox dir outlives this app: only what this app opened counts
    let _ = fs::remove_file(sandbox.path(SIGN_IN_AGAIN_FILE));
    let root = tempfile::tempdir()?;
    let (running, mut trace) = seeded_app(sandbox, server, "admin-two", &root)?;
    let mut input = sandbox.input()?;
    page_opened(sandbox, &mut trace, &running, (admin, 2))?;
    ensure(!offers_manage_users(sandbox, rita), || {
        "Rita, demoted, is offered Manage users after the app started again".to_owned()
    })?;
    notes.push("demoted, after the app started again: not offered".to_owned());
    tabs(&mut input, 1)?;
    found_and_opened(&mut input, &mut trace, &running, (server, "rita", &[RUNNING]))?;
    tabs(&mut input, 2)?;
    input.key(keys::SPACE)?;
    shot(sandbox, "ngteams-qa-cant-sign-in-ticked.png")?;
    tabs(&mut input, 5)?;
    input.key(keys::ENTER)?;
    admin_line(&mut trace, &running, "done", "edit")?;
    listed(&mut trace, &running, server, &[RUNNING])?;
    shot(sandbox, "ngteams-qa-cant-sign-in-row.png")?;
    ensure(server_user(server, RUNNING)? == Some((false, false)), || {
        "Rita can still sign in on the server".to_owned()
    })?;
    let refused = grant(server, RUNNING, own::PASSWORD)?;
    notes.push(format!("Rita's password grant after: {refused}"));
    ctl(sandbox, &["account", "switch", rita])?;
    let out_after = signed_out_after(sandbox, server)?;
    notes.push(format!(
        "Rita's client signed out {} ms after the switch",
        out_after.as_millis()
    ));
    // the turned-off account's sign-out ends with nothing more to see: the window again would come within this time
    std::thread::sleep(UNASKED);
    let again = fs::read_to_string(sandbox.path(SIGN_IN_AGAIN_FILE)).unwrap_or_default();
    let asked = again.lines().any(|upn| upn == RUNNING);
    let rita_mri = format!("8:orgid:{rita_id}");
    let trace_lines: Vec<String> = trace
        .seen
        .iter()
        .filter(|line| line.to_string().contains(&rita_mri) || line["view"] == "status")
        .map(ToString::to_string)
        .collect();
    shot(sandbox, "ngteams-qa-disabled-client.png")?;
    notes.push(format!(
        "Rita's client after: the sign-in window again for {:?}",
        again.lines().collect::<Vec<_>>()
    ));
    let status = ctl(sandbox, &["status"]).unwrap_or_else(|error| error.to_string());
    notes.push(format!("ngteamsctl status: {status}"));
    notes.push(format!("trace lines of Rita or the status: {trace_lines:?}"));
    ensure(!asked, || {
        "the app opens the sign-in window again for Rita, but an administrator turned her off".to_owned()
    })?;
    quit_and_finish(sandbox, &mut trace, running)
}

/// How long the app has, once Rita's client is signed out, to open the sign-in window again. It opens nothing for an
/// account that an administrator turned off; for an account signed out for another reason it opens the window once
/// the account's engine stops.
const UNASKED: Duration = Duration::from_secs(5);

/// Waits until `ngteamsctl status` says that Rita's client on `server` is signed out: the time from the call.
fn signed_out_after(sandbox: &Sandbox, server: &own::Own) -> SandboxResult<Duration> {
    let start = Instant::now();
    loop {
        let status = ctl(sandbox, &["status"])?;
        let out = status.lines().any(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            words.contains(&RUNNING) && words.contains(&server.origin()) && words.contains(&"signed_out")
        });
        if out {
            return Ok(start.elapsed());
        }
        ensure(start.elapsed() < SIGN_IN, || {
            format!("Rita's client is not signed out after her sign-in was turned off: {status}")
        })?;
        std::thread::sleep(Duration::from_millis(250));
    }
}

// --- the page at a width, light or dark ---

/// The page and its dialogs in a sandbox `width` wide, in Teams Light or Dark, with the keyboard alone; at 1280 px and
/// wider, also the account menu opened over the list and an item over a user's row clicked (`menu_over_the_list`).
pub fn page_at(width: u32, dark: bool) -> SandboxResult {
    let options = SandboxOptions {
        width,
        height: 800,
        niri_extra: NIRI_FULL_WINDOW,
        fill_output: true,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("ngteams Manage users QA", &options)? else {
        return Ok(());
    };
    let server = own::Own::start()?;
    require_binary(&server)?;
    server.add_admin(OWN_UPN, OWN_NAME)?;
    server.add_user(DANA, "Dana Doe")?;
    server.add_user(LONG_UPN, &long_name())?;
    server.cli(&["user", "disable", DANA], None)?;
    let (mode, suffix) = if dark {
        ("admin-dark", format!("-{width}-dark"))
    } else {
        ("admin-light", format!("-{width}"))
    };
    let root = tempfile::tempdir()?;
    let (running, mut trace) = seeded_app(&sandbox, &server, mode, &root)?;
    let mut input = sandbox.input()?;
    let admin = format!("{}/{OWN_UPN}", server.origin());
    page_opened(&sandbox, &mut trace, &running, (&admin, 3))?;
    shot(&sandbox, &format!("ngteams-qa-page{suffix}.png"))?;
    input.key(keys::TAB)?;
    shot(&sandbox, &format!("ngteams-qa-page-search-focus{suffix}.png"))?;
    tabs(&mut input, 2)?;
    input.key(keys::DOWN)?;
    shot(&sandbox, &format!("ngteams-qa-page-list-focus{suffix}.png"))?;
    // Shift+Tab from the list is on "Add user"
    input.key_with(&[Modifier::Shift], keys::TAB)?;
    shot(&sandbox, &format!("ngteams-qa-page-add-focus{suffix}.png"))?;
    input.key(keys::ENTER)?;
    admin_line(&mut trace, &running, "dialog", "add")?;
    shot(&sandbox, &format!("ngteams-qa-add{suffix}.png"))?;
    input.key(keys::ENTER)?;
    shot(&sandbox, &format!("ngteams-qa-add-said{suffix}.png"))?;
    if width >= 1280 {
        long_name_over_three(&mut input, &mut trace, &running)?;
    }
    input.key(keys::ESC)?;
    // the long user's dialogs (the search has the keyboard again)
    found_and_opened(&mut input, &mut trace, &running, (&server, "remarkably", &[LONG_UPN]))?;
    shot(&sandbox, &format!("ngteams-qa-edit-long{suffix}.png"))?;
    tabs(&mut input, 5)?;
    input.key(keys::ENTER)?;
    admin_line(&mut trace, &running, "dialog", "delete")?;
    shot(&sandbox, &format!("ngteams-qa-delete-long{suffix}.png"))?;
    input.key(keys::ESC)?;
    backspaces(&mut input, "remarkably".len())?;
    listed_count(&mut trace, &running, 3)?;
    // the administrator's own refusal, said in the dialog
    found_and_opened(&mut input, &mut trace, &running, (&server, "bot-a", &[OWN_UPN]))?;
    tabs(&mut input, 3)?;
    input.key(keys::SPACE)?;
    tabs(&mut input, 4)?;
    input.key(keys::ENTER)?;
    admin_line(&mut trace, &running, "failed", "last-admin")?;
    shot(&sandbox, &format!("ngteams-qa-last-admin{suffix}.png"))?;
    input.key(keys::ESC)?;
    backspaces(&mut input, "bot-a".len())?;
    listed_count(&mut trace, &running, 3)?;
    // Dana's password reset, shown once
    found_and_opened(&mut input, &mut trace, &running, (&server, "dana", &[DANA]))?;
    tabs(&mut input, 4)?;
    input.key(keys::ENTER)?;
    admin_line(&mut trace, &running, "dialog", "reset")?;
    input.type_text(GIVEN)?;
    input.key(keys::ENTER)?;
    admin_line(&mut trace, &running, "done", "reset")?;
    shot(&sandbox, &format!("ngteams-qa-reset-done{suffix}.png"))?;
    input.key(keys::ENTER)?;
    listed(&mut trace, &running, &server, &[DANA])?;
    shot(&sandbox, &format!("ngteams-qa-after-dialog{suffix}.png"))?;
    if width >= 1280 {
        menu_over_the_list(&sandbox, &mut input, &mut trace, &suffix)?;
    }
    quit_and_finish(&sandbox, &mut trace, running)
}

/// The title bar's account picture at 1280 px, and a point of the account menu's "Do not disturb" over the list's first
/// row.
const AVATAR_AT: (u32, u32) = (1254, 23);
const ITEM_OVER_ROW_AT: (u32, u32) = (1100, 236);

/// The account menu opened over the list: its item over a user's row takes the click alone, no user's dialog opens.
fn menu_over_the_list(
    sandbox: &Sandbox,
    input: &mut VirtualInput,
    trace: &mut TraceReader,
    suffix: &str,
) -> SandboxResult {
    let menu = |change: &'static str| {
        move |line: &serde_json::Value| line["view"] == "status-menu" && line["change"] == change
    };
    input.click(AVATAR_AT.0, AVATAR_AT.1, keys::BTN_LEFT)?;
    trace.next_where("the account menu opened", menu("opened"))?;
    input.move_to(ITEM_OVER_ROW_AT.0, ITEM_OVER_ROW_AT.1)?;
    shot(sandbox, &format!("ngteams-qa-menu-over-rows{suffix}.png"))?;
    input.click(ITEM_OVER_ROW_AT.0, ITEM_OVER_ROW_AT.1, keys::BTN_LEFT)?;
    trace.next_where("the menu's item over the row chosen", menu("chosen"))?;
    trace.none_within(Duration::from_secs(2), "a user's dialog under the menu", |line| {
        line["view"] == "user-admin" && line["change"] == "dialog"
    })?;
    input.move_to(640, 400)?;
    shot(sandbox, &format!("ngteams-qa-menu-item-chosen{suffix}.png"))
}

/// The long name typed as over the 61 users of `manage_users_seeded`, over 3: the time to be refused is the dialog's
/// own, whatever the list behind it.
fn long_name_over_three(input: &mut VirtualInput, trace: &mut TraceReader, running: &Running) -> SandboxResult {
    input.type_text(DANA)?;
    input.key(keys::TAB)?;
    input.type_text(&"N".repeat(300))?;
    input.key(keys::TAB)?;
    input.type_text(GIVEN)?;
    input.key(keys::ENTER)?;
    let typed_at = Instant::now();
    admin_line(trace, running, "failed", "long-name")?;
    say(&format!(
        "qa: a 300-character display name over 3 users, refused in the dialog {} ms after Enter",
        typed_at.elapsed().as_millis()
    ));
    Ok(())
}

// --- the child's side ---

/// An own server's accounts' refresh tokens and skypetokens.
async fn own_secrets(setup: &ngteams::setup::Setup) -> Vec<String> {
    use teams_auth::secrets::SecretStore as _;
    let mut held = Vec::new();
    let accounts = setup.signed_in_accounts().unwrap_or_default();
    for account in accounts.iter().filter(|account| account.server.is_some()) {
        for key in [
            teams_auth::refresh::refresh_token_key(&account.id()),
            teams_auth::authz::skypetoken_key(&account.id()),
        ] {
            if let Ok(Some(secret)) = setup.secrets().get(&key).await {
                held.push(secret.expose().to_owned());
            }
        }
    }
    held
}

/// Once the parent writes [`NEEDLES_ASK`] (one word a line): `mock-needles requests=<n> admin_paths=<n> hits=<n>` of
/// the requests the mock (Microsoft's services here) got: how many asked an own server's admin or account path, and
/// how many carry one of the words or an own server's account's refresh token or skypetoken anywhere (URL, headers,
/// body).
pub fn say_mock_needles_when_asked(mock: Arc<ChatServiceMock>, setup: ngteams::setup::Setup, cx: &mut App) {
    let Ok(dir) = own::sandbox_dir() else {
        say("no sandbox dir");
        return;
    };
    let ask: PathBuf = dir.join(NEEDLES_ASK);
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let Ok(words) = fs::read_to_string(&ask) else {
                continue;
            };
            let _ = fs::remove_file(&ask);
            let mut needles: Vec<String> = words
                .lines()
                .filter(|word| !word.is_empty())
                .map(str::to_owned)
                .collect();
            let own = own_secrets(&setup).await;
            let held = own.len();
            needles.extend(own);
            let requests = mock.received_requests().await;
            let admin_paths = requests
                .iter()
                .filter(|request| {
                    let path = request.url.path();
                    path.contains("/api/admin/") || path.contains("/api/account/")
                })
                .count();
            let hits = requests
                .iter()
                .filter(|request| {
                    let body = String::from_utf8_lossy(&request.body);
                    let url = request.url.as_str();
                    needles.iter().any(|needle| {
                        url.contains(needle.as_str())
                            || body.contains(needle.as_str())
                            || request
                                .headers
                                .values()
                                .any(|value| value.to_str().is_ok_and(|value| value.contains(needle.as_str())))
                    })
                })
                .count();
            say(&format!(
                "mock-needles requests={} admin_paths={admin_paths} hits={hits} own_tokens={held}",
                requests.len()
            ));
        }
    })
    .detach();
}
