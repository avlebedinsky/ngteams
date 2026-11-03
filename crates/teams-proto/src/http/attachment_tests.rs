//! Handwritten protocol answers and placeholder identities; no captured account data.
use std::sync::Arc;

use serde_json::{Value, json};
use teams_auth::{secrets::Secret, token::TokenError};
use teams_fence::{Fence, FenceLog, Mri, Roster};
use teams_lab_targets::{LabAccount, LabRoster, one_to_one_id};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

use super::{Credentials, HttpTransport, Transport};
use crate::{Author, DriveSite, DriveWrite, SendMessage, UploadedFile, chat::MessageText};

const A: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const B: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
const C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const X: &str = "8:orgid:00000000-0000-4000-8000-00000000000d";
const SITE: &str = "https://madeup-my.sharepoint.com/personal/lab_a/";
const DRIVE: &str = "/personal/lab_a/_api/v2.0/drive";
const GUID: &str = "00000000-0000-4000-8000-000000000001";

#[tokio::test]
async fn creating_a_lab_chat_checks_the_peers_fresh_identity_before_the_post() {
    use crate::{Authorized, CreateChat, Mutating};
    for wrong_upn in [false, true] {
        let server = MockServer::start().await;
        identity(&server, A).await;
        get(&server,"/v1.0/users/00000000-0000-4000-8000-00000000000b", json!({"id":B.strip_prefix("8:orgid:").unwrap(),"userPrincipalName":if wrong_upn {"outside@example.test"} else {"b@lab.example.test"}})).await;
        Mock::given(method("POST"))
            .and(path("/api/chatsvc/amer/v1/threads"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id":"synthetic"})))
            .mount(&server)
            .await;
        let transport = transport(&server, true);
        let write = CreateChat::one_to_one(Mri::new(A), Mri::new(B)).unwrap();
        let permit = transport
            .fence
            .check(write.target(), &teams_fence::FailClosed)
            .await
            .unwrap();
        let result = transport.deliver(Authorized::new(write, permit).unwrap()).await;
        assert_eq!(result.is_ok(), !wrong_upn);
        assert_eq!(writes(&server).await.len(), usize::from(!wrong_upn));
    }
}

struct Tokens;
impl Credentials for Tokens {
    fn bearer(&self, _: &str) -> impl std::future::Future<Output = Result<Secret, TokenError>> + Send {
        std::future::ready(Ok(Secret::new("placeholder")))
    }
    async fn bearer_rejected(&self, audience: &str, _: &Secret) -> Result<Secret, TokenError> {
        self.bearer(audience).await
    }
}
fn roster() -> Arc<LabRoster> {
    Arc::new(
        LabRoster::new([A, B, C].map(|mri| LabAccount {
            mri: mri.into(),
            upn: format!("{}@lab.example.test", mri.chars().last().unwrap()),
        }))
        .unwrap(),
    )
}
fn transport(server: &MockServer, lab: bool) -> HttpTransport<Tokens> {
    let fence = Arc::new(Fence::new(
        Roster::new([Mri::new(A), Mri::new(B), Mri::new(C)], []),
        FenceLog::from_writer(std::io::sink()),
    ));
    let transport = HttpTransport::with_mock_origin(
        Arc::new(Tokens),
        fence,
        "https://teams.microsoft.com/api/chatsvc/amer",
        &server.uri(),
    )
    .unwrap();
    if lab {
        transport
            .with_lab_targets(A.into(), "a@lab.example.test".into(), roster())
            .unwrap()
    } else {
        transport
    }
}
fn site() -> DriveSite {
    DriveSite::from_web_url(SITE).unwrap()
}
fn file() -> UploadedFile {
    UploadedFile::from_upload(&site(), &serde_json::to_vec(&json!({"id":"ABC123","name":"demo.txt","size":3,"webUrl":format!("{SITE}Documents/demo.txt"),"parentReference":{"driveId":"drive1"},"sharepointIds":{"listItemUniqueId":GUID}})).unwrap()).unwrap()
}
fn grants(people: &[&str]) -> Value {
    json!({"value":people.iter().map(|mri|json!({"id":format!("grant-{mri}"),"roles":[if *mri==A {"owner"} else {"write"}],"grantedToV2":{"user":{"id":mri.strip_prefix("8:orgid:").unwrap()}}})).collect::<Vec<_>>()})
}
async fn get(server: &MockServer, address: &str, answer: Value) {
    Mock::given(method("GET"))
        .and(path(address.replace(' ', "%20")))
        .respond_with(ResponseTemplate::new(200).set_body_json(answer))
        .mount(server)
        .await;
}
async fn identity(server: &MockServer, actor: &str) {
    get(
        server,
        "/v1.0/me",
        json!({"id":actor.strip_prefix("8:orgid:").unwrap(),"userPrincipalName":"a@lab.example.test"}),
    )
    .await;
}
async fn chat(server: &MockServer, members: &[&str]) -> String {
    let id = one_to_one_id(A, B).unwrap();
    get(
        server,
        &format!("/api/chatsvc/amer/v1/threads/{id}"),
        json!({"id":id,"members":members.iter().map(|mri|json!({"id":mri})).collect::<Vec<_>>()}),
    )
    .await;
    id
}
async fn owner(server: &MockServer) {
    get(
        server,
        DRIVE,
        json!({"owner":{"user":{"id":A.strip_prefix("8:orgid:").unwrap()}}}),
    )
    .await;
}
async fn permissions(server: &MockServer, body: Value) {
    get(
        server,
        &format!("/personal/lab_a/_api/v2.0/sites/root/items/{GUID}/driveItem/permissions"),
        body,
    )
    .await;
}
async fn writes(server: &MockServer) -> Vec<wiremock::Request> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.method.as_str() != "GET")
        .collect()
}

