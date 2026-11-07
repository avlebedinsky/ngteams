//! Synthetic current and historical captures retain the same observable request/call facts.
use std::io::Write as _;

use base64::Engine as _;
use proto_lab::protocol::{
    chat_service_path, delivery_of, http_requests, is_group_chat_id, log_url, one_to_one_id, sent_frames,
};
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

type TestResult = Result<(), Box<dyn std::error::Error>>;
fn chat_path(url: &str) -> Result<Option<Vec<String>>, url::ParseError> {
    Ok(chat_service_path(&url::Url::parse(url)?))
}
#[test]
fn chat_service_paths_take_regional_hosts_and_refuse_partial_or_foreign_shapes() -> TestResult {
    assert_eq!(
        chat_path("https://amer-02.msg.teams.microsoft.com/v1/users/ME/conversations")?,
        Some(vec!["users".into(), "ME".into(), "conversations".into()])
    );
    assert_eq!(
        chat_path("https://teams.cloud.microsoft/api/chatsvc/emea-2/v1/threads")?,
        Some(vec!["threads".into()])
    );
    for refused in [
        // the front door, cut short before its region or version
        "https://teams.microsoft.com/",
        "https://teams.microsoft.com/api",
        "https://teams.microsoft.com/api/chatsvc",
        "https://teams.microsoft.com/api/chatsvc/amer",
        "https://teams.microsoft.com/api/other/amer/v1/threads",
        "https://teams.microsoft.com/api/chatsvc/AMER/v1/threads",
        "https://teams.microsoft.com/api/chatsvc/amer/v2/threads",
        // a regional host: no name, a bad label, no version
        "https://msg.teams.microsoft.com/v1/threads",
        "https://am_er.msg.teams.microsoft.com/v1/threads",
        "https://amer.msg.teams.microsoft.com/",
        "https://amer.msg.teams.microsoft.com/v2/threads",
        // a port of its own
        "https://amer.msg.teams.microsoft.com:8443/v1/threads",
        // a segment that isn't UTF-8 once decoded
        "https://amer.msg.teams.microsoft.com/v1/threads/%FF",
    ] {
        assert_eq!(chat_path(refused)?, None, "{refused}");
    }
    Ok(())
}
#[test]
fn group_chat_ids_are_thread_ids_that_are_not_meetings() {
    assert!(is_group_chat_id("19:Synthetic_group-1@thread.v2"));
    for refused in [
        "19:@thread.v2",
        "19:meeting_synthetic@thread.v2",
        "19:synthetic@unq.gbl.spaces",
        "19:has space@thread.v2",
        "synthetic@thread.v2",
        &format!("19:{}@thread.v2", "a".repeat(129)),
    ] {
        assert!(!is_group_chat_id(refused), "{refused}");
    }
}
#[test]
fn logged_urls_keep_names_and_drop_values() {
    // nothing before a query or fragment, and nothing at all, name no URL
    assert_eq!(log_url("?synthetic=1"), "");
    assert_eq!(log_url("#synthetic"), "");
    assert_eq!(log_url(""), "");
    // a CONNECT target names only a host and port
    for authority in ["proxy.example.test:443", "[::1]:8443"] {
        assert_eq!(log_url(authority), authority);
    }
    // what isn't an authority and doesn't parse, or has no path, is not shown
    for unreadable in [
        "not a url",
        "host.example.test:",
        "host.example.test:123456",
        "mailto:someone@example.test",
    ] {
        assert_eq!(log_url(unreadable), "[unreadable url]", "{unreadable}");
    }
    assert_eq!(
        log_url("https://synthetic.example:8443/a/sig=synthetic-signature/b?keep=1&&eyJsynthetic=2"),
        "https://synthetic.example:8443/a/sig=[redacted]/b?keep&[redacted]"
    );
    let long = "A".repeat(100);
    assert_eq!(
        log_url(&format!("https://synthetic.example/{long}/next?{long}=1")),
        "https://synthetic.example/[redacted]/next?[redacted]"
    );
    // just short of an opaque run, or holding a character no token has, a segment stays
    let short = "A".repeat(99);
    assert_eq!(
        log_url(&format!("https://synthetic.example/{short}")),
        format!("https://synthetic.example/{short}")
    );
    let spaced = format!("{}%20", "A".repeat(100));
    assert_eq!(
        log_url(&format!("https://synthetic.example/{spaced}")),
        format!("https://synthetic.example/{spaced}")
    );
}
fn gzip(data: &[u8]) -> std::io::Result<String> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(data)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(encoder.finish()?))
}
fn frame(message: &serde_json::Value) -> String {
    format!("3:::{message}")
}
#[test]
fn deliveries_read_their_headers_in_any_case_and_their_body() {
    let message = json!({
        "id": 7,
        "url": "/v4/f/synthetic/messaging",
        "headers": {
            "TROUTER-REQUEST": "{\"id\":\"synthetic\"}",
            "x-microsoft-skype-chain-id": "synthetic-chain",
            "X-Microsoft-Skype-Message-ID": "synthetic-message",
            "X-Other": 1,
        },
        "body": "{\"syntheticBody\":true,\"n\":1.5,\"none\":null,\"escaped\":\"a\\\"b\"}",
    });
    let delivery = delivery_of("synthetic-connection", &frame(&message)).unwrap();
    assert_eq!(delivery.connection, "synthetic-connection");
    assert_eq!(delivery.id, 7);
    assert_eq!(delivery.path, "/v4/f/synthetic/messaging");
    assert_eq!(delivery.trouter_request.as_deref(), Some("{\"id\":\"synthetic\"}"));
    assert_eq!(delivery.chain_id.as_deref(), Some("synthetic-chain"));
    assert_eq!(delivery.message_id.as_deref(), Some("synthetic-message"));
    assert_eq!(delivery.body, message["body"].as_str().unwrap().as_bytes());
}
#[test]
fn gzipped_deliveries_are_decoded_within_their_bound() -> TestResult {
    let headers = json!({"X-Microsoft-Skype-Content-Encoding": "GZIP"});
    let body = b"{\"syntheticBody\":[true,false,null,-1,2.5]}";
    let message = json!({"id": 1, "url": "/synthetic", "headers": headers, "body": gzip(body)?});
    let delivery = delivery_of("", &frame(&message)).unwrap();
    assert_eq!(delivery.body, body);
    assert_eq!(delivery.trouter_request, None);
    // not base64, not gzip, or decoding past the bound: no delivery
    for bad in [
        "not base64!".to_owned(),
        base64::engine::general_purpose::STANDARD.encode(b"not gzip"),
        gzip(&vec![b'0'; 8 * 1024 * 1024 + 1])?,
    ] {
        let message = json!({"id": 1, "url": "/synthetic", "headers": headers, "body": bad});
        assert!(delivery_of("", &frame(&message)).is_none());
    }
    // exactly the bound is still a delivery
    let message =
        json!({"id": 1, "url": "/synthetic", "headers": headers, "body": gzip(&vec![b'0'; 8 * 1024 * 1024])?});
    assert_eq!(delivery_of("", &frame(&message)).unwrap().body.len(), 8 * 1024 * 1024);
    Ok(())
}
#[test]
fn frames_that_are_not_whole_deliveries_are_none() {
    let whole = json!({"id": 1, "url": "/synthetic", "headers": {}, "body": ""});
    assert!(delivery_of("", &frame(&whole)).is_some());
    let mut texts = vec![
        "1::".to_owned(),
        "3:::not json".to_owned(),
        "3:::{\"id\":1,\"ID\":2,\"url\":\"/synthetic\",\"headers\":{},\"body\":\"\"}".to_owned(),
    ];
    for missing in ["id", "url", "headers", "body"] {
        let mut message = whole.clone();
        message.as_object_mut().unwrap().remove(missing);
        texts.push(frame(&message));
    }
    for (field, wrong) in [
        ("id", json!("1")),
        ("url", json!(1)),
        ("headers", json!([])),
        ("body", json!(1)),
    ] {
        let mut message = whole.clone();
        message[field] = wrong;
        texts.push(frame(&message));
    }
    for text in texts {
        assert!(delivery_of("", &text).is_none(), "{text}");
    }
}
#[test]
fn capture_lines_that_are_not_requests_or_frames_are_passed_over() {
    let text = [
        "not json".to_owned(),
        json!({"type":"cdp","method":"Network.requestWillBeSent","params":{"request":{"method":"GET"}}}).to_string(),
        json!({"type":"verdict","channel":"http","verdict":"deny","method":"POST","url":"https://synthetic.example/denied"}).to_string(),
        json!({"type":"other"}).to_string(),
    ]
    .join("\n");
    let requests = http_requests(&text);
    assert_eq!(requests.len(), 1);
    assert!(requests[0].held);
    // a verdict without a network id is keyed by its line
    assert_eq!(requests[0].id, "entry-2");
    assert!(sent_frames("not json").is_empty());
    let binary = json!({"type":"cdp","method":"Network.webSocketFrameSent","params":{"requestId":"s","response":{"opcode":2,"payloadData":"AA=="}}});
    let no_data =
        json!({"type":"cdp","method":"Network.webSocketFrameSent","params":{"requestId":"s","response":{"opcode":1}}});
    assert!(sent_frames(&format!("{binary}\n{no_data}")).is_empty());
}
