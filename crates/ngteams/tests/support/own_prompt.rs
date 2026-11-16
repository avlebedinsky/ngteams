//! A password that must change, replaced from the sign-in screen (`ngteams::own_prompt`) on GPUI's test context
//! against an in-process `ngteams-server`: the change goes to the server the account's entry in `accounts.toml` names,
//! its CA file too when none was typed; and a sign-in that fails after the change leaves the screen on its sign-in
//! form, saying the new password is set. Every value is made up and nothing leaves loopback.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Entity, TestAppContext, VisualTestContext};
use ngteams::accounts;
use ngteams::accounts_file::{self, PersonalAccount, PersonalPaths};
use ngteams::own_admin::AdminService;
use ngteams::own_prompt::{OwnPrompt, PASSWORD_CHANGED, SignIns};
use ngteams::secrets::AppSecrets;
use ngteams::setup::Setup;
use teams_auth::authz::{AuthzClient, Region};
use teams_auth::secrets::Secret;
use teams_auth::session::Session;
use teams_auth::token::TokenClient;
use teams_ui::account_prompt::Stage;
use teams_ui::user_admin::{NewAccount, UserAdmin as _};
use teams_ui::{AccountPrompt, theme};

use super::own_server::{OwnServer, PASSWORD};

const ADMIN: &str = "ada@own.example.test";
const DANA: &str = "dana@own.example.test";
const GIVEN: &str = "given horse battery";
const CHOSEN: &str = "chosen staple correct";
/// What the stand-in sign-in says once the password changed.
const BUSY: &str = "The server is busy.";

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// The account a sign-in from the screen handed on.
type Signed = Rc<RefCell<Option<PersonalAccount>>>;

/// A server whose administrator added Dana with [`GIVEN`] to change at the first sign-in, and Dana's entry in
/// `accounts.toml` naming the server and its CA file, as an earlier sign-in that stored nothing more leaves it.
fn dana_must_change(dir: &tempfile::TempDir) -> TestResult<(OwnServer, Setup)> {
    let server = OwnServer::start()?;
    let paths = PersonalPaths::new(dir.path().join("config/ngteams"), dir.path().join("share/ngteams"));
    let setup = Setup::new(paths)?.with_secrets(AppSecrets::in_memory());
    let entry = accounts_file::OwnServer::parse(&server.origin, &server.ca_file.display().to_string())?;
    server.add_admin(ADMIN, "Ada Admin")?;
    let admin = accounts::add_own(&setup, ADMIN, &entry, &Secret::new(PASSWORD))?;
    let cloud = admin.cloud()?;
    let region = Region::load(&setup.paths.region_file(&admin).ok_or("the administrator's region")?)?;
    let session = Session::new(
        admin.id(),
        setup.secrets(),
        TokenClient::for_cloud(&cloud)?,
        AuthzClient::for_cloud(&cloud)?,
        region,
    );
    let sessions = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()?;
    let service = AdminService::of(&admin, std::sync::Arc::new(session), sessions.handle().clone())
        .ok_or("the administrator's service")?;
    let dana = NewAccount {
        upn: DANA.to_owned(),
        display_name: "Dana Doe".to_owned(),
        password: Secret::new(GIVEN),
        admin: false,
        must_change_password: true,
    };
    service.add(&dana).map_err(|problem| problem.words().to_string())?;
    drop(service);
    sessions.shutdown_background();
    PersonalAccount::pending_on(DANA, &entry)?.save(&setup.paths)?;
    Ok((server, setup))
}

/// Types `text` into the input of the field `selector` (its name is above the input, so near its bottom edge).
fn type_into(selector: &'static str, text: &str, cx: &mut VisualTestContext) {
    let bounds = cx.debug_bounds(selector).unwrap_or_else(|| panic!("no {selector}"));
    let at = gpui::point(bounds.center().x, bounds.bottom() - gpui::px(10.0));
    cx.simulate_click(at, gpui::Modifiers::none());
    cx.simulate_input(text);
    // a field hears that it lost the keyboard when the next frame is drawn
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
}

