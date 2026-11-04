use super::*;
use serde_json::json;

#[test]
fn credentials_never_escape_the_original_validated_chat_service() {
    for url in [
        "https://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/conversations",
        "https://emea.ng.msg.teams.microsoft.com/v1/threads/x",
    ] {
        for headers in [
            json!({"Authorization":"Bearer synthetic"}),
            json!({"Authentication":"skypetoken=synthetic"}),
            json!({"X-Skypetoken":"synthetic"}),
        ] {
            let captured = credential(&json!({"url":url,"headers":headers})).unwrap();
            assert!(captured.base.as_str().ends_with('/'));
            assert!(captured.base.query().is_none());
        }
    }
    for url in [
        "http://teams.microsoft.com/api/chatsvc/emea/v1/x",
        "https://teams.microsoft.com.evil.example/api/chatsvc/emea/v1/x",
        "https://evil.example/v1/x",
        "https://u:p@teams.microsoft.com/api/chatsvc/emea/v1/x",
        "https://teams.microsoft.com:8443/api/chatsvc/emea/v1/x",
        "https://teams.microsoft.com/api/chatsvc/../v1/x",
        "https://teams.microsoft.com/api/chatsvc/emea/x",
    ] {
        assert!(
            credential(&json!({"url":url,"headers":{"Authorization":"Bearer synthetic"}})).is_none(),
            "{url}"
        );
    }
    assert!(credential(&json!({"url":"https://teams.microsoft.com/api/chatsvc/emea/v1/x", "headers":{"Authorization":"Bearer secret\nvalue"}})).is_none());
}

fn config(root: &std::path::Path) -> LabConfig {
    use std::fmt::Write as _;
    let mut text = String::new();
    for (index, alias) in ["a", "b", "c"].into_iter().enumerate() {
        writeln!(text,
            "[[accounts]]\nalias=\"{alias}\"\nupn=\"bot-{alias}@lab.example.com\"\nmri=\"8:orgid:00000000-0000-4000-8000-{:012}\"", index + 1).unwrap();
    }
    LabConfig::parse(&text, root).unwrap()
}

#[tokio::test]
async fn observed_creator_identity_must_match_the_configured_actor_until_navigation() {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let dir = tempfile::tempdir().unwrap();
    let source = ActionTargets::new(&config(dir.path()), Alias::A).unwrap();
    let request = |oid: &str, upn: &str| Event {
        method: "Network.requestWillBeSent".to_owned(),
        session_id: None,
        params: json!({"request": {
            "url":"https://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/conversations",
            "headers":{"Authorization":format!("Bearer synthetic.{}.synthetic", URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&json!({"oid":oid,"upn":upn,"tid":"synthetic-tenant"})).unwrap()))}
        }}),
    };
    let own = "00000000-0000-4000-8000-000000000001";
    source.observe(&request(own, "BOT-A@LAB.EXAMPLE.COM"));
    assert!(source.observations.lock().unwrap().identity);
    assert!(!source.observations.lock().unwrap().mismatch);
    source.observe(&request(own, "different@lab.example.com"));
    source.observe(&request(own, "bot-a@lab.example.com"));
    assert!(
        source
            .recipients("19:declared@thread.v2")
            .await
            .unwrap_err()
            .contains("identity")
    );
    source.observe(&Event {
        method: "Page.frameNavigated".to_owned(),
        session_id: None,
        params: json!({"frame":{"id":"new-document"}}),
    });
    source.observe(&request(
        "00000000-0000-4000-8000-000000000002",
        "bot-a@lab.example.com",
    ));
    assert!(!source.observations.lock().unwrap().identity);
    assert!(source.observations.lock().unwrap().mismatch);
}

#[tokio::test]
async fn unknown_identity_and_credential_fail_before_any_network_read() {
    let dir = tempfile::tempdir().unwrap();
    let source = ActionTargets::new(&config(dir.path()), Alias::A).unwrap();
    assert!(
        source
            .recipients("19:any@thread.v2")
            .await
            .unwrap_err()
            .contains("identity")
    );
    source.observations.lock().unwrap().identity = true;
    assert!(
        source
            .recipients("19:any@thread.v2")
            .await
            .unwrap_err()
            .contains("credential")
    );
    source.observe(&Event {
        method: "Page.frameNavigated".to_owned(),
        session_id: None,
        params: json!({"frame":{"id":"fresh-document"}}),
    });
    assert!(!source.observations.lock().unwrap().identity);
    source.observations.lock().unwrap().mismatch = true;
    assert!(
        source
            .recipients("19:any@thread.v2")
            .await
            .unwrap_err()
            .contains("identity")
    );
}

