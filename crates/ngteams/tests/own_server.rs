//! An own server's account, signed in and kept beside a Microsoft 365 one: the password grant against an in-process
//! `ngteams-server` over TLS (its certificate authority in the account's `ca_file`), the refresh token and skypetoken
//! stored under the account's server and UPN together, its region saved from the server's authz, its entry in
//! `accounts.toml` naming the server; what a wrong password, an untrusted certificate and own servers switched off
//! leave behind; the forget. And the server's account management as the app asks it (`support/own_admin.rs`), and a
//! password that must change replaced from the sign-in screen (`support/own_prompt.rs`). Every value is made up and
//! nothing leaves loopback.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]
#![cfg(target_os = "linux")]

#[path = "support/own_admin.rs"]
mod own_admin;
#[path = "support/own_prompt.rs"]
mod own_prompt;
#[path = "support/own_server.rs"]
mod own_server;

use ngteams::accounts_file::{self, PersonalAccount, PersonalPaths};
use ngteams::engine::{self, StartError};
use ngteams::forget::{self, Forgot};
use ngteams::setup::Setup;
use ngteams::signin::{self, SignInError};
use own_server::{OwnServer, PASSWORD};
use teams_auth::authz::{Region, skypetoken_expiry_key, skypetoken_key};
use teams_auth::refresh::refresh_token_key;
use teams_auth::secrets::{MemorySecretStore, Secret, SecretStore as _};
use teams_core::CallFloor;
use teams_core::account::AccountError;

const UPN: &str = "someone@own.example.test";
const NAME: &str = "Made-Up Person";
const MICROSOFT_OID: &str = "00000000-0000-4000-8000-0000000000ff";
const TENANT: &str = "00000000-0000-4000-8000-000000000001";

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn setup_in(dir: &tempfile::TempDir) -> Setup {
    let paths = PersonalPaths::new(dir.path().join("config/ngteams"), dir.path().join("share/ngteams"));
    Setup::new(paths).unwrap().with_own_servers(true)
}

fn at(server: &OwnServer, trusted: bool) -> accounts_file::OwnServer {
    let ca_file = if trusted {
        server.ca_file.display().to_string()
    } else {
        String::new()
    };
    accounts_file::OwnServer::parse(&server.origin, &ca_file).unwrap()
}

fn block_on<T>(future: impl Future<Output = T>) -> T {
    ngteams::accounts::runtime().unwrap().block_on(future)
}

#[test]
fn an_own_servers_account_signs_in_with_its_password_beside_the_microsoft_one() -> TestResult {
    let server = OwnServer::start()?;
    let oid = server.add_user(UPN, NAME)?;
    let dir = tempfile::tempdir()?;
    let setup = setup_in(&dir);
    let store = MemorySecretStore::new();
    // a Microsoft 365 account of the same UPN, signed in before: its entry and its token stay its own
    PersonalAccount::new(UPN, MICROSOFT_OID, TENANT)?.save(&setup.paths)?;
    block_on(store.put(&refresh_token_key(UPN), &Secret::new("microsoft-refresh")))?;

    let password = Secret::new(PASSWORD);
    let signed = block_on(signin::add_own(&setup, UPN, &at(&server, true), &password, &store))?;
    assert_eq!(signed.oid.as_deref(), Some(oid.as_str()));
    assert_eq!(signed.name.as_deref(), Some(NAME));
    assert_eq!(signed.server.as_deref(), Some(server.origin.as_str()));
    assert_eq!(signed.ca_file.as_deref(), Some(server.ca_file.as_path()));
    assert_eq!(signed.id(), format!("{}/{UPN}", server.origin));

    // the entry beside the Microsoft one, which is as it was
    let accounts = PersonalAccount::load_all(&setup.paths)?;
    assert_eq!(accounts.len(), 2);
    assert_eq!(
        (accounts[0].server.as_deref(), accounts[0].oid.as_deref()),
        (None, Some(MICROSOFT_OID))
    );
    assert_eq!(accounts[1], signed);
    kept_under_its_server(&setup, &server, &signed, &store)?;

    signed_in_again_and_forgotten(&setup, &signed, &store)
}

