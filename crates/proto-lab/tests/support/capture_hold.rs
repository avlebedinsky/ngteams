//! Offline capture experiments and redirect handling through the real CDP command connection.
use super::*;
use crate::scenario::test_page::Page;
fn event(method: &str, url: &str, body: Option<&str>) -> Event {
    let mut request = json!({"method":method,"url":url});
    if let Some(body) = body {
        request["postData"] = json!(body);
    }
    Event {
        session_id: Some(crate::cdp::SessionId::new("synthetic-page")),
        method: "Fetch.requestPaused".into(),
        params: json!({"requestId":"pause-1","networkId":"network-1","request":request}),
    }
}
#[test]
fn capture_rules_match_method_url_and_optional_body_conditions() {
    let rule = Hold::new("post", r"/objects")
        .unwrap()
        .except("/objects/read")
        .unwrap()
        .with_body_containing("shape")
        .with_body_lacking("sent")
        .labelled("synthetic hold");
    assert_eq!(rule.label(), Some("synthetic hold"));
    for (method, url, body, expected) in [
        ("POST", "https://synthetic.example/objects", Some("shape"), true),
        ("PUT", "https://synthetic.example/objects", Some("shape"), false),
        ("POST", "https://synthetic.example/objects/read", Some("shape"), false),
        ("POST", "https://synthetic.example/objects", Some("shape sent"), false),
        ("POST", "https://synthetic.example/objects", None, false),
    ] {
        let request = PausedRequest::from_params(&event(method, url, body).params).unwrap();
        assert_eq!(rule.matches(&request), expected);
    }
    assert!(Hold::new("POST", "[").is_err());
    assert!(Hold::new("POST", ".").unwrap().except("[").is_err());
    assert!(PausedRequest::from_params(&json!({})).is_none());
    assert_eq!(
        HeldRequest::new("POST", "https://synthetic.example/x?token=synthetic#fragment").url,
        "https://synthetic.example/x"
    );
}
#[tokio::test]
async fn only_explicit_holds_fail_and_every_other_mutation_is_continued_without_a_log() {
    let mut peer = Page::new(|_| Ok(json!({}))).await;
    let dir = tempfile::tempdir().unwrap();
    let session = peer.session(dir.path(), false);
    let capture = CaptureHold::new();
    let mutation = event(
        "POST",
        "https://unknown.synthetic.example/write?token=synthetic",
        Some("synthetic body"),
    );
    assert_eq!(
        capture.handle(session.cdp(), &mutation).await.unwrap(),
        Handled::Continued
    );
    capture.hold(Hold::new("POST", "/write").unwrap().labelled("shape"));
    assert_eq!(capture.handle(session.cdp(), &mutation).await.unwrap(), Handled::Held);
    let held = capture.held_requests();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].label.as_deref(), Some("shape"));
    assert_eq!(held[0].network_id.as_deref(), Some("network-1"));
    assert!(!held[0].url.contains("?token"));
    capture.clear();
    assert_eq!(
        capture.handle(session.cdp(), &mutation).await.unwrap(),
        Handled::Continued
    );
    let commands = peer.commands();
    assert_eq!(
        commands
            .iter()
            .map(|command| command["method"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["Fetch.continueRequest", "Fetch.failRequest", "Fetch.continueRequest"]
    );
    assert!(commands.iter().all(|command| command["sessionId"] == "synthetic-page"));
    assert!(!dir.path().join("fence.log").exists());
}
#[tokio::test]
async fn redirects_are_caught_before_following_and_other_responses_continue() {
    let mut peer = Page::new(|_| Ok(json!({}))).await;
    let dir = tempfile::tempdir().unwrap();
    let session = peer.session(dir.path(), false);
    let (catch, mut caught) =
        RedirectCatch::new("https://login.synthetic.example/authorize", "synthetic://callback").unwrap();
    let capture = CaptureHold::new().with_redirect_catch(catch);
    let mut response = event("GET", "https://login.synthetic.example/authorize", None);
    response.params["responseStatusCode"] = json!(302);
    response.params["responseHeaders"] =
        json!([{"name":"Location","value":"synthetic://callback?code=synthetic-code"}]);
    assert_eq!(
        capture.handle(session.cdp(), &response).await.unwrap(),
        Handled::RedirectCaught
    );
    assert!(caught.try_recv().unwrap().expose().contains("synthetic-code"));
    assert!(capture.held_requests().is_empty());
    response.params["responseHeaders"] = json!([{"name":"Location","value":"/another"}]);
    assert_eq!(
        capture.handle(session.cdp(), &response).await.unwrap(),
        Handled::ResponseContinued
    );
    response.params.as_object_mut().unwrap().remove("responseStatusCode");
    response.params["responseErrorReason"] = json!("Failed");
    assert_eq!(
        capture.handle(session.cdp(), &response).await.unwrap(),
        Handled::ResponseContinued
    );
    assert_eq!(
        capture
            .handle(session.cdp(), &event("GET", "synthetic://callback?code=another", None))
            .await
            .unwrap(),
        Handled::RedirectCaught
    );
    let mut empty = event("GET", "https://synthetic.example", None);
    empty.params.as_object_mut().unwrap().remove("requestId");
    assert_eq!(capture.handle(session.cdp(), &empty).await.unwrap(), Handled::Continued);
}
#[tokio::test]
async fn a_refused_hold_surfaces_the_cdp_error() {
    let mut peer = Page::new(|_| Err("synthetic refusal".into())).await;
    let dir = tempfile::tempdir().unwrap();
    let session = peer.session(dir.path(), false);
    let capture = CaptureHold::new();
    capture.hold(Hold::new("POST", ".").unwrap());
    assert!(
        capture
            .handle(session.cdp(), &event("POST", "https://synthetic.example", None))
            .await
            .is_err()
    );
    assert!(capture.held_requests().is_empty());
}
