//! The personal build's sign-in and start: `teams` signs in through its sign-in window (`sh` standing in for
//! `teams-signin-personal`, answering the redirect), the code redeemed at a local token endpoint and the skypetoken at a
//! local authz (wiremock), the refresh token stored under the account's UPN in the store handed in (the app hands in the
//! desktop's Secret Service under `teams-personal`; here a `MemorySecretStore`), `account.toml` given the object id and
//! tenant, the region saved; a later sign-in landing in another account stores nothing; a lab account is refused; the
//! engine opens its store under the personal data dir. Every directory is a test's own; every value is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::process::Command;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use teams::KEYRING_APPLICATION;
use teams::setup::{Setup, SetupError};
use teams::signin::{self, SignInClients, SignInError};
use teams_auth::authz::AuthzClient;
use teams_auth::refresh::refresh_token_key;
use teams_auth::secrets::{MemorySecretStore, Secret, SecretStore};
use teams_auth::signin::{WindowAnswer, WindowSignInError};
use teams_auth::token::{EntraConfig, NATIVE_CLIENT_REDIRECT, TEAMS_NATIVE_CLIENT_ID, TokenClient, TokenError};
use teams_fence::Fence;
use teams_fence::lab::LabLocations;
use teams_fence::personal::{PersonalAccount, PersonalError, PersonalPaths};
use teams_mock::CHAT_BASE;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const UPN: &str = "someone@example.test";
const OID: &str = "00000000-0000-4000-8000-0000000000ff";
const TENANT: &str = "00000000-0000-4000-8000-000000000001";
const OTHER_OID: &str = "00000000-0000-4000-8000-0000000000ee";
const LAB_A_OID: &str = "00000000-0000-4000-8000-00000000000a";
const AUTHZ_FIXTURE: &str = include_str!("../../../tests/fixtures/auth/authz-ok.json");

fn lab_toml() -> String {
    let mut text = String::new();
    for (alias, end) in ["a", "b", "c"].iter().zip(["0a", "0b", "0c"]) {
        text.push_str(
            &[
                "[[accounts]]\nalias = \"",
                alias,
                "\"\nupn = \"bot-",
                alias,
                "@lab.example.test\"\n",
            ]
            .concat(),
        );
        text.push_str(&["mri = \"8:orgid:00000000-0000-4000-8000-0000000000", end, "\"\n"].concat());
    }
    text
}

/// The personal build's files and a lab beside them, all under `dir`.
fn setup_in(dir: &tempfile::TempDir) -> Setup {
    let config_file = dir.path().join("config/teams-lab/lab.toml");
    fs::create_dir_all(config_file.parent().unwrap()).unwrap();
    fs::write(&config_file, lab_toml()).unwrap();
    let lab = LabLocations {
        config_file,
        data_dir: dir.path().join("share/teams-lab"),
        personal_data_dir: dir.path().join("share/teams"),
    };
    let paths = PersonalPaths::new(dir.path().join("config/teams"), dir.path().join("share/teams"));
    Setup::new(paths, lab).unwrap()
}

/// An unsigned ID token naming `upn`, `oid`, the tenant, for the native client.
fn id_token(upn: &str, oid: &str) -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"RS256","kid":"test"}"#);
    let claims = json!({"aud": TEAMS_NATIVE_CLIENT_ID, "oid": oid, "tid": TENANT, "preferred_username": upn});
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

/// `sh` standing in for the sign-in window: it reads the request and answers the redirect to it, its `state` taken
/// from the request's authorize URL.
fn window() -> Command {
    let mut command = Command::new("sh");
    command.arg("-c").arg(format!(
        r#"read -r line; state=$(printf '%s' "$line" | sed -n 's/.*[?&]state=\([^&"]*\).*/\1/p'); printf '{{"redirect":"{NATIVE_CLIENT_REDIRECT}?code=made-up-code&state=%s"}}\n' "$state""#
    ));
    command
}

