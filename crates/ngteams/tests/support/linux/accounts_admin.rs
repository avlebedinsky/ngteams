//! An own server's administrator in the accounts window test: the server's first administrator made from the window
//! asking for an account, the Manage users page of that account's settings, and a user who must choose a new
//! password at the first sign-in.
//!
//! **The first administrator**: the parent's own server starts with no account. Beside two Microsoft 365 accounts
//! (the mock's a and b), the rail's "+" and "Own server" ask for the server's address and its certificate authority's
//! file; the app asks the server's setup and the window says the server has no administrator yet, with the form that
//! creates one (setup code, user name, display name, password twice). A wrong setup code says so; the server's own
//! code creates the account, signs it in and puts it on the rail with its chat list.
//!
//! **Manage users** (Settings, for that account alone): the keyboard adds a user, resets their password (to change at
//! the next sign-in), makes them an administrator and takes that away again, is told why the only administrator can't
//! stop being one, and deletes a user after a confirmation. Each step is checked against the server's own list.
//!
//! **A user who must change their password** signs in from the same window: it asks for a new password, changes it on
//! the server and signs the user in. That user is no administrator: their settings have no Manage users page, and
//! neither have the Microsoft 365 accounts'.
//!
//! **More scenes**: Manage users and its dialogs in Teams Dark, the administrator signed in beforehand
//! ([`manage_users_in_dark`]); and the window asking for an account alone in a sandbox as wide as the window itself
//! (480 px), in Teams Light and Dark: the first administrator created from it ([`first_admin_at_480`]), and an
//! account written into `accounts.toml` by hand whose password must change, asked for a new one on the screen that
//! asks its password ([`password_change_at_480`]).
//!
//! The view trace names each step (`user-admin` with `shown`, `listed`, `dialog`, `done`, `failed` and a kind); the
//! app's request log and the mock say where the requests went.

use std::fs;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{App, WindowingModes};
use ngteams::accounts;
use ngteams::accounts_file::{PersonalAccount, PersonalPaths};
use ngteams::own_admin::AdminService;
use ngteams::own_prompt::OwnPrompt;
use ngteams::setup::Setup;
use teams_auth::authz::{AuthzClient, Region};
use teams_auth::secrets::Secret;
use teams_auth::session::Session;
use teams_auth::token::TokenClient;
use teams_mock::{LAB_A, LAB_B};
use teams_ui::account_prompt::{self, OwnOffer};
use teams_ui::user_admin::{NewAccount, UserAdmin as _};
use teams_ui::window::PERSONAL_APP_ID;
use ui_sandbox::input::{Modifier, VirtualInput, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};

use super::{
    MOCK_ASK, OWN_CA_ENV, OWN_NAME, OWN_ORIGIN_ENV, OWN_SEGMENT_AT, OWN_UPN, RAIL_ADD_AT, READY_FRAMES, ROOT_ENV,
    SIGN_IN, TraceReader, WAIT, about, accounts_shown, after_frames, ctl, ensure, field, own, prompt_active,
    prompt_line, prompt_now, quit_and_finish, say, shot, start_with,
};

use teams_ui::account_prompt::{MUST_CHANGE, NO_ADMIN_YET};

/// The administrator's password, typed into the form.
const ADMIN_PASSWORD: &str = "first admin horse";
/// The users the administrator adds.
const DANA: &str = "dana@lab.example.test";
const EVAN: &str = "evan@lab.example.test";
/// The password Dana is given, and the one she chooses.
const GIVEN: &str = "temporary horse 1";
const CHOSEN: &str = "chosen battery 2";

/// Waits for the trace's next `user-admin` line of `change` and `kind`.
pub(super) fn admin_line(trace: &mut TraceReader, running: &Running, change: &str, kind: &str) -> SandboxResult {
    trace
        .next_where(&format!("user-admin {change} {kind}"), |line| {
            line["view"] == "user-admin" && line["change"] == change && line["kinds"] == serde_json::json!([kind])
        })
        .map_err(|error| {
            format!(
                "{error}; the app said {:?}",
                running.line_where(std::time::Duration::ZERO, |_| true)
            )
            .into()
        })
}

