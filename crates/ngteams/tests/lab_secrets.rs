//! Lab credentials use a synthetic in-process broker, never the operator's keyring or tenant.

#![cfg(test)]

use std::os::unix::net::UnixStream;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ngteams::accounts;
use ngteams::accounts_file::PersonalPaths;
use ngteams::forget::Forgot;
use ngteams::lab::LAB_KEYRING_APPLICATION;
use ngteams::read_only;
use ngteams::secrets::{AppSecrets, lab_broker_scope};
use ngteams::setup::Setup;
use ngteams::signin::{self, SignInClients};
use serde_json::{Value, json};
use teams_auth::authz::{AuthzClient, skypetoken_expiry_key, skypetoken_key};
use teams_auth::broker::BrokerSecretStore;
use teams_auth::refresh::refresh_token_key;
use teams_auth::secrets::{MemorySecretStore, Secret, SecretKey, SecretStore};
use teams_auth::signin::{SignInAnswer, SignInRequest};
use teams_auth::token::{EntraConfig, NATIVE_CLIENT_REDIRECT, Scope, TEAMS_NATIVE_CLIENT_ID, TokenClient};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const UPN: &str = "lab-a@synthetic.example";
const OID: &str = "00000000-0000-4000-8000-0000000000ff";
const TENANT: &str = "00000000-0000-4000-8000-000000000001";

fn serve(socket: UnixStream, store: Arc<MemorySecretStore>) -> tokio::task::JoinHandle<(bool, Vec<u8>)> {
    tokio::spawn(async move {
        let mut noted = Vec::new();
        let done = read_only::serve_broker(socket, &lab_broker_scope(UPN), &*store, &mut noted)
            .await
            .is_ok();
        (done, noted)
    })
}

#[tokio::test]
async fn lab_store_clones_share_writable_broker_and_refuse_other_accounts() {
    let backing = Arc::new(MemorySecretStore::new());
    let (ours, theirs) = UnixStream::pair().unwrap();
    let served = serve(theirs, Arc::clone(&backing));
    let store = AppSecrets::lab(ours).unwrap();
    let clone = store.clone();
    let key = refresh_token_key(UPN);
    assert_eq!(store.application(), LAB_KEYRING_APPLICATION);
    assert!(!lab_broker_scope(UPN).is_get_only());
    assert!(read_only::broker_scope(UPN).is_get_only());
    assert_eq!(AppSecrets::personal().application(), ngteams::KEYRING_APPLICATION);
    clone.put(&key, &Secret::new("synthetic-refresh")).await.unwrap();
    assert_eq!(store.get(&key).await.unwrap().unwrap().expose(), "synthetic-refresh");
    assert_eq!(backing.get(&key).await.unwrap().unwrap().expose(), "synthetic-refresh");
    let other = SecretKey::new("other@synthetic.example", "refresh_token");
    assert!(store.get(&other).await.is_err());
    assert!(store.put(&other, &Secret::new("synthetic-other")).await.is_err());
    assert!(store.delete(&other).await.is_err());
    assert!(backing.get(&other).await.unwrap().is_none());
    clone.delete(&key).await.unwrap();
    assert!(store.get(&key).await.unwrap().is_none());
    drop((store, clone));
    let (closed, notes) = served.await.unwrap();
    assert!(closed);
    let notes = String::from_utf8(notes).unwrap();
    assert!(notes.contains("refused"));
    assert!(!notes.contains("synthetic-refresh") && !notes.contains("synthetic-other"));
}

#[tokio::test]
async fn lab_broker_refuses_the_personal_namespace_without_touching_store() {
    let backing = Arc::new(MemorySecretStore::new());
    let (ours, theirs) = UnixStream::pair().unwrap();
    let served = serve(theirs, Arc::clone(&backing));
    let wrong = BrokerSecretStore::new(ours, ngteams::KEYRING_APPLICATION).unwrap();
    let key = refresh_token_key(UPN);
    assert!(wrong.get(&key).await.is_err());
    assert!(
        wrong
            .put(&key, &Secret::new("synthetic-wrong-namespace"))
            .await
            .is_err()
    );
    assert!(wrong.delete(&key).await.is_err());
    assert!(backing.get(&key).await.unwrap().is_none());
    drop(wrong);
    assert!(served.await.unwrap().0);
}