#[tokio::test]
async fn a_private_upload_uses_encoded_filename_and_never_overwrites() {
    let server = MockServer::start().await;
    identity(&server, A).await;
    owner(&server).await;
    get(
        &server,
        &format!("{DRIVE}/root:/Microsoft Teams Chat Files"),
        json!({"id":"folder"}),
    )
    .await;
    get(
        &server,
        &format!("{DRIVE}/root:/Microsoft Teams Chat Files:/permissions"),
        grants(&[A]),
    )
    .await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&server)
        .await;
    let write = DriveWrite::upload(&site(), "100% #notes;.txt", b"abc".to_vec(), A).unwrap();
    let answer = transport(&server, true)
        .deliver_attachment(write)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(answer.status(), 201);
    let requests = writes(&server).await;
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].url.path().ends_with("100%25%20%23notes;.txt:/content"),
        "{}",
        requests[0].url
    );
    assert!(
        requests[0]
            .url
            .query_pairs()
            .any(|(key, value)| key == "@name.conflictBehavior" && value == "rename")
    );
    assert_eq!(requests[0].body, b"abc");
}

#[tokio::test]
async fn a_share_proves_actor_peers_and_existing_access_before_explicit_direct_grant() {
    let server = MockServer::start().await;
    identity(&server, A).await;
    let id = chat(&server, &[A, B]).await;
    owner(&server).await;
    permissions(&server, grants(&[A])).await;
    get(
        &server,
        "/v1.0/users/00000000-0000-4000-8000-00000000000b",
        json!({"id":B.strip_prefix("8:orgid:").unwrap(),"userPrincipalName":"b@lab.example.test"}),
    )
    .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let write = DriveWrite::share(&file(), &id, &[(B.into(), "b@lab.example.test".into())], A).unwrap();
    assert_eq!(
        transport(&server, true)
            .deliver_attachment(write)
            .await
            .unwrap()
            .unwrap()
            .status(),
        200
    );
    let requests = writes(&server).await;
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(
        body,
        json!({"recipients":[{"email":"b@lab.example.test"}],"roles":["write"],"requireSignIn":true,"sendInvitation":false})
    );
}