/// A second sign-in takes the same entry, Microsoft's window never opens for the account, and forgetting it leaves
/// the Microsoft 365 account of the same UPN whole.
fn signed_in_again_and_forgotten(setup: &Setup, signed: &PersonalAccount, store: &MemorySecretStore) -> TestResult {
    let password = Secret::new(PASSWORD);
    // a second sign-in (signed out, say) must name the same account, and takes the same entry
    let again = block_on(signin::sign_in_own(setup, signed, &password, store))?;
    assert_eq!(&again, signed);
    // Microsoft's window is never opened for it
    let clients = signin::SignInClients::live()?;
    let refused = block_on(signin::sign_in(
        setup,
        signed,
        &clients,
        store,
        Box::new(|_| panic!("no sign-in window opens for an own server's account")),
    ));
    assert!(matches!(refused, Err(SignInError::WrongCloud)), "{refused:?}");

    // forgotten: its tokens, its region and its entry go; the Microsoft account stays whole
    let forgot = block_on(forget::forget(setup, &signed.id(), store))?;
    assert!(matches!(forgot, Forgot::Account { .. }));
    assert!(block_on(store.get(&refresh_token_key(&signed.id())))?.is_none());
    assert!(block_on(store.get(&refresh_token_key(UPN)))?.is_some());
    assert_eq!(PersonalAccount::load_all(&setup.paths)?.len(), 1);
    Ok(())
}

/// The signed-in account's tokens and region are its own server's, and its engine reads them.
fn kept_under_its_server(
    setup: &Setup,
    server: &OwnServer,
    signed: &PersonalAccount,
    store: &MemorySecretStore,
) -> TestResult {
    // its tokens under its server and UPN together, the Microsoft account's untouched
    let microsoft = block_on(store.get(&refresh_token_key(UPN)))?.ok_or("the Microsoft token")?;
    assert_eq!(microsoft.expose(), "microsoft-refresh");
    let own = block_on(store.get(&refresh_token_key(&signed.id())))?.ok_or("the own server's token")?;
    assert_ne!(own.expose(), "microsoft-refresh");
    assert!(block_on(store.get(&skypetoken_key(&signed.id())))?.is_some());
    assert!(block_on(signin::ready(setup, signed, store))?);
    // its region is the server's own: every base on its origin
    let region = Region::load(&setup.paths.region_file(signed).ok_or("the region file")?)?;
    let auth = region.base("authService").ok_or("the auth base")?;
    assert!(auth.starts_with(&server.origin), "{auth}");

    // the engine's own handle on the account's tokens
    let engine_store = MemorySecretStore::new();
    for key in [
        refresh_token_key(&signed.id()),
        skypetoken_key(&signed.id()),
        skypetoken_expiry_key(&signed.id()),
    ] {
        if let Some(secret) = block_on(store.get(&key))? {
            block_on(engine_store.put(&key, &secret))?;
        }
    }
    engine_starts_or_names_the_missing_chat_service(setup, signed, &region, engine_store);
    Ok(())
}

/// The account's engine against the server: started when the server's authz names a chat service; until the server
/// serves one, the start names exactly that, having read the account's own region and cloud.
fn engine_starts_or_names_the_missing_chat_service(
    setup: &Setup,
    account: &PersonalAccount,
    region: &Region,
    store: MemorySecretStore,
) {
    let started = engine::start(&setup.paths, account, CallFloor::new(), store, false, None);
    if region.base("chatServiceAfd").is_some() || region.base("chatService").is_some() {
        match started {
            Ok(engine) => engine.handle.shutdown(),
            Err(error) => panic!("the engine doesn't start on the server's chat service: {error}"),
        }
    } else {
        assert!(
            matches!(started, Err(StartError::Account(AccountError::NoChatBase))),
            "{:?}",
            started.err()
        );
    }
}

/// A server whose certificate the certificate authority typed doesn't vouch for is said untrusted, not unreachable:
/// the file typed is named, and the form's field is where to fix it.
#[test]
fn an_unreachable_server_with_a_ca_file_names_that_file() -> TestResult {
    let server = OwnServer::start()?;
    server.add_user(UPN, NAME)?;
    let dir = tempfile::tempdir()?;
    let setup = &setup_in(&dir);
    let store = MemorySecretStore::new();
    let other = OwnServer::start()?;
    let wrong_ca = accounts_file::OwnServer::parse(&server.origin, &other.ca_file.display().to_string())?;
    let said = block_on(signin::add_own(setup, UPN, &wrong_ca, &Secret::new(PASSWORD), &store)).unwrap_err();
    assert!(
        said.contains("certificate isn't trusted")
            && said.contains(&other.ca_file.display().to_string())
            && said.contains("Certificate authority file"),
        "{said}"
    );
    assert!(
        !said.contains("can't be reached") && !said.contains(signin::CA_FILE_EXAMPLE),
        "{said}"
    );
    for account in PersonalAccount::load_all(&setup.paths)? {
        PersonalAccount::remove(&setup.paths, &account.id())?;
    }
    Ok(())
}

