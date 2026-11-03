//! The personal build's sign-in and start: `ngteams` signs in through its sign-in window (a closure standing in
//! for the app's window, answering the redirect), the code redeemed at a local token endpoint and the skypetoken at a
//! local authz (wiremock), the refresh token stored under the account's UPN in the store handed in (the app hands in the
//! desktop's Secret Service under `ngteams`; here a `MemorySecretStore`), the account's `accounts.toml` entry
//! given the object id and tenant, its region saved under `regions/`; a later sign-in landing in another account stores
//! nothing; several accounts side by side (added, abandoned, signed in, forgotten one at a
//! time), an added account whose sign-in stored nothing keeping what the user wrote into its entry (its `idp_hosts`)
//! for the next try, and a host the window refused named with where to add it; a single account's files of an earlier
//! build moved into theirs; the engine opens each account's store under
//! the personal data dir. Every directory is a test's own; every value is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ngteams::KEYRING_APPLICATION;
use ngteams::accounts_file::{PersonalAccount, PersonalError, PersonalPaths};
use ngteams::forget::{self, ForgetError, Forgot};
use ngteams::setup::{Setup, SetupError};
use ngteams::signin::{self, SignInClients, SignInError};
use serde_json::{Value, json};
use teams_auth::authz::{AuthzClient, skypetoken_expiry_key, skypetoken_key};
use teams_auth::refresh::refresh_token_key;
use teams_auth::secrets::{MemorySecretStore, Secret, SecretStore};
use teams_auth::signin::{SignInAnswer, SignInRequest, WindowSignInError};
use teams_auth::token::{EntraConfig, NATIVE_CLIENT_REDIRECT, TEAMS_NATIVE_CLIENT_ID, TokenClient, TokenError};
use teams_mock::CHAT_BASE;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const UPN: &str = "someone@example.test";
const OID: &str = "00000000-0000-4000-8000-0000000000ff";
const TENANT: &str = "00000000-0000-4000-8000-000000000001";
const DISPLAY_NAME: &str = "Made-Up Person";
const OTHER_OID: &str = "00000000-0000-4000-8000-0000000000ee";
const SECOND_UPN: &str = "someone-else@example.test";
const AUTHZ_FIXTURE: &str = include_str!("../../../tests/fixtures/auth/authz-ok.json");

/// Temporary account paths.
fn paths_in(dir: &tempfile::TempDir) -> PersonalPaths {
    PersonalPaths::new(dir.path().join("config/ngteams"), dir.path().join("share/ngteams"))
}
fn setup_in(dir: &tempfile::TempDir) -> Setup {
    Setup::new(paths_in(dir)).unwrap()
}

/// `upn` signed in as `oid` through a token endpoint and authz of its own.
async fn signed_in(setup: &Setup, upn: &str, oid: &str, store: &MemorySecretStore) -> PersonalAccount {
    let server = MockServer::start().await;
    let clients = clients(&server, upn, oid).await;
    let account = setup.ask_for(upn).unwrap();
    signin::sign_in(setup, &account, &clients, store, window())
        .await
        .unwrap()
}

/// `account`'s engine using an in-memory secret store (nothing is sent).
fn start_engine(setup: &Setup, account: &PersonalAccount) -> ngteams::engine::PersonalEngine<MemorySecretStore> {
    ngteams::engine::start(
        &setup.paths,
        account,
        teams_core::CallFloor::new(),
        MemorySecretStore::new(),
        false,
        None,
    )
    .unwrap()
}

/// An unsigned ID token naming `upn`, `oid`, the tenant, for the native client.
fn id_token(upn: &str, oid: &str) -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"RS256","kid":"test"}"#);
    let claims = json!({
        "aud": TEAMS_NATIVE_CLIENT_ID,
        "oid": oid,
        "tid": TENANT,
        "preferred_username": upn,
        "name": DISPLAY_NAME,
    });
    format!(
        "{header}.{}.c2lnbmF0dXJlLW5vdC1jaGVja2Vk",
        URL_SAFE_NO_PAD.encode(claims.to_string())
    )
}

