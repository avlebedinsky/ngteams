//! End-to-end private upload and Send's direct grant, with handwritten service answers only.
use super::{Uploads, upload};
use crate::Outbox;
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use teams_auth::{secrets::Secret, token::TokenError};
use teams_fence::{Fence, FenceLog, Membership, MembershipFuture, MembershipSource, Mri, Roster, Scope};
use teams_lab_targets::{LabAccount, LabRoster, one_to_one_id};
use teams_proto::{Author, Client, Credentials, HttpTransport};
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{method, path},
};
const A: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const B: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
const C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const SITE: &str = "https://madeup-my.sharepoint.com/personal/lab_a/";
const DRIVE: &str = "/personal/lab_a/_api/v2.0/drive";
const ITEM: &str = "/personal/lab_a/_api/v2.0/sites/root/items/00000000-0000-4000-8000-000000000001/driveItem";
struct Tokens;
impl Credentials for Tokens {
    fn bearer(&self, _: &str) -> impl std::future::Future<Output = Result<Secret, TokenError>> + Send {
        std::future::ready(Ok(Secret::new("placeholder")))
    }
    async fn bearer_rejected(&self, audience: &str, _: &Secret) -> Result<Secret, TokenError> {
        self.bearer(audience).await
    }
}
struct Members;
impl MembershipSource for Members {
    fn members<'a>(&'a self, _: &'a Scope) -> MembershipFuture<'a> {
        Box::pin(async { Membership::Complete(vec![Mri::new(A), Mri::new(B)]) })
    }
}
async fn get(server: &MockServer, url: &str, body: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path(url))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}
fn grants(shared: bool) -> serde_json::Value {
    json!({"value":if shared {vec![grant(A,"owner"),grant(B,"write")]} else {vec![grant(A,"owner")]}})
}
fn grant(mri: &str, role: &str) -> serde_json::Value {
    json!({"id":mri,"roles":[role],"grantedToV2":{"user":{"id":mri.strip_prefix("8:orgid:").unwrap()}}})
}
type NativeClient = Arc<Client<HttpTransport<Tokens>>>;
type NativeOutbox = Arc<Outbox<HttpTransport<Tokens>>>;
fn native(server: &MockServer) -> (NativeClient, NativeOutbox) {
    let fence = Arc::new(Fence::new(
        Roster::new([A, B, C].map(Mri::new), []),
        FenceLog::from_writer(std::io::sink()),
    ));
    let roster = Arc::new(
        LabRoster::new([A, B, C].map(|mri| LabAccount {
            mri: mri.into(),
            upn: format!("{}@lab.example.test", mri.chars().last().unwrap()),
        }))
        .unwrap(),
    );
    let transport = || {
        HttpTransport::with_mock_origin(
            Arc::new(Tokens),
            Arc::clone(&fence),
            "https://teams.microsoft.com/api/chatsvc/amer",
            &server.uri(),
        )
        .unwrap()
        .with_lab_targets(A.into(), "a@lab.example.test".into(), Arc::clone(&roster))
        .unwrap()
    };
    let client = Arc::new(Client::new(transport()));
    let outbox = Arc::new(Outbox::new(
        Arc::new(transport()),
        fence,
        Arc::new(Members),
        Author::new(Mri::new(A), "A"),
    ));
    (client, outbox)
}
async fn mock_upload(server: &MockServer) {
    get(server, "/v1.0/me/drive", json!({"webUrl":format!("{SITE}Documents")})).await;
    get(
        server,
        "/v1.0/me",
        json!({"id":A.strip_prefix("8:orgid:").unwrap(),"userPrincipalName":"a@lab.example.test"}),
    )
    .await;
    get(
        server,
        DRIVE,
        json!({"owner":{"user":{"id":A.strip_prefix("8:orgid:").unwrap()}}}),
    )
    .await;
    get(
        server,
        &format!("{DRIVE}/root:/Microsoft%20Teams%20Chat%20Files"),
        json!({"id":"folder"}),
    )
    .await;
    get(
        server,
        &format!("{DRIVE}/root:/Microsoft%20Teams%20Chat%20Files:/permissions"),
        grants(false),
    )
    .await;
    let uploaded = json!({"id":"ABC1","name":"renamed.txt","size":3,"webUrl":format!("{SITE}Documents/renamed.txt"),"parentReference":{"driveId":"drive1"},"sharepointIds":{"listItemUniqueId":"00000000-0000-4000-8000-000000000001"}});
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(201).set_body_json(uploaded))
        .mount(server)
        .await;
}
#[tokio::test]
async fn private_upload_then_send_shares_once_with_named_chat_members_and_confirms_access() {
    let server = MockServer::start().await;
    let (client, outbox) = native(&server);
    mock_upload(&server).await;
    let temp = tempfile::tempdir().unwrap();
    let local_file = temp.path().join("demo.txt");
    std::fs::write(&local_file, b"abc").unwrap();
    let file = upload(&client, &outbox, local_file).await.unwrap();
    assert_eq!(file.name(), "renamed.txt");
    assert_eq!(file.size(), 3);
    assert_eq!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.method.as_str() == "PUT")
            .count(),
        1
    );
    assert!(
        !server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .any(|request| request.method.as_str() == "POST"),
        "staging never shares"
    );
    let chat = one_to_one_id(A, B).unwrap();
    get(
        &server,
        &format!("/api/chatsvc/amer/v1/threads/{chat}"),
        json!({"id":chat,"members":[{"id":A},{"id":B}]}),
    )
    .await;
    get(
        &server,
        "/v1.0/users/00000000-0000-4000-8000-00000000000b",
        json!({"id":B.strip_prefix("8:orgid:").unwrap(),"userPrincipalName":"b@lab.example.test"}),
    )
    .await;
    let shared = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&shared);
    Mock::given(method("GET"))
        .and(path(format!("{ITEM}/permissions")))
        .respond_with(move |_: &Request| {
            ResponseTemplate::new(200).set_body_json(grants(observed.load(Ordering::SeqCst)))
        })
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{ITEM}/invite")))
        .respond_with(move |request: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(body["requireSignIn"], true);
            assert_eq!(body["sendInvitation"], false);
            shared.store(true, Ordering::SeqCst);
            ResponseTemplate::new(200).set_body_json(json!({"value":[]}))
        })
        .mount(&server)
        .await;
    let uploads = Uploads::new(client, outbox);
    uploads.share(&chat, &[file]).await.unwrap();
    assert_eq!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.method.as_str() == "POST")
            .count(),
        1
    );
}