/// Waits for the page to list exactly the accounts `upns` (the trace names them by id).
pub(super) fn listed(trace: &mut TraceReader, running: &Running, server: &own::Own, upns: &[&str]) -> SandboxResult {
    let users = server.users()?;
    let mut wanted: Vec<String> = users
        .iter()
        .filter(|(upn, ..)| upns.contains(&upn.as_str()))
        .map(|(_, id, ..)| format!("8:orgid:{id}"))
        .collect();
    wanted.sort();
    ensure(wanted.len() == upns.len(), || {
        format!("the server doesn't hold {upns:?}: {users:?}")
    })?;
    trace
        .next_where(&format!("user-admin listed {upns:?}"), |line| {
            let mut people: Vec<String> = line["people"]
                .as_array()
                .map(|people| people.iter().filter_map(|id| id.as_str().map(str::to_owned)).collect())
                .unwrap_or_default();
            people.sort();
            line["view"] == "user-admin" && line["change"] == "listed" && people == wanted
        })
        .map_err(|error| {
            format!(
                "{error}; the app said {:?}",
                running.line_where(std::time::Duration::ZERO, |_| true)
            )
            .into()
        })
}

/// The server's account `upn`: `(enabled, admin)`.
pub(super) fn server_user(server: &own::Own, upn: &str) -> SandboxResult<Option<(bool, bool)>> {
    Ok(server
        .users()?
        .into_iter()
        .find(|(held, ..)| held == upn)
        .map(|(_, _, enabled, admin)| (enabled, admin)))
}

pub(super) fn tabs(input: &mut VirtualInput, count: usize) -> SandboxResult {
    for _ in 0..count {
        input.key(keys::TAB)?;
    }
    Ok(())
}

/// The rail's "+" and "Own server", the server's address and its certificate authority's file typed: the keyboard
/// leaves on the user name's field (`on_user`) or on "Sign in" past the file's.
fn own_server_typed(
    sandbox: &Sandbox,
    input: &mut VirtualInput,
    running: &Running,
    server: &own::Own,
) -> SandboxResult {
    input.click(RAIL_ADD_AT.0, RAIL_ADD_AT.1, keys::BTN_LEFT)?;
    prompt_active(sandbox, running)?;
    input.click(OWN_SEGMENT_AT.0, OWN_SEGMENT_AT.1, keys::BTN_LEFT)?;
    let chosen = prompt_now(sandbox, running)?;
    ensure(chosen.contains("kind=own"), || {
        format!("\"Own server\" wasn't chosen: {chosen}")
    })?;
    input.type_text(server.origin())?;
    // the user name, the password, then the certificate authority's file
    tabs(input, 3)?;
    input.type_text(&server.ca_file().display().to_string())
}

/// The first run on a server without an administrator: the window offers to create one (see the module docs).
fn first_admin_created(
    sandbox: &Sandbox,
    input: &mut VirtualInput,
    trace: &mut TraceReader,
    running: &Running,
    server: &own::Own,
) -> SandboxResult<String> {
    own_server_typed(sandbox, input, running, server)?;
    shot(sandbox, "ngteams-admin-own-with-ca.png")?;
    input.key(keys::TAB)?;
    let offered = prompt_line(running, SIGN_IN, "the server without an administrator", |line| {
        line.contains(NO_ADMIN_YET)
    })?;
    ensure(offered.contains("signing=false"), || {
        format!("the offer to create the administrator: {offered}")
    })?;
    shot(sandbox, "ngteams-admin-first-run.png")?;

    // a setup code that isn't the server's; the form in Tab's order: code, user name, display name, password twice
    input.type_text("AAAAA-AAAAA-AAAAA-AAAAA-AAAAA")?;
    input.key(keys::TAB)?;
    input.type_text(OWN_UPN)?;
    input.key(keys::TAB)?;
    input.type_text(OWN_NAME)?;
    input.key(keys::TAB)?;
    input.type_text(ADMIN_PASSWORD)?;
    input.key(keys::TAB)?;
    input.type_text(ADMIN_PASSWORD)?;
    shot(sandbox, "ngteams-admin-first-run-filled.png")?;
    input.key(keys::ENTER)?;
    prompt_line(running, SIGN_IN, "the wrong setup code's reason", |line| {
        line.contains("problem=The setup code isn't right")
    })?;
    shot(sandbox, "ngteams-admin-first-run-wrong-code.png")?;
    ensure(server.users()?.is_empty(), || {
        "a wrong setup code made an account".to_owned()
    })?;

    // the server's own code: the code's field is empty again and holds the keyboard
    input.type_text(&server.setup_code()?)?;
    input.key(keys::ENTER)?;
    let mri = super::own_account_signed_in(sandbox, trace, running)?;
    ensure(server_user(server, OWN_UPN)? == Some((true, true)), || {
        "the server's first account isn't an enabled administrator".to_owned()
    })?;
    Ok(mri)
}