/// A local Entra token endpoint whose answers name `upn` and `oid`, and a local authz whose region's chat base is the
/// mock's; the clients for them.
async fn clients(server: &MockServer, upn: &str, oid: &str) -> SignInClients {
    let token = json!({
        "token_type": "Bearer",
        "scope": "https://api.spaces.skype.com/.default",
        "expires_in": 3600,
        "access_token": "made-up-access-token-0123456789",
        "refresh_token": "made-up-refresh-token",
        "id_token": id_token(upn, oid),
    });
    Mock::given(method("POST"))
        .and(path("/organizations/oauth2/v2.0/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(token))
        .mount(server)
        .await;
    let fixture: Value = serde_json::from_str(AUTHZ_FIXTURE).unwrap();
    let mut authz = fixture["response"]["body"].clone();
    authz["tokens"]["skypeToken"] = json!("made-up-skypetoken-0123456789");
    authz["regionGtms"]["chatServiceAfd"] = json!(CHAT_BASE);
    Mock::given(method("POST"))
        .and(path("/api/authsvc/v1.0/authz"))
        .respond_with(ResponseTemplate::new(200).set_body_json(authz))
        .mount(server)
        .await;
    let entra = EntraConfig::teams_native().with_authority(format!("{}/organizations/", server.uri()));
    SignInClients {
        token: TokenClient::new(entra.clone()).unwrap(),
        authz: AuthzClient::with_endpoint(&format!("{}/api/authsvc/v1.0/authz", server.uri())).unwrap(),
        entra,
    }
}

/// Standing in for the sign-in window: it answers the redirect to the request, its `state` taken from the request's
/// authorize URL.
fn window() -> signin::Window {
    Box::new(|request: SignInRequest| {
        let state = request
            .authorize
            .split(['?', '&'])
            .find_map(|pair| pair.strip_prefix("state="))
            .unwrap_or_default()
            .to_owned();
        SignInAnswer::Redirect(Secret::new(format!(
            "{NATIVE_CLIENT_REDIRECT}?code=made-up-code&state={state}"
        )))
    })
}

#[tokio::test]
async fn the_first_sign_in_stores_the_token_the_account_and_the_region_and_the_engine_opens_its_store_there() {
    let dir = tempfile::tempdir().unwrap();
    let setup = setup_in(&dir);
    let server = MockServer::start().await;
    let clients = clients(&server, UPN, OID).await;
    let store = MemorySecretStore::new();
    let account = setup.ask_for(UPN).unwrap();
    assert!(!account.is_signed_in());
    assert_eq!(setup.paths.region_file(&account), None);
    assert!(!signin::ready(&setup, &account, &store).await.unwrap());

    let signed = signin::sign_in(&setup, &account, &clients, &store, window())
        .await
        .unwrap();
    assert_eq!(signed.oid.as_deref(), Some(OID));
    assert_eq!(signed.tid.as_deref(), Some(TENANT));
    assert_eq!(signed.name.as_deref(), Some(DISPLAY_NAME));
    assert_eq!(ngteams::engine::display_name(&signed), DISPLAY_NAME);
    let stored = store.get(&refresh_token_key(UPN)).await.unwrap();
    assert_eq!(stored.as_ref().map(Secret::expose), Some("made-up-refresh-token"));
    assert_eq!(PersonalAccount::load_all(&setup.paths).unwrap(), vec![signed.clone()]);
    let region_file = setup.paths.region_file(&signed).unwrap();
    assert_eq!(
        region_file,
        setup.paths.data_dir().join("regions").join(format!("{OID}.json"))
    );
    let region = fs::read_to_string(&region_file).unwrap();
    assert!(
        region.contains(CHAT_BASE) && !region.contains("made-up-skypetoken"),
        "{region}"
    );
    assert_eq!(fs::metadata(&region_file).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(
        fs::metadata(region_file.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert!(signin::ready(&setup, &signed, &store).await.unwrap());

    // the engine uses the account store under the personal data dir; nothing is sent
    let engine = start_engine(&setup, &signed);
    let store_file = setup.paths.store_file(&signed).unwrap();
    assert!(store_file.is_file());
    assert_eq!(
        store_file,
        setup.paths.data_dir().join("store").join(format!("{OID}.sqlite"))
    );
    engine.handle.shutdown();
    assert_eq!(KEYRING_APPLICATION, "ngteams");
}

#[tokio::test]
async fn a_later_sign_in_that_lands_in_another_account_stores_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let setup = setup_in(&dir);
    PersonalAccount::new(UPN, OID, TENANT)
        .unwrap()
        .save(&setup.paths)
        .unwrap();
    let account = setup.ask_for(UPN).unwrap();
    assert!(account.is_signed_in());
    let server = MockServer::start().await;
    let clients = clients(&server, UPN, OTHER_OID).await;
    let store = MemorySecretStore::new();
    let refused = signin::sign_in(&setup, &account, &clients, &store, window()).await;
    assert!(
        matches!(
            refused,
            Err(SignInError::Window(WindowSignInError::Token(
                TokenError::WrongAccount { .. }
            )))
        ),
        "{refused:?}"
    );
    assert!(store.get(&refresh_token_key(UPN)).await.unwrap().is_none());
    assert_eq!(PersonalAccount::load_all(&setup.paths).unwrap(), vec![account.clone()]);
    assert!(!setup.paths.data_dir().join("regions").exists());
    // a window closed before the redirect stores nothing either
    assert!(
        signin::sign_in(&setup, &account, &clients, &store, answering(&SignInAnswer::Closed))
            .await
            .is_err()
    );
}

#[test]
fn an_account_given_is_the_one_held_and_another_is_added_after_it() {
    let dir = tempfile::tempdir().unwrap();
    let setup = setup_in(&dir);
    assert!(setup.accounts().unwrap().is_empty());
    let pending = setup.ask_for(UPN).unwrap();
    assert_eq!(pending.upn, UPN);
    assert!(!pending.is_signed_in());
    assert_eq!(
        fs::metadata(setup.paths.accounts_file()).unwrap().permissions().mode() & 0o777,
        0o600
    );
    // the same account again, case aside: the one held, not a second
    assert_eq!(setup.ask_for("SOMEONE@example.test").unwrap(), pending);
    assert_eq!(setup.accounts().unwrap(), vec![pending.clone()]);
    // another one is added after it, the first kept
    let other = setup.ask_for(SECOND_UPN).unwrap();
    assert_eq!(other.upn, SECOND_UPN);
    assert_eq!(setup.accounts().unwrap(), vec![pending.clone(), other.clone()]);
    assert_eq!(
        setup.pending_upns().unwrap(),
        vec![UPN.to_owned(), SECOND_UPN.to_owned()]
    );
    assert!(
        setup.signed_in_accounts().unwrap().is_empty(),
        "no sign-in named either"
    );
    assert_eq!(setup.ask_for(SECOND_UPN).unwrap(), other);
    // a bad UPN is refused, nothing written
    assert!(matches!(
        setup.ask_for("not a upn"),
        Err(SetupError::Personal(PersonalError::Account(_)))
    ));
    assert_eq!(setup.accounts().unwrap().len(), 2);
}

#[tokio::test]
async fn an_abandoned_account_goes_only_while_no_sign_in_named_it() {
    let dir = tempfile::tempdir().unwrap();
    let setup = setup_in(&dir);
    // nothing held: the first run's screen asks, with an empty field
    assert!(setup.signed_in_accounts().unwrap().is_empty());
    assert!(setup.pending_upns().unwrap().is_empty());
    // an account given whose sign-in stored nothing leaves nothing behind
    let typo = setup.ask_for("someone@example.tset").unwrap();
    assert!(!typo.is_signed_in());
    assert_eq!(setup.pending_upns().unwrap(), vec!["someone@example.tset".to_owned()]);
    assert!(setup.abandon("SOMEONE@example.tset").unwrap());
    assert!(setup.accounts().unwrap().is_empty());
    assert!(!setup.abandon("someone@example.tset").unwrap(), "none left to take out");
    // the identity provider's hosts given to a pending account are kept by its sign-in
    let pending = setup.ask_for(UPN).unwrap();
    let mut with_idp = pending.clone();
    with_idp.idp_hosts = vec!["sso.example.test".to_owned()];
    with_idp.save(&setup.paths).unwrap();
    let store = MemorySecretStore::new();
    let signed = signed_in(&setup, UPN, OID, &store).await;
    assert_eq!(signed.idp_hosts, vec!["sso.example.test".to_owned()]);
    assert_eq!(setup.signed_in_accounts().unwrap(), vec![signed.clone()]);
    assert!(setup.pending_upns().unwrap().is_empty());
    // once a sign-in named it, it stays: abandon takes out only a pending one
    assert!(!setup.abandon(UPN).unwrap());
    assert_eq!(setup.ask_for("SOMEONE@example.test").unwrap(), signed);
    let added = setup.ask_for(SECOND_UPN).unwrap();
    assert!(setup.abandon(SECOND_UPN).unwrap());
    assert_eq!(setup.accounts().unwrap(), vec![signed]);
    assert!(!added.is_signed_in());
}

/// Standing in for the sign-in window, answering `answer` to the request.
fn answering(answer: &SignInAnswer) -> signin::Window {
    let answer = answer.clone();
    Box::new(move |_| answer)
}

/// `UPN`'s entry as the user writes it by hand into `accounts.toml`: its identity provider's host.
fn hand_written_idp_hosts(setup: &Setup) {
    let text = format!("[[account]]\nupn = \"{UPN}\"\nidp_hosts = [\"sso.example.test\"]\n");
    fs::create_dir_all(setup.paths.config_dir()).unwrap();
    fs::write(setup.paths.accounts_file(), text).unwrap();
}

#[tokio::test]
async fn a_failed_sign_in_keeps_a_hand_written_idp_hosts_and_the_retry_takes_it_up() {
    let dir = tempfile::tempdir().unwrap();
    let setup = setup_in(&dir);
    let server = MockServer::start().await;
    let clients = clients(&server, UPN, OID).await;
    let store = MemorySecretStore::new();
    // an entry the sign-in wrote and nothing more goes when the window is closed (a typo leaves nothing behind)
    let closed = signin::add(&setup, UPN, &clients, &store, |_| Ok(answering(&SignInAnswer::Closed))).await;
    assert!(closed.unwrap_err().starts_with("the sign-in stored nothing: "));
    assert!(setup.accounts().unwrap().is_empty());

    // one the user wrote its identity provider's host into stays, closed or failed, pending: no account starts
    hand_written_idp_hosts(&setup);
    for answer in [
        SignInAnswer::Closed,
        SignInAnswer::Failed("the page wasn't armed in time".into()),
    ] {
        assert!(
            signin::add(&setup, UPN, &clients, &store, |_| Ok(answering(&answer)))
                .await
                .is_err()
        );
        let kept = PersonalAccount::find(&setup.paths, UPN).unwrap().unwrap();
        assert_eq!(kept.idp_hosts, vec!["sso.example.test".to_owned()], "{answer:?}");
        assert!(!kept.is_signed_in());
        assert!(
            setup.signed_in_accounts().unwrap().is_empty(),
            "never an account to start"
        );
        assert_eq!(
            setup.pending_upns().unwrap(),
            vec![UPN.to_owned()],
            "the first run's screen fills it in"
        );
    }
    assert!(store.get(&refresh_token_key(UPN)).await.unwrap().is_none());

    // the retry, the same UPN case aside, takes the entry up: its window is made for the account with the host, and
    // the sign-in keeps it
    let mut windowed = None;
    let signed = signin::add(&setup, "SOMEONE@example.test", &clients, &store, |account| {
        windowed = Some(account.idp_hosts.clone());
        Ok(window())
    })
    .await
    .unwrap();
    assert_eq!(windowed, Some(vec!["sso.example.test".to_owned()]));
    assert_eq!(signed.upn, UPN);
    assert_eq!(signed.idp_hosts, vec!["sso.example.test".to_owned()]);
    assert_eq!(setup.signed_in_accounts().unwrap(), vec![signed]);
}

#[tokio::test]
async fn a_host_the_window_refused_is_named_with_where_to_add_it_and_the_entry_stays_for_it() {
    let dir = tempfile::tempdir().unwrap();
    let setup = setup_in(&dir);
    let server = MockServer::start().await;
    let clients = clients(&server, UPN, OID).await;
    let store = MemorySecretStore::new();
    // the window closed after it refused the organization's sign-in page: the host alone comes back
    let refused = SignInAnswer::Refused("sso.example.test".into());
    let error = signin::add(&setup, UPN, &clients, &store, |_| Ok(answering(&refused)))
        .await
        .unwrap_err();
    let file = setup.paths.accounts_file();
    assert_eq!(
        error,
        format!(
            "the sign-in window refused sso.example.test, your organization's sign-in page: add it to idp_hosts in the \
             [[account]] entry of {UPN} in {} (idp_hosts = [\"sso.example.test\"]), then sign in again",
            file.display()
        )
    );
    assert!(
        !error.contains("https://") && !error.contains('?'),
        "a host, never a URL: {error}"
    );
    // the entry stays for the user to add the host to, and nothing adds it by itself
    let kept = PersonalAccount::find(&setup.paths, UPN).unwrap().unwrap();
    assert!(kept.idp_hosts.is_empty() && !kept.is_signed_in());
    assert!(store.get(&refresh_token_key(UPN)).await.unwrap().is_none());
    // the same error through a sign-in of an account held: the same words
    let held = signin::sign_in(&setup, &kept, &clients, &store, answering(&refused)).await;
    let Err(held) = held else {
        panic!("the window refused a host");
    };
    assert_eq!(signin::refused_host(&held), Some("sso.example.test"));
    assert_eq!(signin::failure(&setup, &kept, &held), error);
}

#[tokio::test]
async fn a_second_account_s_sign_in_keeps_the_first_and_each_has_its_region_and_store() {
    let dir = tempfile::tempdir().unwrap();
    let setup = setup_in(&dir);
    let store = MemorySecretStore::new();
    let first = signed_in(&setup, UPN, OID, &store).await;
    let second = signed_in(&setup, SECOND_UPN, OTHER_OID, &store).await;
    assert_eq!(second.oid.as_deref(), Some(OTHER_OID));
    assert_eq!(setup.signed_in_accounts().unwrap(), vec![first.clone(), second.clone()]);
    let regions: Vec<_> = [&first, &second]
        .iter()
        .map(|account| setup.paths.region_file(account).unwrap())
        .collect();
    assert_ne!(regions[0], regions[1]);
    let mut held: Vec<OsString> = fs::read_dir(setup.paths.data_dir().join("regions"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    held.sort();
    assert_eq!(
        held,
        [format!("{OTHER_OID}.json"), format!("{OID}.json")].map(OsString::from)
    );
    for account in [&first, &second] {
        assert!(store.get(&refresh_token_key(&account.upn)).await.unwrap().is_some());
        assert!(signin::ready(&setup, account, &store).await.unwrap());
    }
    // each account has its engine and store, side by side
    let engines = [start_engine(&setup, &first), start_engine(&setup, &second)];
    let stores = [&first, &second].map(|account| setup.paths.store_file(account).unwrap());
    assert_ne!(stores[0], stores[1]);
    assert!(stores.iter().all(|store| store.is_file()));
    for engine in engines {
        engine.handle.shutdown();
    }
}

#[tokio::test]
async fn a_sign_in_landing_in_an_account_already_held_under_another_upn_stores_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let setup = setup_in(&dir);
    let store = MemorySecretStore::new();
    let first = signed_in(&setup, UPN, OID, &store).await;
    let server = MockServer::start().await;
    let clients = clients(&server, SECOND_UPN, OID).await;
    let alias = setup.ask_for(SECOND_UPN).unwrap();
    let refused = signin::sign_in(&setup, &alias, &clients, &store, window()).await;
    assert!(
        matches!(refused, Err(SignInError::Personal(PersonalError::Account(_)))),
        "{refused:?}"
    );
    assert!(store.get(&refresh_token_key(SECOND_UPN)).await.unwrap().is_none());
    assert!(store.get(&refresh_token_key(UPN)).await.unwrap().is_some());
    assert_eq!(setup.accounts().unwrap(), vec![first, alias]);
}

#[tokio::test]
async fn forget_is_refused_while_the_engine_runs_and_forgets_one_account_leaving_the_other() {
    let dir = tempfile::tempdir().unwrap();
    let setup = setup_in(&dir);
    let secrets = MemorySecretStore::new();
    let signed = signed_in(&setup, UPN, OID, &secrets).await;
    let second = signed_in(&setup, SECOND_UPN, OTHER_OID, &secrets).await;
    let engine = start_engine(&setup, &signed);
    let first_region = setup.paths.region_file(&signed).unwrap();
    let second_region = setup.paths.region_file(&second).unwrap();

    // the engine runs: nothing is deleted
    let error = forget::forget(&setup, UPN, &secrets).await.unwrap_err();
    assert!(
        matches!(&error, ForgetError::Running { upn, .. } if upn == UPN),
        "{error}"
    );
    assert!(error.to_string().contains("quit it"), "{error}");
    assert!(secrets.get(&refresh_token_key(UPN)).await.unwrap().is_some());
    assert_eq!(setup.accounts().unwrap(), vec![signed.clone(), second.clone()]);
    assert!(first_region.is_file());

    // once it has stopped, the account goes (case aside) and its store stays; the other account is untouched
    engine.handle.shutdown();
    let kept = setup.paths.store_file(&signed).unwrap();
    assert_eq!(
        forget::forget(&setup, "SOMEONE@example.test", &secrets).await.unwrap(),
        Forgot::Account {
            upn: UPN.to_owned(),
            store: Some(kept.clone()),
        }
    );
    for key in [refresh_token_key(UPN), skypetoken_key(UPN), skypetoken_expiry_key(UPN)] {
        assert!(secrets.get(&key).await.unwrap().is_none(), "{key:?}");
    }
    for key in [
        refresh_token_key(SECOND_UPN),
        skypetoken_key(SECOND_UPN),
        skypetoken_expiry_key(SECOND_UPN),
    ] {
        assert!(secrets.get(&key).await.unwrap().is_some(), "{key:?}");
    }
    assert_eq!(setup.accounts().unwrap(), vec![second.clone()]);
    assert!(!first_region.exists());
    assert!(second_region.is_file());
    assert!(kept.is_file());
    assert!(signin::ready(&setup, &second, &secrets).await.unwrap());
    assert_eq!(forget::forget(&setup, UPN, &secrets).await.unwrap(), Forgot::Nothing);

    // a pending account is forgotten with no store and no lock to take
    setup.ask_for("pending@example.test").unwrap();
    assert_eq!(
        forget::forget(&setup, "pending@example.test", &secrets).await.unwrap(),
        Forgot::Account {
            upn: "pending@example.test".to_owned(),
            store: None,
        }
    );

    // another account signed in next has a store of its own, never the kept one
    let other = signed_in(
        &setup,
        "third@example.test",
        "00000000-0000-4000-8000-0000000000dd",
        &secrets,
    )
    .await;
    let engine = start_engine(&setup, &other);
    let own = setup.paths.store_file(&other).unwrap();
    assert_ne!(own, kept);
    assert!(own.is_file());
    engine.handle.shutdown();
}

#[tokio::test]
async fn a_single_account_of_an_earlier_build_is_moved_into_the_files_of_several() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(&dir);
    fs::create_dir_all(paths.config_dir()).unwrap();
    fs::write(
        paths.legacy_account_file(),
        format!("upn = \"{UPN}\"\noid = \"{OID}\"\ntid = \"{TENANT}\"\nidp_hosts = [\"sso.example.test\"]\n"),
    )
    .unwrap();
    paths.make_dirs().unwrap();
    let region =
        format!(r#"{{"region": "amer", "partition": null, "regionGtms": {{"chatServiceAfd": "{CHAT_BASE}"}}}}"#);
    fs::write(paths.legacy_region_file(), &region).unwrap();

    let setup = Setup::new(paths).unwrap();
    assert!(!setup.paths.legacy_account_file().exists());
    assert!(!setup.paths.legacy_region_file().exists());
    assert!(setup.paths.accounts_file().is_file());
    let mut held = PersonalAccount::new(UPN, OID, TENANT).unwrap();
    held.idp_hosts = vec!["sso.example.test".to_owned()];
    assert_eq!(setup.signed_in_accounts().unwrap(), vec![held.clone()]);
    let moved = setup.paths.region_file(&held).unwrap();
    assert_eq!(fs::read_to_string(moved).unwrap(), region);
    let store = MemorySecretStore::new();
    store
        .put(&refresh_token_key(UPN), &Secret::new("made-up-refresh-token"))
        .await
        .unwrap();
    assert!(signin::ready(&setup, &held, &store).await.unwrap());
    // a second setup over the moved files changes nothing
    let again = Setup::new(setup.paths.clone()).unwrap();
    assert_eq!(again.accounts().unwrap(), vec![held]);
}
