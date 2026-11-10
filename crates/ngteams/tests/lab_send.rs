//! A lab alias sends through a real session and writable broker to the loopback chat mock only.

#![cfg(test)]

use std::sync::Arc;
use std::time::Duration;
use teams_auth::broker::private_pair;

use ngteams::accounts_file::{PersonalAccount, PersonalLocations};
use ngteams::lab::{self, LAB_KEYRING_APPLICATION};
use ngteams::read_only;
use ngteams::secrets::{AppSecrets, lab_broker_scope};
use ngteams::setup::Setup;
use serde_json::json;
use teams_auth::authz::{AuthzClient, Region};
use teams_auth::refresh::refresh_token_key;
use teams_auth::secrets::{MemorySecretStore, Secret, SecretStore};
use teams_auth::session::Session;
use teams_auth::token::{EntraConfig, TokenClient};
use teams_core::Outbox;
use teams_core::engine::{self, Action, EngineHandle, EngineOptions, Parts};
use teams_mock::{AB_CHAT, CHAT_BASE, ChatServiceMock, EventKind, LAB_A, LAB_B, TENANT};
use teams_proto::chat::MessageText;
use teams_proto::{Author, Client, HttpTransport, Mri};
use teams_store::{OutboxState, Store};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const UPN: &str = "lab-a@synthetic.example";
const TEXT: &str = "lab broker sends to b";

async fn token_endpoint(server: &MockServer, bearer: &str) -> TokenClient {
    Mock::given(method("POST"))
        .and(path("/organizations/oauth2/v2.0/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=synthetic-refresh"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "token_type": "Bearer", "scope": "https://ic3.teams.office.com/.default", "expires_in": 7200,
            "access_token": bearer, "refresh_token": "synthetic-rotated-refresh",
        })))
        .expect(1)
        .mount(server)
        .await;
    TokenClient::new(EntraConfig::teams_native().with_authority(format!("{}/organizations/", server.uri()))).unwrap()
}

fn writable_engine(mock: &ChatServiceMock, setup: &Setup, session: &Arc<Session<AppSecrets>>) -> EngineHandle {
    let http = || HttpTransport::with_mock_origin(Arc::clone(session), CHAT_BASE, &mock.origin()).unwrap();
    let account = PersonalAccount::new(UPN, LAB_A.strip_prefix("8:orgid:").unwrap(), TENANT).unwrap();
    account.save(&setup.paths).unwrap();
    setup.paths.make_dirs().unwrap();
    let file = setup.paths.store_file(&account).unwrap();
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let outbox = Outbox::new(Arc::new(http()), Author::new(Mri::new(LAB_A), "Synthetic Lab A"));
    assert!(!outbox.is_read_only());
    engine::start(Parts {
        calls: None,
        store: Store::open(&file).unwrap(),
        client: Arc::new(Client::new(http())),
        outbox,
        push: None,
        options: EngineOptions {
            tick: Duration::from_millis(50),
            ..EngineOptions::default()
        },
        known_names: Vec::new(),
    })
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lab_alias_sends_to_another_lab_account_and_rotates_credentials_through_its_broker() {
    let root = tempfile::tempdir().unwrap();
    let mine = PersonalLocations::resolve(
        Some(root.path().join("config").into_os_string()),
        Some(root.path().join("share").into_os_string()),
        None,
    )
    .unwrap();
    let paths = lab::paths_for("a", &mine).unwrap();
    let backing = Arc::new(MemorySecretStore::new());
    let refresh = refresh_token_key(UPN);
    backing.put(&refresh, &Secret::new("synthetic-refresh")).await.unwrap();
    let (ours, theirs) = private_pair().unwrap();
    let served_store = Arc::clone(&backing);
    let served = tokio::spawn(async move {
        let mut notes = Vec::new();
        read_only::serve_broker(theirs, &lab_broker_scope(UPN), &*served_store, &mut notes)
            .await
            .unwrap();
        notes
    });
    let setup = Setup::new(paths).unwrap().with_secrets(AppSecrets::lab(ours).unwrap());
    assert_eq!(setup.secrets().application(), LAB_KEYRING_APPLICATION);
    let mock = ChatServiceMock::start().await.unwrap();
    let token_server = MockServer::start().await;
    let token = token_endpoint(&token_server, &mock.token(LAB_A).unwrap()).await;
    let session = Arc::new(Session::new(
        UPN,
        setup.secrets(),
        token,
        AuthzClient::with_endpoint(&format!("{}/authz", token_server.uri())).unwrap(),
        Region::default(),
    ));
    let handle = writable_engine(&mock, &setup, &session);
    let requests = handle.requests();
    assert!(!requests.read_only());
    requests.sync_conversation(AB_CHAT).await.unwrap();
    let row = requests
        .write(Action::Send {
            conversation: AB_CHAT.to_owned(),
            text: MessageText::plain(TEXT).unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(row.state, OutboxState::Sent, "{row:?}");
    let delivered = mock
        .take_events(LAB_B)
        .into_iter()
        .find(|event| event.kind == EventKind::Message && event.conversation == AB_CHAT)
        .unwrap();
    assert_eq!(delivered.from, LAB_A);
    assert!(delivered.resource["content"].as_str().unwrap().contains(TEXT));
    assert_eq!(delivered.resource["clientmessageid"], row.client_message_id);
    assert_eq!(
        backing.get(&refresh).await.unwrap().unwrap().expose(),
        "synthetic-rotated-refresh"
    );
    assert!(!mine.config_dir.exists() && !mine.data_dir.exists());
    assert!(setup.paths.accounts_file().is_file());
    handle.shutdown();
    drop((requests, session, setup));
    assert!(
        tokio::time::timeout(Duration::from_secs(5), served)
            .await
            .unwrap()
            .unwrap()
            .is_empty()
    );
}