/// Opens Manage users of the account `account` (as `ngteamsctl` addresses it) and waits for its list.
pub(super) fn manage_users_opened(
    sandbox: &Sandbox,
    trace: &mut TraceReader,
    running: &Running,
    (server, account): (&own::Own, &str),
) -> SandboxResult {
    // the page is offered once the server said the account is an administrator
    let start = std::time::Instant::now();
    while !offers_manage_users(sandbox, account) {
        ensure(start.elapsed() < WAIT, || {
            "the administrator isn't offered Manage users".to_owned()
        })?;
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    // asked again once the account is the one shown: the keyboard is then on its settings' page column
    std::thread::sleep(std::time::Duration::from_millis(500));
    ctl(sandbox, &["--account", account, "open", "settings", "users"])?;
    let all: Vec<String> = server.users()?.into_iter().map(|(upn, ..)| upn).collect();
    let all: Vec<&str> = all.iter().map(String::as_str).collect();
    listed(trace, running, server, &all)
}

/// From the page's search field: types `query`, waits for the list, and opens its first row's dialog (Tab past
/// "Add user" to the list, Enter).
pub(super) fn first_found_opened(
    input: &mut VirtualInput,
    trace: &mut TraceReader,
    running: &Running,
    (server, query, upn): (&own::Own, &str, &str),
) -> SandboxResult {
    input.type_text(query)?;
    listed(trace, running, server, &[upn])?;
    tabs(input, 2)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "dialog", "edit")
}

/// The search field emptied of `typed` characters (the keyboard is on it after a dialog closes): the page lists
/// `all` again.
pub(super) fn search_cleared(
    input: &mut VirtualInput,
    trace: &mut TraceReader,
    running: &Running,
    (server, typed, all): (&own::Own, usize, &[&str]),
) -> SandboxResult {
    for _ in 0..typed {
        input.key(keys::BACKSPACE)?;
    }
    listed(trace, running, server, all)
}

/// "Add user" from the search field: the user `upn` named `name` with the password `password`.
fn user_added(
    sandbox: &Sandbox,
    input: &mut VirtualInput,
    trace: &mut TraceReader,
    running: &Running,
    (upn, name, password): (&str, &str, &str),
    (server, all, shot_name): (&own::Own, &[&str], Option<&str>),
) -> SandboxResult {
    input.key(keys::TAB)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "dialog", "add")?;
    input.type_text(upn)?;
    input.key(keys::TAB)?;
    input.type_text(name)?;
    input.key(keys::TAB)?;
    input.type_text(password)?;
    if let Some(name) = shot_name {
        shot(sandbox, name)?;
    }
    input.key(keys::ENTER)?;
    admin_line(trace, running, "done", "add")?;
    // the password given is shown once: Done
    input.key(keys::ENTER)?;
    listed(trace, running, server, all)
}

/// The administrator's work on the page (see the module docs); the keyboard starts on the settings' page column.
fn users_managed(
    sandbox: &Sandbox,
    input: &mut VirtualInput,
    trace: &mut TraceReader,
    running: &Running,
    server: &own::Own,
) -> SandboxResult {
    shot(sandbox, "ngteams-admin-users.png")?;
    // the page column, then the search field
    input.key(keys::TAB)?;
    user_added(
        sandbox,
        input,
        trace,
        running,
        (DANA, "Dana Doe", GIVEN),
        (server, &[OWN_UPN, DANA][..], Some("ngteams-admin-add-user.png")),
    )?;
    ensure(server_user(server, DANA)? == Some((true, false)), || {
        "the user added isn't on the server as an enabled user".to_owned()
    })?;
    let all = [OWN_UPN, DANA, EVAN];
    user_added(
        sandbox,
        input,
        trace,
        running,
        (EVAN, "Evan Roe", GIVEN),
        (server, &all[..], None),
    )?;
    shot(sandbox, "ngteams-admin-users-added.png")?;

    // a user name another account has
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
    shot(sandbox, "ngteams-admin-add-user-taken.png")?;
    input.key(keys::ESC)?;

    roles_and_removal(sandbox, input, trace, running, server)
}

