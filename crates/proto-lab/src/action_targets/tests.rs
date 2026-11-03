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

#[tokio::test]
async fn unknown_identity_and_credential_fail_before_any_network_read() {
    let text = ["a","b","c"].map(|id|format!("[[accounts]]\nalias=\"{id}\"\nupn=\"bot-{id}@lab.example.com\"\nmri=\"8:orgid:00000000-0000-4000-8000-00000000000{id}\"\n")).concat();
    let dir = tempfile::tempdir().unwrap();
    let config = LabConfig::parse(&text, dir.path()).unwrap();
    let targets = ActionTargets::new(&config, Alias::A).unwrap();
    assert_eq!(
        targets.recipients("19:any@thread.v2").await.unwrap_err(),
        TargetError::Identity.to_string()
    );
    targets.observations.lock().unwrap().identity = true;
    assert!(
        targets
            .recipients("19:any@thread.v2")
            .await
            .unwrap_err()
            .contains("credential")
    );
    targets.observe(&Event {
        method: "Page.frameNavigated".to_owned(),
        session_id: None,
        params: json!({"frame":{"id":"fresh-document"}}),
    });
    assert!(
        !targets.observations.lock().unwrap().identity,
        "identity evidence never carries across documents"
    );
    targets.observations.lock().unwrap().mismatch = true;
    assert_eq!(
        targets.recipients("19:any@thread.v2").await.unwrap_err(),
        TargetError::Identity.to_string()
    );
}

#[tokio::test]
async fn every_action_rereads_members_and_rejects_a_new_personal_or_mixed_target() {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};
    let server = MockServer::start().await;
    let text = ["a","b","c"].map(|id|format!("[[accounts]]\nalias=\"{id}\"\nupn=\"bot-{id}@lab.example.com\"\nmri=\"8:orgid:00000000-0000-4000-8000-00000000000{id}\"\n")).concat();
    let dir = tempfile::tempdir().unwrap();
    let config = LabConfig::parse(&text, dir.path()).unwrap();
    let mut targets = ActionTargets::new(&config, Alias::A).unwrap();
    // Only this private unit test can replace the read-only client's origin/scheme.
    targets.client = Client::builder()
        .no_proxy()
        .redirect(redirect::Policy::none())
        .build()
        .unwrap();
    {
        let mut seen = targets.observations.lock().unwrap();
        seen.identity = true;
        seen.credential = Some(Credential {
            base: Url::parse(&format!("{}/", server.uri())).unwrap(),
            header: "Authorization",
            value: Zeroizing::new("Bearer synthetic".to_owned()),
        });
    }
    let a = config.account(Alias::A).mri.as_deref().unwrap();
    let b = config.account(Alias::B).mri.as_deref().unwrap();
    let expected = teams_lab_targets::one_to_one_id(a, b).unwrap();
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":expected,"members":[{"id":a},{"id":b}]})))
        .mount(&server)
        .await;
    targets.recipients(&expected).await.unwrap();
    server.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"id":expected,"members":[{"id":a},{"id":"8:orgid:00000000-0000-4000-8000-0000000000f0"}]}),
        ))
        .mount(&server)
        .await;
    assert!(targets.recipients(&expected).await.is_err());
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "second action must perform a fresh read"
    );
    server.reset().await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"isDeleted":false,"totalMemberCount":3,"members":[{"id":a},{"id":b},{"id":"8:orgid:00000000-0000-4000-8000-0000000000f0"}]}))).mount(&server).await;
    assert!(targets.recipients("19:group@thread.v2").await.is_err());
}

#[tokio::test]
async fn explicit_synthetic_sources_still_verify_full_recipients_and_missing_context_never_authorizes() {
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
    let accounts = ["a", "b", "c"].map(|id| LabAccount {
        mri: format!("8:orgid:00000000-0000-4000-8000-00000000000{id}"),
        upn: format!("bot-{id}@lab.example.com"),
    });
    let own = accounts[0].mri.clone();
    let replies = [("19:group@thread.v2".to_owned(), json!({"isDeleted":false,"totalMemberCount":3,"members":accounts.iter().map(|account|json!({"id":account.mri})).collect::<Vec<_>>()})),
        ("19:mixed@thread.v2".to_owned(), json!({"isDeleted":false,"totalMemberCount":2,"members":[{"id":own},{"id":"8:orgid:00000000-0000-4000-8000-0000000000f0"}]}))].into_iter().collect();
    session.set_action_targets(Arc::new(
        ActionTargets::synthetic(&own, LabRoster::new(accounts).unwrap(), replies).unwrap(),
    ));
    let mut driver = session.driver();
    driver.preflight_recipients("lab", "19:group@thread.v2").await.unwrap();
    assert!(
        driver
            .preflight_recipients("mixed", "19:mixed@thread.v2")
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

#[tokio::test]
async fn unverified_service_and_free_form_scenarios_fail_before_opening_a_browser() {
    use crate::scenario::{RunOptions, ScenarioName, ab_call::CallStep};
    let dir = tempfile::tempdir().unwrap();
    let paths = crate::config::LabPaths::new(dir.path().to_owned());
    for scenario in [
        ScenarioName::EchoCall,
        ScenarioName::AbFile,
        ScenarioName::AbImage,
        ScenarioName::TeamManage,
        ScenarioName::MeetingSteps,
        ScenarioName::AbMeeting,
        ScenarioName::AbMeetingJoin,
        ScenarioName::ChannelCreate,
    ] {
        let error = crate::scenario::run(
            scenario,
            &paths,
            RunOptions {
                display: crate::browser::Display::Headless,
                load_timeout: Duration::from_secs(1),
                call: CallStep::Answer,
            },
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("independently of the fence"), "{error}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
