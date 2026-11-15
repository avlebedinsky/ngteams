//! An own server's account management from the app (`ngteams::own_admin`), against an in-process `ngteams-server`
//! over TLS: a new server asks for its first administrator and gets one with the setup code; the administrator's
//! service lists, adds, changes, resets and deletes accounts and is told why the only administrator stays one; an
//! account that isn't an administrator is refused; a password that must change is replaced before the sign-in; a
//! Microsoft 365 account has no such service. Every value is made up and nothing leaves loopback.

use std::sync::Arc;

use super::own_server::{OwnServer, PASSWORD};
use ngteams::accounts;
use ngteams::accounts_file::{self, PersonalAccount, PersonalPaths};
use ngteams::own_admin::{self, ADMIN_EXISTS, AdminService, NewAdmin, WRONG_SETUP_CODE};
use ngteams::secrets::AppSecrets;
use ngteams::setup::Setup;
use ngteams::signin::PASSWORD_MUST_CHANGE;
use teams_auth::admin::UserChanges;
use teams_auth::authz::{AuthzClient, Region};
use teams_auth::secrets::Secret;
use teams_auth::session::Session;
use teams_auth::token::{EntraConfig, TokenClient};
use teams_ui::user_admin::{AdminProblem, NewAccount, UserAdmin as _};

const ADMIN: &str = "ada@own.example.test";
const DANA: &str = "dana@own.example.test";
const GIVEN: &str = "given horse battery";
const CHOSEN: &str = "chosen staple correct";

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn setup_in(dir: &tempfile::TempDir) -> Setup {
    let paths = PersonalPaths::new(dir.path().join("config/ngteams"), dir.path().join("share/ngteams"));
    Setup::new(paths)
        .unwrap()
        .with_secrets(AppSecrets::in_memory())
        .with_own_servers(true)
}

fn at(server: &OwnServer) -> accounts_file::OwnServer {
    accounts_file::OwnServer::parse(&server.origin, &server.ca_file.display().to_string()).unwrap()
}

/// The runtime the services' sessions run their requests on, as the app's run on the account's engine's.
static SESSIONS: std::sync::LazyLock<tokio::runtime::Runtime> = std::sync::LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap()
});

/// The service of `account`, signed in under `setup`, as the app builds it over the account's session.
fn service(setup: &Setup, account: &PersonalAccount) -> TestResult<AdminService<AppSecrets>> {
    let cloud = account.cloud()?;
    let region = Region::load(&setup.paths.region_file(account).ok_or("the account's region file")?)?;
    let session = Session::new(
        account.id(),
        setup.secrets(),
        TokenClient::for_cloud(&cloud)?,
        AuthzClient::for_cloud(&cloud)?,
        region,
    );
    Ok(AdminService::of(account, Arc::new(session), SESSIONS.handle().clone())
        .ok_or("an own server's account has a service")?)
}

/// The administrator [`ADMIN`] of `server`, signed in under `setup`.
fn admin_signed_in(server: &OwnServer, setup: &Setup) -> TestResult<PersonalAccount> {
    server.add_admin(ADMIN, "Ada Admin")?;
    Ok(accounts::add_own(setup, ADMIN, &at(server), &Secret::new(PASSWORD))?)
}

fn new_account(upn: &str, name: &str, must_change_password: bool) -> NewAccount {
    NewAccount {
        upn: upn.to_owned(),
        display_name: name.to_owned(),
        password: Secret::new(GIVEN),
        admin: false,
        must_change_password,
    }
}