/// The rest of the administrator's work: a password reset, an administrator made and unmade, the only administrator
/// kept, and a user deleted.
fn roles_and_removal(
    sandbox: &Sandbox,
    input: &mut VirtualInput,
    trace: &mut TraceReader,
    running: &Running,
    server: &own::Own,
) -> SandboxResult {
    let all = [OWN_UPN, DANA, EVAN];
    // Dana's password reset: Display name, User name, Can sign in, Administrator, then "Reset password"
    first_found_opened(input, trace, running, (server, "dana", DANA))?;
    shot(sandbox, "ngteams-admin-edit-user.png")?;
    tabs(input, 4)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "dialog", "reset")?;
    input.type_text(GIVEN)?;
    shot(sandbox, "ngteams-admin-reset-password.png")?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "done", "reset")?;
    shot(sandbox, "ngteams-admin-reset-done.png")?;
    input.key(keys::ENTER)?;

    // Dana made an administrator: the tick, then Reset password, Delete user, Cancel, Save
    tabs(input, 2)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "dialog", "edit")?;
    tabs(input, 3)?;
    input.key(keys::SPACE)?;
    tabs(input, 4)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "done", "edit")?;
    listed(trace, running, server, &[DANA])?;
    ensure(server_user(server, DANA)? == Some((true, true)), || {
        "Dana isn't an administrator on the server".to_owned()
    })?;
    shot(sandbox, "ngteams-admin-made-admin.png")?;

    // and no longer one
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

    // the only administrator can't stop being one
    search_cleared(input, trace, running, (server, "dana".len(), &all[..]))?;
    first_found_opened(input, trace, running, (server, "bot-a", OWN_UPN))?;
    tabs(input, 3)?;
    input.key(keys::SPACE)?;
    tabs(input, 4)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "failed", "last-admin")?;
    shot(sandbox, "ngteams-admin-last-admin.png")?;
    ensure(server_user(server, OWN_UPN)? == Some((true, true)), || {
        "the only administrator stopped being one".to_owned()
    })?;
    input.key(keys::ESC)?;

    // Evan deleted: "Delete user" asks first, the keyboard on Cancel
    search_cleared(input, trace, running, (server, "bot-a".len(), &all[..]))?;
    first_found_opened(input, trace, running, (server, "evan", EVAN))?;
    tabs(input, 5)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "dialog", "delete")?;
    shot(sandbox, "ngteams-admin-delete-user.png")?;
    // the keyboard starts on Cancel: Delete is next
    tabs(input, 1)?;
    input.key(keys::ENTER)?;
    admin_line(trace, running, "done", "delete")?;
    listed(trace, running, server, &[])?;
    ensure(server_user(server, EVAN)?.is_none(), || {
        "Evan is still on the server".to_owned()
    })?;
    search_cleared(input, trace, running, (server, "evan".len(), &[OWN_UPN, DANA][..]))?;
    shot(sandbox, "ngteams-admin-users-after.png")
}

/// Dana signs in with the password she was given: the window asks for a new one, changes it and signs her in.
fn a_user_must_change_the_password(
    sandbox: &Sandbox,
    input: &mut VirtualInput,
    trace: &mut TraceReader,
    running: &Running,
    server: &own::Own,
) -> SandboxResult {
    input.click(RAIL_ADD_AT.0, RAIL_ADD_AT.1, keys::BTN_LEFT)?;
    prompt_active(sandbox, running)?;
    input.click(OWN_SEGMENT_AT.0, OWN_SEGMENT_AT.1, keys::BTN_LEFT)?;
    input.type_text(server.origin())?;
    input.key(keys::TAB)?;
    input.type_text(DANA)?;
    input.key(keys::TAB)?;
    input.type_text(GIVEN)?;
    input.key(keys::TAB)?;
    input.type_text(&server.ca_file().display().to_string())?;
    input.key(keys::ENTER)?;
    prompt_line(running, SIGN_IN, "the password that must change", |line| {
        line.contains(MUST_CHANGE)
    })?;
    shot(sandbox, "ngteams-admin-must-change.png")?;

    // two passwords that differ
    input.type_text(CHOSEN)?;
    input.key(keys::TAB)?;
    input.type_text("something else 3")?;
    input.key(keys::ENTER)?;
    prompt_line(running, WAIT, "passwords that differ", |line| {
        line.contains("problem=The passwords don't match")
    })?;
    shot(sandbox, "ngteams-admin-must-change-differ.png")?;
    for _ in 0.."something else 3".len() {
        input.key(keys::BACKSPACE)?;
    }
    input.type_text(CHOSEN)?;
    input.key(keys::ENTER)?;
    trace
        .next_where("Dana's account added", |line| {
            line["view"] == "account" && line["change"] == "added"
        })
        .map_err(|error| format!("Dana wasn't signed in: {error}"))?;
    let closed = prompt_now(sandbox, running)?;
    ensure(closed == "prompt-now closed", || {
        format!("the window asking for an account stayed: {closed}")
    })?;
    shot(sandbox, "ngteams-admin-user-signed-in.png")
}