#[tokio::test]
async fn wrong_actor_mixed_chat_and_broad_existing_grants_never_write() {
    for fault in ["actor", "chat", "link", "outside", "upn"] {
        let server = MockServer::start().await;
        identity(&server, if fault == "actor" { X } else { A }).await;
        let id = chat(&server, if fault == "chat" { &[A, X] } else { &[A, B] }).await;
        owner(&server).await;
        let permission = if fault == "link" {
            json!({"value":[{"roles":["write"],"link":{"scope":"anonymous"}}]})
        } else if fault == "outside" {
            grants(&[A, X])
        } else {
            grants(&[A])
        };
        permissions(&server, permission).await;
        get(&server,"/v1.0/users/00000000-0000-4000-8000-00000000000b",json!({"id":B.strip_prefix("8:orgid:").unwrap(),"userPrincipalName":if fault=="upn" {"external@outside.test"} else {"b@lab.example.test"}})).await;
        let write = DriveWrite::share(&file(), &id, &[(B.into(), "b@lab.example.test".into())], A).unwrap();
        assert!(
            transport(&server, true)
                .deliver_attachment(write)
                .await
                .unwrap()
                .is_err(),
            "{fault}"
        );
        assert!(writes(&server).await.is_empty(), "{fault}");
    }
}

#[tokio::test]
async fn an_old_outbox_file_is_rechecked_and_personal_transport_stays_unrestricted() {
    let server = MockServer::start().await;
    identity(&server, A).await;
    let id = chat(&server, &[A, B]).await;
    owner(&server).await;
    permissions(&server, grants(&[A, X])).await;
    let text = MessageText::plain("draft").unwrap().with_files(vec![file()]);
    let message = SendMessage::new(&id, Author::new(Mri::new(A), "A"), text);
    assert!(transport(&server, true).preflight_lab_write(&message).await.is_err());
    assert!(transport(&server, false).preflight_lab_write(&message).await.is_ok());
    assert!(writes(&server).await.is_empty());
}

