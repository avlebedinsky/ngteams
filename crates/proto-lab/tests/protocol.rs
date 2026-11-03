//! Synthetic current and historical captures retain the same observable request/call facts.
use proto_lab::protocol::{chat_service_path, http_requests, log_url, one_to_one_id, sent_frames};
use serde_json::json;
#[test]
fn chat_helpers_read_protocol_identifiers_without_a_policy() {
    let a = "8:orgid:00000000-0000-4000-8000-00000000000a";
    let b = "8:orgid:00000000-0000-4000-8000-00000000000b";
    assert_eq!(one_to_one_id(a, b), one_to_one_id(b, a));
    assert!(one_to_one_id(a, a).is_none());
    assert!(one_to_one_id("not an MRI", b).is_none());
    let url =
        url::Url::parse("https://teams.microsoft.com/api/chatsvc/amer/v1/threads/19%3Asynthetic%40thread.v2").unwrap();
    assert_eq!(
        chat_service_path(&url),
        Some(vec!["threads".into(), "19:synthetic@thread.v2".into()])
    );
    for value in [
        "http://teams.microsoft.com/api/chatsvc/amer/v1/threads",
        "https://teams.microsoft.com.example.test/api/chatsvc/amer/v1/threads",
        "https://user:password@teams.microsoft.com/api/chatsvc/amer/v1/threads",
    ] {
        assert!(chat_service_path(&url::Url::parse(value).unwrap()).is_none());
    }
}
#[test]
fn capture_notes_redact_url_credentials() {
    let shown = log_url(
        "https://user:synthetic@synthetic.example/skypetoken/synthetic-secret/path?access_token=synthetic-token#synthetic-fragment",
    );
    assert!(!shown.contains("synthetic-secret"));
    assert!(!shown.contains("synthetic-token"));
    assert!(!shown.contains("synthetic-fragment"));
    assert!(!shown.contains("user:"));
    assert!(shown.contains("[redacted]"));
}
#[test]
fn held_requests_override_their_network_start_without_hiding_other_sends() {
    let sent = |id: &str| {
        json!({"type":"cdp","method":"Network.requestWillBeSent","params":{"requestId":id,"request":{"method":"POST","url":"https://synthetic.example/call"}}}).to_string()
    };
    let hold =
        json!({"type":"held_request","network_id":"held","method":"POST","url":"https://synthetic.example/call"})
            .to_string();
    for text in [
        format!("{}\n{hold}\n{}", sent("held"), sent("sent")),
        format!("{hold}\n{}\n{}", sent("sent"), sent("held")),
    ] {
        let requests = http_requests(&text);
        assert_eq!(requests.len(), 2);
        assert_eq!(requests.iter().filter(|request| request.held).count(), 1);
        assert_eq!(requests.iter().filter(|request| !request.held).count(), 1);
    }
}
#[test]
fn network_sends_take_precedence_when_historical_verdicts_are_also_present() {
    let created = json!({"type":"cdp","session":"page","method":"Network.webSocketCreated","params":{"requestId":"socket","url":"wss://synthetic.example"}});
    let sent = json!({"type":"cdp","session":"page","method":"Network.webSocketFrameSent","params":{"requestId":"socket","response":{"opcode":1,"payloadData":"3:::synthetic"}}});
    let old = json!({"type":"verdict","channel":"web_socket","verdict":"allow","url":"wss://synthetic.example","frame":{"data":"3:::synthetic"}});
    assert_eq!(sent_frames(&format!("{created}\n{sent}\n{old}")).len(), 1);
    assert_eq!(sent_frames(&old.to_string()).len(), 1);
    assert!(sent_frames(&format!("{created}\n{old}")).is_empty());
}