/// Whether `ngteamsctl` opens Manage users for the account `account`.
pub(super) fn offers_manage_users(sandbox: &Sandbox, account: &str) -> bool {
    ctl(sandbox, &["--account", account, "open", "settings", "users"]).is_ok()
}

/// The admin API's requests went to the own server alone, and the mock (Microsoft's services here) got none of the
/// own accounts' tokens.
fn admin_requests_stay_on_the_server(sandbox: &Sandbox, running: &Running) -> SandboxResult {
    fs::write(sandbox.path(own::REQUESTS_ASK), "")?;
    let requests = running
        .line_where(WAIT, |line| line.starts_with("requests "))
        .map_err(|error| format!("the app's requests weren't said: {error:?}"))?;
    say(&requests);
    let own_paths: Vec<&str> = field(&requests, "own_paths").unwrap_or_default().split(',').collect();
    let asked = ["/api/admin/v1/setup", "/api/admin/v1/users", "/api/account/v1/password"]
        .iter()
        .all(|path| own_paths.contains(path));
    ensure(asked && field(&requests, "elsewhere_auth") == Some("0"), || {
        format!("the admin requests: {requests}")
    })?;
    fs::copy(
        sandbox.path(own::REQUESTS_FILE),
        super::shots_dir()?.join("ngteams-admin-requests.txt"),
    )?;
    let listed = fs::read_to_string(sandbox.path(own::REQUESTS_FILE))?;
    let strayed: Vec<&str> = listed
        .lines()
        .filter(|line| {
            !line.starts_with("own-server ") && (line.contains("/api/admin/") || line.contains("/api/account/"))
        })
        .collect();
    ensure(strayed.is_empty(), || {
        format!("admin requests left the own server: {strayed:?}")
    })?;
    fs::write(sandbox.path(MOCK_ASK), "")?;
    let mock = running
        .line_where(WAIT, |line| line.starts_with("mock-requests "))
        .map_err(|error| format!("the mock's requests weren't said: {error:?}"))?;
    say(&mock);
    ensure(
        field(&mock, "auth_paths") == Some("0") && field(&mock, "own_token") == Some("0"),
        || format!("the mock's requests: {mock}"),
    )
}

/// The whole of it (see the module docs).
pub fn first_admin_and_manage_users(sandbox: &Sandbox) -> SandboxResult {
    let server = own::Own::start()?;
    let root = tempfile::tempdir()?;
    let kept = [
        (ROOT_ENV, root.path().display().to_string()),
        (OWN_ORIGIN_ENV, server.origin().to_owned()),
        (OWN_CA_ENV, server.ca_file().display().to_string()),
    ];
    let (mut running, mut trace, _) = start_with(sandbox, "admin", &kept)?;
    accounts_shown(&mut trace, &mut running, &[LAB_A, LAB_B])?;
    let mut input = sandbox.input()?;
    let admin = format!("{}/{OWN_UPN}", server.origin());

    let mri = first_admin_created(sandbox, &mut input, &mut trace, &running, &server)?;
    ensure(!offers_manage_users(sandbox, LAB_A), || {
        "a Microsoft 365 account is offered Manage users".to_owned()
    })?;
    manage_users_opened(sandbox, &mut trace, &running, (&server, admin.as_str()))?;
    users_managed(sandbox, &mut input, &mut trace, &running, &server)?;

    a_user_must_change_the_password(sandbox, &mut input, &mut trace, &running, &server)?;
    let dana = format!("{}/{DANA}", server.origin());
    ensure(!offers_manage_users(sandbox, &dana), || {
        "a user who is no administrator is offered Manage users".to_owned()
    })?;
    ctl(sandbox, &["--account", dana.as_str(), "open", "settings", "general"])?;
    shot(sandbox, "ngteams-admin-user-settings.png")?;
    ensure(!offers_manage_users(sandbox, LAB_B), || {
        "a Microsoft 365 account is offered Manage users".to_owned()
    })?;
    ensure(offers_manage_users(sandbox, &admin), || {
        "the administrator is no longer offered Manage users".to_owned()
    })?;
    trace.wait_where("the administrator's account", |line| {
        about(line, "account", "added", &mri)
    })?;
    admin_requests_stay_on_the_server(sandbox, &running)?;
    quit_and_finish(sandbox, &mut trace, running)
}