#[tokio::test]
async fn staging_refuses_shared_folders_and_read_only_never_requests_credentials_or_network() {
    let server = MockServer::start().await;
    identity(&server, A).await;
    owner(&server).await;
    get(
        &server,
        &format!("{DRIVE}/root:/Microsoft Teams Chat Files"),
        json!({"id":"folder"}),
    )
    .await;
    get(
        &server,
        &format!("{DRIVE}/root:/Microsoft Teams Chat Files:/permissions"),
        grants(&[A, B]),
    )
    .await;
    let write = DriveWrite::upload(&site(), "demo.txt", vec![1], A).unwrap();
    assert!(
        transport(&server, true)
            .deliver_attachment(write.clone())
            .await
            .unwrap()
            .is_err()
    );
    assert!(writes(&server).await.is_empty());
    server.reset().await;
    assert!(
        transport(&server, true)
            .read_only()
            .deliver_attachment(write)
            .await
            .unwrap()
            .is_err()
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_refused_or_uncertain_upload_is_not_replayed_by_the_transport() {
    for status in [401, 429, 500, 503] {
        let server = MockServer::start().await;
        owner(&server).await;
        get(
            &server,
            &format!("{DRIVE}/root:/Microsoft Teams Chat Files"),
            json!({"id":"folder"}),
        )
        .await;
        get(
            &server,
            &format!("{DRIVE}/root:/Microsoft Teams Chat Files:/permissions"),
            grants(&[A]),
        )
        .await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(status))
            .mount(&server)
            .await;
        let write = DriveWrite::upload(&site(), "demo.txt", vec![1], A).unwrap();
        assert_eq!(
            transport(&server, false)
                .deliver_attachment(write)
                .await
                .unwrap()
                .unwrap()
                .status(),
            status
        );
        assert_eq!(writes(&server).await.len(), 1);
    }
}

const TEAM: &str = "19:synthetic-team@thread.tacv2";
const CHANNEL: &str = "19:synthetic-channel@thread.tacv2";
async fn channel_context(server: &MockServer, members: &[&str], count: usize, selected: &str) {
    identity(server, A).await;
    get(server, "/api/csa/amer/api/v3/teams/users/me", json!({"metadata":{"isPartialData":false},"teams":[{"id":TEAM,"displayName":"Synthetic lab","teamSiteInformation":{"groupId":"group"},"channels":[{"id":selected,"displayName":"Synthetic channel","channelType":0}]}]})).await;
    get(
        server,
        &format!("/api/chatsvc/amer/v1/threads/{TEAM}"),
        json!({"id":TEAM,"members":[{"id":A}]}),
    )
    .await;
    get(
        server,
        &format!("/api/chatsvc/amer/v1/threads/{TEAM}/members"),
        json!({"isDeleted":false,"totalMemberCount":count,"members":members.iter().map(|id|json!({"id":id})).collect::<Vec<_>>()}),
    )
    .await;
}
fn channel_write() -> crate::TeamWrite {
    crate::TeamWrite::Channel(crate::ChannelWrite {
        target: crate::ChannelTarget {
            team: TEAM.into(),
            team_group: "group".into(),
            channel: CHANNEL.into(),
            channel_group: None,
            mailbox: None,
        },
        action: crate::ChannelChange::Delete,
    })
}
#[tokio::test]
async fn channel_admin_requires_fresh_whole_roster_and_exact_selected_channel() {
    for (members, count, selected, allowed) in [
        (vec![A, B], 2, CHANNEL, true),
        (vec![A, X], 2, CHANNEL, false),
        (vec![A, "28:synthetic-bot"], 2, CHANNEL, false),
        (vec![A], 2, CHANNEL, false),
        (vec![A, B], 2, "19:other@thread.tacv2", false),
    ] {
        let server = MockServer::start().await;
        channel_context(&server, &members, count, selected).await;
        let transport = transport(&server, true)
            .with_csa("https://teams.microsoft.com/api/csa/amer")
            .unwrap();
        assert_eq!(transport.preflight_lab_write(&channel_write()).await.is_ok(), allowed);
        assert!(writes(&server).await.is_empty());
    }
}
#[tokio::test]
async fn channel_admin_checks_added_member_without_a_personal_restriction() {
    let server = MockServer::start().await;
    channel_context(&server, &[A, B], 2, CHANNEL).await;
    let transport = transport(&server, true)
        .with_csa("https://teams.microsoft.com/api/csa/amer")
        .unwrap();
    let mut write = channel_write();
    if let crate::TeamWrite::Channel(channel) = &mut write {
        channel.action = crate::ChannelChange::AddMember {
            mri: X.into(),
            owner: false,
        };
    }
    assert!(transport.preflight_lab_write(&write).await.is_err());
    assert!(
        super::attachment_tests::transport(&server, false)
            .preflight_lab_write(&write)
            .await
            .is_ok()
    );
    assert!(writes(&server).await.is_empty());
}

#[tokio::test]
async fn channel_admin_refuses_a_changed_actor_even_with_an_exact_lab_roster() {
    let server = MockServer::start().await;
    channel_context(&server, &[A, B], 2, CHANNEL).await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"id":X.strip_prefix("8:orgid:").unwrap(),"userPrincipalName":"outside@example.test"}),
        ))
        .with_priority(1)
        .mount(&server)
        .await;
    let transport = transport(&server, true)
        .with_csa("https://teams.microsoft.com/api/csa/amer")
        .unwrap();
    assert!(transport.preflight_lab_write(&channel_write()).await.is_err());
    assert!(writes(&server).await.is_empty());
}