#[test]
fn a_new_server_asks_for_its_first_administrator_and_gets_one() -> TestResult {
    let server = OwnServer::start()?;
    let dir = tempfile::tempdir()?;
    let setup = setup_in(&dir);
    assert_eq!(own_admin::needs_admin(&at(&server))?, Some(true));

    let password = Secret::new(PASSWORD);
    let wrong = Secret::new("AAAAA-AAAAA-AAAAA-AAAAA-AAAAA");
    let form = |code| NewAdmin {
        code,
        upn: ADMIN,
        display_name: "Ada Admin",
        password: &password,
    };
    let refused = own_admin::create_first_admin(&at(&server), form(Some(&wrong))).unwrap_err();
    assert_eq!(
        (refused.words.as_str(), refused.code, refused.admin_exists),
        (WRONG_SETUP_CODE, true, false)
    );
    assert!(server.users()?.is_empty());

    let code = Secret::new(server.setup_code()?);
    own_admin::create_first_admin(&at(&server), form(Some(&code)))?;
    assert_eq!(own_admin::needs_admin(&at(&server))?, None);
    let users = server.users()?;
    assert_eq!(users.len(), 1);
    assert_eq!((users[0].0.as_str(), users[0].2, users[0].3), (ADMIN, true, true));

    let again = own_admin::create_first_admin(&at(&server), form(Some(&code))).unwrap_err();
    assert_eq!(
        (again.words.as_str(), again.code, again.admin_exists),
        (ADMIN_EXISTS, false, true)
    );

    // the account signs in as any other, and its server says it is an administrator
    let signed = accounts::add_own(&setup, ADMIN, &at(&server), &password)?;
    assert!(service(&setup, &signed)?.is_admin()?);
    Ok(())
}

#[test]
fn an_administrator_manages_the_servers_users() -> TestResult {
    let server = OwnServer::start()?;
    let dir = tempfile::tempdir()?;
    let setup = setup_in(&dir);
    let admin = admin_signed_in(&server, &setup)?;
    let service = service(&setup, &admin)?;
    let demoted = service.demoted();
    assert_eq!(service.list("", None)?.users.len(), 1);

    let dana = service.add(&new_account(DANA, "Dana Doe", false))?;
    assert!(dana.enabled && !dana.admin);
    assert_eq!(
        service.add(&new_account(DANA, "Dana Again", false)).unwrap_err(),
        AdminProblem::UpnTaken
    );
    assert_eq!(
        service.add(&new_account("not a user name", "X", false)).unwrap_err(),
        AdminProblem::InvalidUpn
    );
    let found = service.list("dana", None)?;
    assert_eq!(found.users.len(), 1);
    assert_eq!(found.users[0].id, dana.id);
    assert!(found.next.is_none());

    let promote = |admin| UserChanges {
        admin: Some(admin),
        ..UserChanges::default()
    };
    assert!(service.change(&dana.id, &promote(true))?.admin);
    assert!(!service.change(&dana.id, &promote(false))?.admin);
    let own_id = admin.oid.clone().ok_or("the administrator's id")?;
    assert_eq!(
        service.change(&own_id, &promote(false)).unwrap_err(),
        AdminProblem::LastAdmin
    );
    assert_eq!(service.delete(&own_id).unwrap_err(), AdminProblem::LastAdmin);
    // a refusal for the administrator still being one keeps Manage users
    assert!(!*demoted.borrow());
    let renamed = service.change(
        &dana.id,
        &UserChanges {
            display_name: Some("Dana D.".to_owned()),
            enabled: Some(false),
            ..UserChanges::default()
        },
    )?;
    assert_eq!((renamed.display_name.as_str(), renamed.enabled), ("Dana D.", false));

    reset_and_deleted(&service, &dana.id)
}

/// The account `id`'s password is reset, then the account deleted.
fn reset_and_deleted(service: &AdminService<AppSecrets>, id: &str) -> TestResult {
    assert_eq!(
        service.reset_password(id, &Secret::new("short"), true).unwrap_err(),
        AdminProblem::WeakPassword
    );
    service.reset_password(id, &Secret::new(CHOSEN), true)?;
    assert!(service.list("dana", None)?.users[0].must_change_password);

    service.delete(id)?;
    assert_eq!(service.delete(id).unwrap_err(), AdminProblem::NotFound);
    assert!(service.list("dana", None)?.users.is_empty());
    Ok(())
}