// --- Teams Dark, the administrator signed in beforehand ---

/// The parent's own server's administrator signed in as the app signs one in, and started (the child of
/// [`manage_users_in_dark`]).
pub fn signed_in(
    runner: &accounts::Runner,
    server: &ngteams::accounts_file::OwnServer,
) -> SandboxResult<accounts::Started> {
    let account = accounts::add_own(&runner.setup, OWN_UPN, server, &Secret::new(own::PASSWORD))?;
    Ok(runner.start(&account).map_err(String::from)?)
}

/// Manage users in Teams Dark: the page, "Add a user", a user's dialog and its password reset.
pub fn manage_users_in_dark(sandbox: &Sandbox) -> SandboxResult {
    let server = own::Own::start()?;
    server.add_admin(OWN_UPN, OWN_NAME)?;
    server.add_user(DANA, "Dana Doe")?;
    let root = tempfile::tempdir()?;
    let kept = [
        (ROOT_ENV, root.path().display().to_string()),
        (OWN_ORIGIN_ENV, server.origin().to_owned()),
        (OWN_CA_ENV, server.ca_file().display().to_string()),
    ];
    let (mut running, mut trace, _) = start_with(sandbox, "admin-dark", &kept)?;
    accounts_shown(&mut trace, &mut running, &[LAB_A, LAB_B])?;
    let admin = format!("{}/{OWN_UPN}", server.origin());
    // a click in the conversation pane first: the sandbox's keyboard goes to the window the pointer was last in
    let mut input = sandbox.input()?;
    input.click(700, 400, keys::BTN_LEFT)?;
    manage_users_opened(sandbox, &mut trace, &running, (&server, admin.as_str()))?;
    shot(sandbox, "ngteams-admin-users-dark.png")?;
    tabs(&mut input, 2)?;
    input.key(keys::ENTER)?;
    admin_line(&mut trace, &running, "dialog", "add")?;
    shot(sandbox, "ngteams-admin-add-user-dark.png")?;
    input.key(keys::ESC)?;
    first_found_opened(&mut input, &mut trace, &running, (&server, "dana", DANA))?;
    shot(sandbox, "ngteams-admin-edit-user-dark.png")?;
    tabs(&mut input, 4)?;
    input.key(keys::ENTER)?;
    admin_line(&mut trace, &running, "dialog", "reset")?;
    shot(sandbox, "ngteams-admin-reset-password-dark.png")?;
    input.key(keys::ESC)?;
    quit_and_finish(sandbox, &mut trace, running)
}

// --- the window asking for an account alone, at its own width ---

/// A sandbox as wide as the window asking for an account; `None` without the sandbox's tools.
pub(super) fn narrow() -> SandboxResult<Option<Sandbox>> {
    let options = SandboxOptions {
        width: 480,
        height: 800,
        niri_extra: NIRI_FULL_WINDOW,
        fill_output: true,
        ..SandboxOptions::default()
    };
    sandbox_or_skip("ngteams admin prompt at 480 px", &options)
}

/// What the child said after `name ` before it was ready.
fn said_value(said: &[String], name: &str) -> SandboxResult<String> {
    Ok(said
        .iter()
        .find_map(|line| line.strip_prefix(name)?.strip_prefix(' '))
        .ok_or_else(|| format!("the child didn't say {name}: {said:?}"))?
        .to_owned())
}