#[tokio::test]
async fn channel_own_preferences_send_once_after_actor_proof_without_contacting_a_team() {
    let server = MockServer::start().await;
    identity(&server, A).await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let request = crate::OwnTeamView::ChannelProperty {
        channel: CHANNEL.into(),
        property: crate::ChannelOwnProperty::Notifications,
        value: "{\"allNewPosts\":true}".into(),
    };
    assert_eq!(
        transport(&server, true).channel_own(&request).await.unwrap().status(),
        200
    );
    assert_eq!(writes(&server).await.len(), 1);
    let reads = server.received_requests().await.unwrap();
    assert!(!reads.iter().any(|request| request.url.path().contains("/threads/")));
}

#[tokio::test]
async fn channel_own_refuses_changed_actor_other_interest_account_and_read_only() {
    let request = crate::OwnTeamView::ChannelInterest {
        channel: CHANNEL.into(),
        own: B.into(),
        shown: false,
    };
    let server = MockServer::start().await;
    identity(&server, X).await;
    assert!(transport(&server, true).channel_own(&request).await.is_err());
    let request = crate::OwnTeamView::ChannelFavorite {
        channel: CHANNEL.into(),
        general: false,
        shown: false,
    };
    assert!(transport(&server, true).channel_own(&request).await.is_err());
    assert!(
        transport(&server, true)
            .read_only()
            .channel_own(&request)
            .await
            .is_err()
    );
    assert!(writes(&server).await.is_empty());
}

#[tokio::test]
async fn channel_own_folder_conflict_is_returned_without_replaying_a_create() {
    let server = MockServer::start().await;
    identity(&server, A).await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(409))
        .mount(&server)
        .await;
    let request = crate::ChannelSectionWrite::new(
        &crate::ChannelSectionAction::Create {
            channel: CHANNEL.into(),
            name: "Synthetic section".into(),
        },
        &crate::ChannelSections {
            version: 7,
            sections: Vec::new(),
        },
    )
    .unwrap();
    let transport = transport(&server, true)
        .with_csa("https://teams.microsoft.com/api/csa/amer")
        .unwrap();
    assert_eq!(transport.channel_own(&request).await.unwrap().status(), 409);
    assert_eq!(writes(&server).await.len(), 1);
}

#[tokio::test]
async fn private_channel_admin_proves_both_parent_and_channel_audience() {
    for (parent, channel, allowed) in [
        (vec![A, B], vec![A, B], true),
        (vec![A, X], vec![A, B], false),
        (vec![A, B], vec![A, X], false),
    ] {
        let server = MockServer::start().await;
        channel_context(&server, &parent, parent.len(), CHANNEL).await;
        Mock::given(method("GET")).and(path("/api/csa/amer/api/v3/teams/users/me"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"metadata":{"isPartialData":false},"teams":[{"id":TEAM,"displayName":"Synthetic team","tenantId":"tenant","teamSiteInformation":{"groupId":"group"},"channels":[{"id":CHANNEL,"displayName":"Synthetic private channel","channelType":1,"substrateGroupId":"private-group"}]}]})))
            .with_priority(1).mount(&server).await;
        get(
            &server,
            &format!("/api/chatsvc/amer/v1/threads/{CHANNEL}"),
            json!({"id":CHANNEL}),
        )
        .await;
        get(&server,&format!("/api/chatsvc/amer/v1/threads/{CHANNEL}/members"),json!({"isDeleted":false,"totalMemberCount":channel.len(),"members":channel.iter().map(|mri|json!({"id":mri})).collect::<Vec<_>>()})).await;
        let mut write = channel_write();
        if let crate::TeamWrite::Channel(write) = &mut write {
            write.target.channel_group = Some("OID:private-group@tenant".into());
        }
        let transport = transport(&server, true)
            .with_csa("https://teams.microsoft.com/api/csa/amer")
            .unwrap();
        assert_eq!(transport.preflight_lab_write(&write).await.is_ok(), allowed);
        assert!(writes(&server).await.is_empty());
    }
}