/// The sign-in screen holds the entry as it was read when it opened: a `ca_file` the user writes into the entry
/// meanwhile is used by the next "Sign in", with no restart.
#[test]
fn a_ca_file_written_while_the_screen_is_open_is_used_by_the_next_sign_in() -> TestResult {
    let server = OwnServer::start()?;
    server.add_user(UPN, NAME)?;
    let dir = tempfile::tempdir()?;
    let setup = setup_in(&dir);
    let store = MemorySecretStore::new();
    let password = Secret::new(PASSWORD);
    // an entry written by hand without the server's certificate authority, as the screen read it
    let shown = PersonalAccount::pending_on(UPN, &at(&server, false))?;
    shown.save(&setup.paths)?;
    let said = block_on(signin::sign_in_own_again(&setup, &shown, &password, &store)).unwrap_err();
    // the screen asks the password alone: where the certificate authority goes is the entry
    assert!(
        said.contains("certificate isn't trusted") && said.contains(signin::CA_FILE_EXAMPLE),
        "{said}"
    );

    let written = PersonalAccount {
        ca_file: Some(server.ca_file.clone()),
        ..shown.clone()
    };
    written.save(&setup.paths)?;
    let signed = block_on(signin::sign_in_own_again(&setup, &shown, &password, &store))?;
    assert!(signed.is_signed_in());
    assert_eq!(signed.ca_file.as_deref(), Some(server.ca_file.as_path()));
    assert_eq!(PersonalAccount::load_all(&setup.paths)?, std::slice::from_ref(&signed));

    // a file that isn't the server's certificate authority, written the same way, is the one named
    let other = OwnServer::start()?;
    PersonalAccount {
        ca_file: Some(other.ca_file.clone()),
        ..signed
    }
    .save(&setup.paths)?;
    let said = block_on(signin::sign_in_own_again(&setup, &shown, &password, &store)).unwrap_err();
    assert!(said.contains(&other.ca_file.display().to_string()), "{said}");
    Ok(())
}

/// An account signed in while its server's authz named no chat service: once the server names one, asking its
/// authz again with the stored session saves a region the engine starts on, and no password is typed.
#[test]
fn a_region_without_a_chat_service_is_asked_again_without_a_password() -> TestResult {
    const NO_CHATS: &str = r#"{"_synthetic":true,"region":"own","partition":null,"regionGtms":{}}"#;
    let server = OwnServer::start()?;
    server.add_user(UPN, NAME)?;
    let dir = tempfile::tempdir()?;
    let setup = setup_in(&dir);
    let store = MemorySecretStore::new();
    let signed = block_on(signin::add_own(
        &setup,
        UPN,
        &at(&server, true),
        &Secret::new(PASSWORD),
        &store,
    ))?;
    let region = setup.paths.region_file(&signed).ok_or("the region file")?;
    std::fs::write(&region, NO_CHATS)?;
    assert!(ngteams::accounts::unserved(&setup, &signed).is_some());

    // not asked while own servers are off, nor of Microsoft for an account of theirs: the saved region stays
    let off = Setup::new(setup.paths.clone())?;
    let refused = block_on(signin::region_again(&off, &signed, &store));
    assert!(matches!(refused, Err(SignInError::OwnServersOff)), "{refused:?}");
    let microsoft = PersonalAccount::new(UPN, MICROSOFT_OID, TENANT)?;
    let refused = block_on(signin::region_again(&setup, &microsoft, &store));
    assert!(matches!(refused, Err(SignInError::WrongCloud)), "{refused:?}");
    assert_eq!(std::fs::read_to_string(&region)?, NO_CHATS);

    block_on(signin::region_again(&setup, &signed, &store))?;
    assert!(ngteams::accounts::unserved(&setup, &signed).is_none());
    let saved = Region::load(&region)?;
    let chat = saved.base(teams_auth::authz::CHAT_BASE_GTM).ok_or("the chat base")?;
    assert!(chat.starts_with(&server.origin), "{chat}");

    // a session that is no longer there can't ask: the region saved before stays
    std::fs::write(&region, NO_CHATS)?;
    block_on(store.delete(&refresh_token_key(&signed.id())))?;
    assert!(block_on(signin::region_again(&setup, &signed, &store)).is_err());
    assert_eq!(std::fs::read_to_string(&region)?, NO_CHATS);
    Ok(())
}