/// The first administrator created from the window alone, 480 px wide, with the keyboard alone.
pub fn first_admin_at_480(dark: bool) -> SandboxResult {
    let Some(sandbox) = narrow()? else {
        return Ok(());
    };
    let (mode, suffix) = if dark { ("setup-dark", "-dark") } else { ("setup", "") };
    let (running, _trace, said) = start_with(&sandbox, mode, &[])?;
    let mut input = sandbox.input()?;
    prompt_active(&sandbox, &running)?;
    // Shift+Tab to the choice, Right chooses "Own server", Enter goes on to the address
    input.key_with(&[Modifier::Shift], keys::TAB)?;
    input.key(keys::RIGHT)?;
    input.key(keys::ENTER)?;
    input.type_text(&said_value(&said, "setup-origin")?)?;
    tabs(&mut input, 3)?;
    input.type_text(&said_value(&said, "setup-ca")?)?;
    shot(&sandbox, &format!("ngteams-admin-own-ca-480{suffix}.png"))?;
    input.key(keys::TAB)?;
    let offered = prompt_line(&running, SIGN_IN, "the server without an administrator", |line| {
        line.contains(" stage=setup ")
    })?;
    ensure(offered.contains(NO_ADMIN_YET), || format!("the offer: {offered}"))?;
    shot(&sandbox, &format!("ngteams-admin-first-run-480{suffix}.png"))?;
    input.type_text(&said_value(&said, "setup-code")?)?;
    for typed in [OWN_UPN, OWN_NAME, ADMIN_PASSWORD, ADMIN_PASSWORD] {
        input.key(keys::TAB)?;
        input.type_text(typed)?;
    }
    shot(&sandbox, &format!("ngteams-admin-first-run-filled-480{suffix}.png"))?;
    input.key(keys::ENTER)?;
    let signed = running
        .line_where(SIGN_IN, |line| line.starts_with("setup-signed-in "))
        .map_err(|error| format!("the first administrator wasn't signed in: {error:?}"))?;
    ensure(signed == "setup-signed-in admin=true ready=true", || {
        format!("the first administrator: {signed}")
    })?;
    let (status, stderr) = running.finish(WAIT)?;
    ensure(status.success(), || format!("the app exited {status}: {stderr}"))
}

/// An account written by hand whose password must change: its screen asks the password, then a new one twice.
pub fn password_change_at_480(dark: bool) -> SandboxResult {
    let Some(sandbox) = narrow()? else {
        return Ok(());
    };
    let (mode, suffix) = if dark { ("change-dark", "-dark") } else { ("change", "") };
    let (running, _trace, _) = start_with(&sandbox, mode, &[])?;
    let asked = prompt_active(&sandbox, &running)?;
    ensure(
        asked.contains(" stage=sign-in ") && asked.contains(&format!(" user={DANA} ")),
        || format!("the screen doesn't ask Dana's password: {asked}"),
    )?;
    let mut input = sandbox.input()?;
    input.type_text(GIVEN)?;
    input.key(keys::ENTER)?;
    let must = prompt_line(&running, SIGN_IN, "the password that must change", |line| {
        line.contains(" stage=new-password ")
    })?;
    ensure(must.contains(MUST_CHANGE), || {
        format!("the new password's form: {must}")
    })?;
    shot(&sandbox, &format!("ngteams-admin-must-change-480{suffix}.png"))?;
    input.type_text(CHOSEN)?;
    input.key(keys::TAB)?;
    input.type_text(CHOSEN)?;
    input.key(keys::ENTER)?;
    let signed = running
        .line_where(SIGN_IN, |line| line.starts_with("change-signed-in "))
        .map_err(|error| format!("the new password didn't sign in: {error:?}"))?;
    ensure(signed == "change-signed-in ready=true", || {
        format!("signed in with the new password: {signed}")
    })?;
    let (status, stderr) = running.finish(WAIT)?;
    ensure(status.success(), || format!("the app exited {status}: {stderr}"))
}

/// The app's files in `dir`, own servers on, a credential store of this process alone.
fn setup_in(dir: &tempfile::TempDir) -> SandboxResult<Setup> {
    let paths = PersonalPaths::new(dir.path().join("ngteams-config"), dir.path().join("ngteams-data"));
    Ok(Setup::new(paths)?
        .with_secrets(ngteams::secrets::AppSecrets::in_memory())
        .with_own_servers(true))
}