#[tokio::test]
async fn fresh_members_match_declared_participants_without_a_global_roster() {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let conf = config(dir.path());
    let own = conf.account(Alias::A).mri.as_ref().unwrap().clone();
    let personal = "8:orgid:00000000-0000-4000-8000-0000000000f0".to_owned();
    let bot = "28:synthetic-echo".to_owned();
    let chat = "19:declared@thread.v2";
    let mut source = ActionTargets::new(&conf, Alias::A).unwrap();
    source.client = Client::builder()
        .no_proxy()
        .redirect(redirect::Policy::none())
        .build()
        .unwrap();
    source
        .declare(chat, &[own.clone(), personal.clone(), bot.clone()])
        .unwrap();
    {
        let mut seen = source.observations.lock().unwrap();
        seen.identity = true;
        seen.credential = Some(Credential {
            base: Url::parse(&format!("{}/", server.uri())).unwrap(),
            header: "Authorization",
            value: Zeroizing::new("Bearer synthetic".to_owned()),
        });
    }
    let answer = |members: Vec<String>| {
        json!({"isDeleted":false,"totalMemberCount":members.len(),
        "members":members.into_iter().map(|id|json!({"id":id})).collect::<Vec<_>>()})
    };
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(answer(vec![own.clone(), personal.clone(), bot])))
        .mount(&server)
        .await;
    source.recipients(chat).await.unwrap();
    server.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(answer(vec![own, personal])))
        .mount(&server)
        .await;
    assert!(source.recipients(chat).await.unwrap_err().contains("intended target"));
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "each action rereads its target"
    );
}

#[tokio::test]
async fn explicit_synthetic_call_evidence_requires_declared_conversation_and_actual_people() {
    use crate::scenario::test_page::{Page, evaluated};
    use std::sync::Arc;
    let mut page = Page::new(|_| Ok(evaluated(Value::Null))).await;
    let dir = tempfile::tempdir().unwrap();
    let mut session = page.session(dir.path(), false);
    assert!(
        session
            .driver()
            .preflight_recipients("no context", "19:group@thread.v2")
            .await
            .is_err()
    );
    let own = "8:orgid:00000000-0000-4000-8000-000000000001".to_owned();
    let peer = "8:orgid:00000000-0000-4000-8000-000000000002".to_owned();
    let expected = vec![own.clone(), peer.clone()];
    let reply = |id: &str| json!({"isDeleted":false,"totalMemberCount":2,"members":[{"id":own},{"id":id}]});
    let source = ActionTargets::synthetic(
        &own,
        [
            ("19:group@thread.v2".to_owned(), expected.clone()),
            ("19:changed@thread.v2".to_owned(), expected),
        ]
        .into_iter()
        .collect(),
        [
            ("19:group@thread.v2".to_owned(), reply(&peer)),
            (
                "19:changed@thread.v2".to_owned(),
                reply("8:orgid:00000000-0000-4000-8000-0000000000f0"),
            ),
        ]
        .into_iter()
        .collect(),
    )
    .unwrap();
    session.set_action_targets(Arc::new(source));
    let mut driver = session.driver();
    driver
        .preflight_recipients("declared", "19:group@thread.v2")
        .await
        .unwrap();
    assert!(
        driver
            .preflight_recipients("changed", "19:changed@thread.v2")
            .await
            .is_err()
    );
    assert!(
        driver
            .preflight_recipients("missing", "19:unknown@thread.v2")
            .await
            .is_err()
    );
}

#[test]
fn declarations_are_operation_specific_and_require_distinct_people_including_the_actor() {
    let dir = tempfile::tempdir().unwrap();
    let conf = config(dir.path());
    let own = conf.account(Alias::A).mri.as_ref().unwrap();
    let peer = conf.account(Alias::B).mri.as_ref().unwrap();
    let source = ActionTargets::new(&conf, Alias::A).unwrap();
    for (chat, members) in [
        ("", vec![own.clone(), peer.clone()]),
        ("19:x@thread.v2", vec![own.clone()]),
        ("19:x@thread.v2", vec![peer.clone(), peer.clone()]),
        ("19:x@thread.v2", vec![own.clone(), String::new()]),
    ] {
        assert!(source.declare(chat, &members).is_err());
    }
    source.declare("19:x@thread.v2", &[own.clone(), peer.clone()]).unwrap();
}