#[test]
fn an_account_that_isnt_an_administrator_is_refused() -> TestResult {
    let server = OwnServer::start()?;
    let dir = tempfile::tempdir()?;
    let setup = setup_in(&dir);
    server.add_admin(ADMIN, "Ada Admin")?;
    server.add_user(DANA, "Dana Doe")?;
    let dana = accounts::add_own(&setup, DANA, &at(&server), &Secret::new(PASSWORD))?;
    let service = service(&setup, &dana)?;
    let demoted = service.demoted();
    assert!(!service.is_admin()?);
    // the server said "no administrator" in a plain answer, not a refusal: nothing to take away yet
    assert!(!*demoted.borrow());
    assert_eq!(service.list("", None).unwrap_err(), AdminProblem::NotAdmin);
    // a refusal for not being one takes Manage users away (the app waits on it)
    assert!(*demoted.borrow());
    assert_eq!(
        service
            .add(&new_account("eve@own.example.test", "Eve", false))
            .unwrap_err(),
        AdminProblem::NotAdmin
    );
    assert_eq!(server.users()?.len(), 2);
    Ok(())
}

#[test]
fn a_password_that_must_change_is_replaced_before_the_sign_in() -> TestResult {
    let server = OwnServer::start()?;
    let dir = tempfile::tempdir()?;
    let setup = setup_in(&dir);
    let admin = admin_signed_in(&server, &setup)?;
    service(&setup, &admin)?.add(&new_account(DANA, "Dana Doe", true))?;

    let (given, chosen) = (Secret::new(GIVEN), Secret::new(CHOSEN));
    let refused = accounts::add_own(&setup, DANA, &at(&server), &given).unwrap_err();
    assert_eq!(refused, PASSWORD_MUST_CHANGE);
    // nothing of the account is kept by a sign-in that stored nothing
    assert_eq!(PersonalAccount::load_all(&setup.paths)?.len(), 1);

    let wrong = own_admin::change_password(&at(&server), DANA, &Secret::new("not the one"), &chosen).unwrap_err();
    assert_eq!(wrong, "The user name or password isn't right.");
    let same = own_admin::change_password(&at(&server), DANA, &given, &given).unwrap_err();
    assert_eq!(same, "Choose a password that isn't the current one.");
    let short = own_admin::change_password(&at(&server), DANA, &given, &Secret::new("short")).unwrap_err();
    assert_eq!(short, "Use a password of at least 8 characters.");

    own_admin::change_password(&at(&server), DANA, &given, &chosen)?;
    let signed = accounts::add_own(&setup, DANA, &at(&server), &chosen)?;
    assert_eq!(signed.upn, DANA);
    assert!(!service(&setup, &signed)?.is_admin()?);
    Ok(())
}

#[test]
fn a_server_that_isnt_there_says_so() -> TestResult {
    let server = OwnServer::start()?;
    // the certificate authority's file outlives the server, as a deployment's does
    let dir = tempfile::tempdir()?;
    let ca_file = dir.path().join("ca.pem");
    std::fs::copy(&server.ca_file, &ca_file)?;
    let gone = accounts_file::OwnServer::parse(&server.origin, &ca_file.display().to_string())?;
    drop(server);
    let said = own_admin::needs_admin(&gone).unwrap_err();
    assert!(said.ends_with("can't be reached."), "{said}");
    let password = Secret::new(PASSWORD);
    let failed = own_admin::create_first_admin(
        &gone,
        NewAdmin {
            code: None,
            upn: ADMIN,
            display_name: "Ada Admin",
            password: &password,
        },
    )
    .unwrap_err();
    assert!(failed.words.ends_with("can't be reached.") && !failed.code && !failed.admin_exists);
    let unread = accounts_file::OwnServer::parse(&gone.origin, "/nowhere/ca.pem")?;
    assert!(own_admin::needs_admin(&unread).is_err());
    assert!(own_admin::change_password(&unread, DANA, &password, &password).is_err());
    Ok(())
}

#[test]
fn a_microsoft_365_account_has_no_such_service() -> TestResult {
    let account = PersonalAccount::new(
        "someone@example.test",
        "00000000-0000-4000-8000-0000000000ff",
        "00000000-0000-4000-8000-000000000001",
    )?;
    let session = Session::new(
        account.upn.clone(),
        AppSecrets::in_memory(),
        TokenClient::new(EntraConfig::teams_native().with_authority("http://127.0.0.1:9/"))?,
        AuthzClient::with_endpoint("http://127.0.0.1:9/authz")?,
        Region::default(),
    );
    assert!(AdminService::of(&account, Arc::new(session), SESSIONS.handle().clone()).is_none());
    Ok(())
}