/// The window asking for an account as `offer` says, its own-server asks done as the app does them (`handler`).
fn run_prompt(offer: OwnOffer, handler: OwnPrompt, is_dark: bool) {
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        if is_dark {
            teams_ui::theme::set_appearance(&own::dark(), cx);
        }
        let title = ngteams::app::TITLE.into();
        match account_prompt::open_offering(cx, PERSONAL_APP_ID, title, None, &offer) {
            Ok((window, prompt)) => {
                cx.subscribe(&prompt, move |prompt, event, cx| {
                    handler.handle(event, prompt, cx);
                })
                .detach();
                own::say_prompt(cx);
                let _ = window.update(cx, |_, window, _| {
                    after_frames(window, READY_FRAMES, |_, _| say("ready"));
                });
            }
            Err(error) => {
                say(&format!("the screen can't be opened: {error:#}"));
                cx.quit();
            }
        }
    });
}

/// The child of [`first_admin_at_480`]: a server without any account, and the window asking for an account as the
/// first run opens it. Says the server's address, its certificate authority's file and its setup code, then
/// `setup-signed-in admin=<bool> ready=<bool>` once the form's account is signed in, and quits.
pub fn child_setup(is_dark: bool) -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let dir = tempfile::tempdir()?;
    let setup = setup_in(&dir)?;
    let server = Rc::new(own::Own::start()?);
    say(&format!("setup-origin {}", server.origin()));
    say(&format!("setup-ca {}", server.ca_file().display()));
    say(&format!("setup-code {}", server.setup_code()?));
    let (held, asked) = (setup.clone(), Rc::clone(&server));
    let handler = OwnPrompt {
        setup,
        sign_ins: ngteams::own_prompt::SignIns::app(),
        again: None,
        signed: Rc::new(move |account, _, cx| {
            let admin = asked.users().is_ok_and(|users| {
                users
                    .iter()
                    .any(|(upn, _, enabled, admin)| upn == OWN_UPN && *enabled && *admin)
            });
            let ready = accounts::ready(&held, &account).unwrap_or(false);
            say(&format!("setup-signed-in admin={admin} ready={ready}"));
            cx.quit();
        }),
    };
    run_prompt(OwnOffer::Choice, handler, is_dark);
    drop(server);
    Ok(())
}

/// The child of [`password_change_at_480`]: a server whose administrator added Dana with a password to change at
/// the first sign-in (as the Manage users page adds one), and Dana's entry written into `accounts.toml` by hand with
/// the server's certificate authority. Her screen asks her password; says `change-signed-in ready=<bool>` once she
/// is signed in with the new one, and quits.
pub fn child_change(is_dark: bool) -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let dir = tempfile::tempdir()?;
    let setup = setup_in(&dir)?;
    let server = own::Own::start()?;
    server.add_admin(OWN_UPN, OWN_NAME)?;
    let entry = server.entry()?;
    let admin = accounts::add_own(&setup, OWN_UPN, &entry, &Secret::new(own::PASSWORD))?;
    let cloud = admin.cloud()?;
    let region = Region::load(&setup.paths.region_file(&admin).ok_or("the administrator's region")?)?;
    let session = Session::new(
        admin.id(),
        setup.secrets(),
        TokenClient::for_cloud(&cloud)?,
        AuthzClient::for_cloud(&cloud)?,
        region,
    );
    // the session's requests on a runtime of their own, as the app's run on the account's engine's
    let sessions = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()?;
    let service =
        AdminService::of(&admin, Arc::new(session), sessions.handle().clone()).ok_or("the administrator's service")?;
    let dana = NewAccount {
        upn: DANA.to_owned(),
        display_name: "Dana Doe".to_owned(),
        password: Secret::new(GIVEN),
        admin: false,
        must_change_password: true,
    };
    service.add(&dana).map_err(|problem| problem.words().to_string())?;
    let dana = PersonalAccount::pending_on(DANA, &entry)?;
    dana.save(&setup.paths)?;
    let offer = OwnOffer::First {
        server: server.origin().to_owned(),
        user: DANA.to_owned(),
    };
    let held = setup.clone();
    let handler = OwnPrompt {
        setup,
        sign_ins: ngteams::own_prompt::SignIns::app(),
        again: Some(dana),
        signed: Rc::new(move |account, _, cx| {
            let ready = accounts::ready(&held, &account).unwrap_or(false);
            say(&format!("change-signed-in ready={ready}"));
            cx.quit();
        }),
    };
    run_prompt(offer, handler, is_dark);
    drop(server);
    Ok(())
}