#[tokio::test]
async fn the_first_sign_in_stores_the_token_the_account_and_the_region_and_the_engine_opens_its_store_there() {
    let dir = tempfile::tempdir().unwrap();
    let setup = setup_in(&dir);
    let server = MockServer::start().await;
    let clients = clients(&server, UPN, OID).await;
    let store = MemorySecretStore::new();
    let account = setup.account(Some(UPN)).unwrap();
    assert!(!account.is_signed_in());
    assert!(!signin::ready(&setup, &account, &store).await.unwrap());

    let signed = signin::sign_in(&setup, &account, &clients, &store, window())
        .await
        .unwrap();
    assert_eq!(signed.oid.as_deref(), Some(OID));
    assert_eq!(signed.tid.as_deref(), Some(TENANT));
    let stored = store.get(&refresh_token_key(UPN)).await.unwrap();
    assert_eq!(stored.as_ref().map(Secret::expose), Some("made-up-refresh-token"));
    assert_eq!(PersonalAccount::load(&setup.paths).unwrap(), Some(signed.clone()));
    let region = fs::read_to_string(setup.paths.region_file()).unwrap();
    assert!(
        region.contains(CHAT_BASE) && !region.contains("made-up-skypetoken"),
        "{region}"
    );
    assert_eq!(
        fs::metadata(setup.paths.region_file()).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(signin::ready(&setup, &signed, &store).await.unwrap());

    // the engine: the personal fence from account.toml, the store under the personal data dir; nothing is sent
    let fence = Arc::new(Fence::open_personal_with(&setup.paths, &setup.lab).unwrap());
    let engine = teams::engine::start(&setup.paths, &signed, fence, MemorySecretStore::new(), false).unwrap();
    assert!(setup.paths.store_file().is_file());
    assert!(setup.paths.store_file().starts_with(setup.paths.data_dir()));
    engine.handle.shutdown();
    assert_eq!(KEYRING_APPLICATION, "teams-personal");
}

#[tokio::test]
async fn a_later_sign_in_that_lands_in_another_account_stores_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let setup = setup_in(&dir);
    PersonalAccount::new(UPN, OID, TENANT)
        .unwrap()
        .save(&setup.paths)
        .unwrap();
    let account = setup.account(None).unwrap();
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
    assert_eq!(PersonalAccount::load(&setup.paths).unwrap(), Some(account));
    // a window closed before the redirect stores nothing either
    let mut closed = Command::new("sh");
    closed.arg("-c").arg(format!(
        "read -r line; printf '%s\\n' '{}'",
        WindowAnswer::Closed.to_line().expose()
    ));
    assert!(
        signin::sign_in(&setup, &setup.account(None).unwrap(), &clients, &store, closed)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_lab_account_is_refused_before_the_window_and_after_it() {
    let dir = tempfile::tempdir().unwrap();
    let setup = setup_in(&dir);
    // by its UPN, before anything is asked
    assert!(matches!(
        setup.account(Some("bot-b@lab.example.test")),
        Err(SetupError::Personal(PersonalError::LabAccount))
    ));
    assert!(!setup.paths.account_file().exists());
    // by its MRI, once the ID token names it: what was stored goes again
    let server = MockServer::start().await;
    let clients = clients(&server, UPN, LAB_A_OID).await;
    let store = MemorySecretStore::new();
    let account = setup.account(Some(UPN)).unwrap();
    let refused = signin::sign_in(&setup, &account, &clients, &store, window()).await;
    assert!(
        matches!(refused, Err(SignInError::Personal(PersonalError::LabAccount))),
        "{refused:?}"
    );
    assert!(store.get(&refresh_token_key(UPN)).await.unwrap().is_none());
    assert!(!PersonalAccount::load(&setup.paths).unwrap().unwrap().is_signed_in());
}

#[test]
fn the_account_is_given_once_and_only_repeated_afterwards() {
    let dir = tempfile::tempdir().unwrap();
    let setup = setup_in(&dir);
    assert!(matches!(setup.account(None), Err(SetupError::NoAccount)));
    let pending = setup.account(Some(UPN)).unwrap();
    assert_eq!(pending.upn, UPN);
    assert_eq!(
        fs::metadata(setup.paths.account_file()).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(setup.account(None).unwrap(), pending);
    assert_eq!(setup.account(Some("SOMEONE@example.test")).unwrap(), pending);
    assert!(matches!(
        setup.account(Some("other@example.test")),
        Err(SetupError::OtherAccount { .. })
    ));
}