#[tokio::test]
async fn lab_sign_in_refresh_readiness_and_forget_use_the_same_selected_broker() {
    let root = tempfile::tempdir().unwrap();
    let paths = PersonalPaths::new(root.path().join("config"), root.path().join("data"));
    let backing = Arc::new(MemorySecretStore::new());
    let (ours, theirs) = UnixStream::pair().unwrap();
    let served = serve(theirs, Arc::clone(&backing));
    let store = AppSecrets::lab(ours).unwrap();
    let setup = Setup::new(paths).unwrap().with_secrets(store.clone());
    let pending = setup.ask_for(UPN).unwrap();
    let endpoint = MockServer::start().await;
    let clients = clients(&endpoint).await;
    let signed = signin::sign_in(&setup, &pending, &clients, &setup.secrets(), window())
        .await
        .unwrap();
    let refresh = refresh_token_key(UPN);
    // Sign-in redeems the code, then reads that stored refresh token to obtain authz's access token. The mock
    // requires the code grant's exact token on that first refresh and returns a distinct rotation.
    assert_eq!(
        backing.get(&refresh).await.unwrap().unwrap().expose(),
        "synthetic-signin-refresh"
    );
    assert!(backing.get(&skypetoken_key(UPN)).await.unwrap().is_some());
    assert!(backing.get(&skypetoken_expiry_key(UPN)).await.unwrap().is_some());
    let (ready_setup, ready_account) = (setup.clone(), signed.clone());
    assert!(
        tokio::task::spawn_blocking(move || accounts::ready(&ready_setup, &ready_account))
            .await
            .unwrap()
            .unwrap()
    );
    clients
        .token
        .refresh(&setup.secrets(), UPN, &Scope::spaces())
        .await
        .unwrap();
    assert_eq!(
        backing.get(&refresh).await.unwrap().unwrap().expose(),
        "synthetic-rotated-refresh"
    );
    let forget_setup = setup.clone();
    let forgotten = tokio::task::spawn_blocking(move || accounts::forget(&forget_setup, UPN))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(forgotten, Forgot::Account { .. }));
    for key in [refresh, skypetoken_key(UPN), skypetoken_expiry_key(UPN)] {
        assert!(backing.get(&key).await.unwrap().is_none());
    }
    assert!(setup.accounts().unwrap().is_empty());
    drop((setup, store));
    let (closed, notes) = served.await.unwrap();
    assert!(closed && notes.is_empty());
}

fn id_token() -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"RS256","kid":"synthetic"}"#);
    let claims = json!({
        "aud": TEAMS_NATIVE_CLIENT_ID,
        "oid": OID,
        "tid": TENANT,
        "preferred_username": UPN,
        "name": "Synthetic Lab",
    });
    format!(
        "{header}.{}.c3ludGhldGljLXNpZ25hdHVyZQ",
        URL_SAFE_NO_PAD.encode(claims.to_string())
    )
}

async fn clients(server: &MockServer) -> SignInClients {
    let endpoint = "/organizations/oauth2/v2.0/token";
    Mock::given(method("POST"))
        .and(path(endpoint))
        .and(body_string_contains("grant_type=authorization_code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "token_type": "Bearer", "scope": "https://api.spaces.skype.com/.default", "expires_in": 3600,
            "access_token": "synthetic-access", "refresh_token": "synthetic-first-refresh", "id_token": id_token(),
        })))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path(endpoint))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=synthetic-first-refresh"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "token_type": "Bearer", "scope": "https://api.spaces.skype.com/.default", "expires_in": 3600,
            "access_token": "synthetic-signin-access", "refresh_token": "synthetic-signin-refresh",
        })))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path(endpoint))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=synthetic-signin-refresh"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "token_type": "Bearer", "scope": "https://api.spaces.skype.com/.default", "expires_in": 3600,
            "access_token": "synthetic-refreshed-access", "refresh_token": "synthetic-rotated-refresh",
        })))
        .expect(1)
        .mount(server)
        .await;
    let fixture: Value = serde_json::from_str(include_str!("../../../tests/fixtures/auth/authz-ok.json")).unwrap();
    let mut authz = fixture["response"]["body"].clone();
    authz["tokens"]["skypeToken"] = json!("synthetic-skypetoken");
    authz["regionGtms"]["chatServiceAfd"] = json!(teams_mock::CHAT_BASE);
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

fn window() -> signin::Window {
    Box::new(|request: SignInRequest| {
        let state = request
            .authorize
            .split(['?', '&'])
            .find_map(|pair| pair.strip_prefix("state="))
            .unwrap();
        SignInAnswer::Redirect(Secret::new(format!(
            "{NATIVE_CLIENT_REDIRECT}?code=synthetic-code&state={state}"
        )))
    })
}