/// An entry the user wrote by hand with a CA file stays for the next try when its sign-in stores nothing.
fn an_entry_written_by_hand_stays(setup: &Setup, server: &OwnServer, store: &MemorySecretStore) -> TestResult {
    PersonalAccount::pending_on(UPN, &at(server, true))?.save(&setup.paths)?;
    let wrong = block_on(signin::add_own(
        setup,
        UPN,
        &at(server, true),
        &Secret::new("not it"),
        store,
    ));
    assert_eq!(wrong.unwrap_err(), "The user name or password isn't right.");
    let kept = PersonalAccount::load_all(&setup.paths)?;
    assert_eq!(kept.len(), 1, "the entry written by hand holds its CA file");
    assert!(!kept[0].is_signed_in() && kept[0].ca_file.is_some());
    PersonalAccount::remove(&setup.paths, &kept[0].id())?;
    Ok(())
}

#[test]
fn a_sign_in_that_stores_nothing_says_why_and_leaves_only_what_the_user_wrote() -> TestResult {
    let server = OwnServer::start()?;
    server.add_user(UPN, NAME)?;
    let dir = tempfile::tempdir()?;
    let setup = setup_in(&dir);
    let store = MemorySecretStore::new();

    let wrong = block_on(signin::add_own(
        &setup,
        UPN,
        &at(&server, true),
        &Secret::new("not it"),
        &store,
    ));
    assert_eq!(wrong.unwrap_err(), "The user name or password isn't right.");
    let unknown = block_on(signin::add_own(
        &setup,
        "nobody@own.example.test",
        &at(&server, true),
        &Secret::new(PASSWORD),
        &store,
    ));
    assert_eq!(unknown.unwrap_err(), "The user name or password isn't right.");
    // an entry the failed attempt itself made goes with it, the CA file typed for it too; nothing is stored
    assert!(PersonalAccount::load_all(&setup.paths)?.is_empty());
    assert!(block_on(store.get(&refresh_token_key(&format!("{}/{UPN}", server.origin))))?.is_none());

    an_entry_written_by_hand_stays(&setup, &server, &store)?;

    // the server's certificate isn't trusted without its certificate authority: said, with the form's field to fill,
    // and the bare entry is taken out again
    let untrusted = block_on(signin::add_own(
        &setup,
        UPN,
        &at(&server, false),
        &Secret::new(PASSWORD),
        &store,
    ));
    let said = untrusted.unwrap_err();
    assert!(
        said.contains("certificate isn't trusted") && said.contains("Certificate authority file"),
        "{said}"
    );
    assert!(
        !said.contains("can't be reached") && !said.contains("accounts.toml"),
        "{said}"
    );
    assert!(PersonalAccount::load_all(&setup.paths)?.is_empty());

    // own servers switched off: nothing is added, signed in or started
    let off = Setup::new(setup.paths.clone())?;
    let refused = block_on(signin::add_own(
        &off,
        UPN,
        &at(&server, true),
        &Secret::new(PASSWORD),
        &store,
    ));
    assert!(refused.unwrap_err().contains("NGTEAMS_OWN_SERVER"));
    let signed = block_on(signin::add_own(
        &setup,
        UPN,
        &at(&server, true),
        &Secret::new(PASSWORD),
        &store,
    ))?;
    assert!(
        off.signed_in_accounts()?.is_empty(),
        "an own server's account isn't started while they are off"
    );
    assert_eq!(setup.signed_in_accounts()?, std::slice::from_ref(&signed));
    // said plainly, never as a sign-in that "stored nothing"
    let again = block_on(signin::sign_in_own(&off, &signed, &Secret::new(PASSWORD), &store)).unwrap_err();
    assert!(matches!(again, SignInError::OwnServersOff), "{again:?}");
    assert_eq!(signin::own_failure(&off, &signed, &again), signin::OWN_SERVERS_OFF);
    Ok(())
}