/// The screen adding an account, its own-server asks done by `sign_ins` as the app does them: Dana signs in on the
/// server with [`GIVEN`] and no CA file typed, is asked for a new password and gives [`CHOSEN`].
fn changed_from_the_screen(
    setup: &Setup,
    sign_ins: SignIns,
    server: &OwnServer,
    cx: &mut TestAppContext,
) -> (Entity<AccountPrompt>, Signed, VisualTestContext) {
    cx.update(theme::init);
    let window = cx.add_window(|window, cx| AccountPrompt::new(None, window, cx));
    let prompt = window.root(cx).unwrap();
    let signed = Signed::default();
    let handed = Rc::clone(&signed);
    let handler = OwnPrompt {
        setup: setup.clone(),
        sign_ins,
        again: None,
        signed: Rc::new(move |account, _, _| {
            handed.replace(Some(account));
        }),
    };
    cx.update(|cx| {
        cx.subscribe(&prompt, move |prompt, event, cx| {
            handler.handle(event, prompt, cx);
        })
        .detach();
    });
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.update(|window, _| window.activate_window());
    prompt.update(&mut cx, AccountPrompt::offer_own_server);
    cx.run_until_parked();
    let segment = cx.debug_bounds("account-prompt-kind-own").unwrap();
    cx.simulate_click(segment.center(), gpui::Modifiers::none());
    type_into("account-prompt-server", &server.origin, &mut cx);
    type_into("account-prompt-user", DANA, &mut cx);
    type_into("account-prompt-password", GIVEN, &mut cx);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(cx.read(|cx| prompt.read(cx).stage()), Stage::NewPassword);

    cx.simulate_input(CHOSEN);
    type_into("account-prompt-confirm", CHOSEN, &mut cx);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    (prompt, signed, cx)
}

#[gpui::test]
fn the_change_takes_the_ca_file_from_the_accounts_entry(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (server, setup) = dana_must_change(&dir).unwrap();
    let (prompt, signed, cx) = changed_from_the_screen(&setup, SignIns::app(), &server, cx);
    let problem = cx.read(|cx| prompt.read(cx).problem().map(ToString::to_string));
    let account = signed.borrow().clone();
    let account = account.unwrap_or_else(|| panic!("Dana wasn't signed in: {problem:?}"));
    assert_eq!(account.upn, DANA);
    assert_eq!(account.ca_file.as_ref(), Some(&server.ca_file));
}

/// The app's sign-in, but a sign-in with any password other than [`GIVEN`] is answered [`BUSY`].
fn busy_once_changed(
    setup: &Setup,
    upn: &str,
    server: &accounts_file::OwnServer,
    password: &Secret,
) -> Result<PersonalAccount, String> {
    if password.expose() == GIVEN {
        accounts::add_own(setup, upn, server, password)
    } else {
        Err(BUSY.to_owned())
    }
}

#[gpui::test]
fn a_sign_in_that_fails_after_the_change_leaves_the_sign_in_form(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (server, setup) = dana_must_change(&dir).unwrap();
    let sign_ins = SignIns {
        add_own: busy_once_changed,
        ..SignIns::app()
    };
    let (prompt, signed, cx) = changed_from_the_screen(&setup, sign_ins, &server, cx);
    assert!(signed.borrow().is_none());
    let (stage, problem, signing_in) = cx.read(|cx| {
        let prompt = prompt.read(cx);
        (
            prompt.stage(),
            prompt.problem().map(ToString::to_string),
            prompt.is_signing_in(),
        )
    });
    // the next submit is a plain sign-in with what is typed, never the old password again
    assert_eq!(stage, Stage::SignIn);
    assert_eq!(problem, Some(format!("{BUSY} {PASSWORD_CHANGED}")));
    assert!(!signing_in);
    // the change itself went through on the server
    let entry = accounts_file::OwnServer::parse(&server.origin, &server.ca_file.display().to_string()).unwrap();
    let dana = accounts::add_own(&setup, DANA, &entry, &Secret::new(CHOSEN)).unwrap();
    assert_eq!(dana.upn, DANA);
}
