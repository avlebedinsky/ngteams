//! Synthetic capture events and real local I/O failure paths; no service or tenant.
use super::*;
use crate::scenario::test_page::Page;

fn event(method: &str, params: Value) -> Event {
    Event {
        session_id: Some(SessionId::new("synthetic-page")),
        method: method.into(),
        params,
    }
}

fn entries(recorder: &Recorder) -> Vec<Value> {
    std::fs::read_to_string(recorder.events_file())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn write_failure_is_sticky_and_sync_failure_is_reported_without_losing_its_path() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::create(root.path(), "synthetic").unwrap();
    recorder.writer().file = OpenOptions::new().write(true).open("/dev/full").unwrap();
    recorder.record(json!({"synthetic": true}));
    assert_eq!(recorder.entries(), 1);
    recorder.record(json!({"must_not_be_written": true}));
    assert_eq!(recorder.entries(), 1);
    assert!(matches!(recorder.finish(), Err(CaptureError::Lost(_))));
    let recorder = Recorder::create(root.path(), "synthetic").unwrap();
    recorder.writer().file = OpenOptions::new().write(true).open("/dev/null").unwrap();
    assert!(matches!(recorder.finish(), Err(CaptureError::Io {path, ..}) if path == recorder.events_file()));
    assert_eq!(unix_ms(UNIX_EPOCH - Duration::from_secs(1)), 0);
    assert_eq!(
        capture_dir_name(UNIX_EPOCH - Duration::from_secs(1)),
        "19700101T000000Z"
    );
}

#[test]
fn occupied_capture_paths_and_exhausted_names_never_overwrite_existing_files() {
    let root = tempfile::tempdir().unwrap();
    let occupied = root.path().join("occupied");
    std::fs::write(&occupied, "synthetic sentinel").unwrap();
    assert!(Recorder::create(&occupied, "synthetic").is_err());
    assert!(unique_dir(&occupied, "synthetic").is_err());
    for index in 1..=1000 {
        let name = if index == 1 {
            "synthetic".to_owned()
        } else {
            format!("synthetic-{index}")
        };
        std::fs::create_dir(root.path().join(name)).unwrap();
    }
    assert!(
        matches!(unique_dir(root.path(), "synthetic"), Err(CaptureError::Io {source, ..}) if source.kind()==io::ErrorKind::AlreadyExists)
    );
    assert_eq!(std::fs::read_to_string(occupied).unwrap(), "synthetic sentinel");
}

#[test]
fn websocket_payload_kinds_and_malformed_requests_remain_distinct_in_the_capture() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Arc::new(Recorder::create(root.path(), "synthetic").unwrap());
    let capture = Capture::new(Arc::clone(&recorder), RtcHooks::default());
    let script = GuardScript::with_token("abcd").unwrap();
    for (index, kind) in ["text", "binary", "unreadable", "foreign"].into_iter().enumerate() {
        let payload = json!({"token":"abcd", "socket": 3, "seq":index+1, "url":"wss://synthetic.example/socket", "kind":kind,"data":"synthetic"}).to_string();
        capture.ws_verdict(
            &event("Runtime.bindingCalled", json!({"payload":payload})),
            &script,
            Verdict::Allow,
        );
    }
    capture.ws_verdict(
        &event("Runtime.bindingCalled", json!({"payload":"{"})),
        &script,
        Verdict::Deny(DenyCause::Malformed),
    );
    let rows = entries(&recorder);
    for (row, kind) in rows.iter().zip(["text", "binary", "unreadable", "foreign"]) {
        assert_eq!(row["frame"]["kind"], kind);
        assert_eq!(
            row["frame"]["data"],
            if matches!(kind, "text" | "binary") {
                "synthetic"
            } else {
                ""
            }
        );
    }
    assert_eq!(rows[4]["malformed"], true);
    assert_eq!(rows[4]["cause"], "malformed");
}

#[test]
fn paused_response_diagnostics_keep_stage_status_and_failure_but_no_request_body() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Arc::new(Recorder::create(root.path(), "synthetic").unwrap());
    let capture = Capture::new(Arc::clone(&recorder), RtcHooks::default());
    for response in [
        json!({}),
        json!({"responseStatusCode": 302}),
        json!({"responseErrorReason":"Failed"}),
    ] {
        let mut params = json!({"requestId":"pause", "request":{"method":"POST","url":"https://synthetic.example","postData":"synthetic body"}});
        params
            .as_object_mut()
            .unwrap()
            .extend(response.as_object().unwrap().clone());
        let paused = event("Fetch.requestPaused", params);
        capture.continued(&paused);
        capture.unanswered(&paused, "synthetic continuation failed");
    }
    let rows = entries(&recorder);
    assert_eq!(rows.len(), 6);
    assert_eq!(rows[0]["stage"], "request");
    assert_eq!(rows[2]["stage"], "response");
    assert_eq!(rows[4]["stage"], "response");
    assert_eq!(rows[3]["status"], 302);
    assert_eq!(rows[5]["response_error"], "Failed");
    assert_eq!(rows[5]["error"], "synthetic continuation failed");
    assert!(
        rows.iter()
            .all(|row| row.get("request").is_none() && row.get("postData").is_none())
    );
}

#[tokio::test]
async fn body_capture_records_cdp_errors_and_nonobject_answers_and_skips_code_and_oversize_bodies() {
    let mut page = Page::new(|command| {
        assert!(matches!(
            command["method"].as_str(),
            Some("Network.getResponseBody" | "Network.getRequestPostData")
        ));
        match command["params"]["requestId"].as_str().unwrap() {
            "error" => Err("synthetic body no longer available".into()),
            "empty" => Ok(Value::Null),
            _ => Ok(json!({"body":"synthetic response","postData":"synthetic request","base64Encoded":false})),
        }
    })
    .await;
    let root = tempfile::tempdir().unwrap();
    let session = page.session(root.path(), false);
    let recorder = Arc::new(Recorder::create(root.path(), "synthetic").unwrap());
    let capture = Capture::new(Arc::clone(&recorder), RtcHooks::default());
    response_events(&capture, session.cdp());
    capture.observe(
        session.cdp(),
        &event(
            "Network.requestWillBeSent",
            json!({"requestId":"request","request":{"hasPostData":true}}),
        ),
    );
    capture.observe(session.cdp(), &event("Network.loadingFinished", json!({})));
    capture.observe(
        session.cdp(),
        &event(
            "Runtime.bindingCalled",
            json!({"name":RTC_BINDING,"payload":"invalid synthetic JSON"}),
        ),
    );
    capture.observe(
        session.cdp(),
        &event(
            "Runtime.bindingCalled",
            json!({"name":RTC_BINDING,"payload":json!({"pc":1})}),
        ),
    );
    capture.observe(
        session.cdp(),
        &event("Target.detachedFromTarget", json!({"sessionId":"synthetic"})),
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        while entries(&recorder)
            .iter()
            .filter(|row| matches!(row["type"].as_str(), Some("response_body" | "request_body")))
            .count()
            < 3
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let rows = entries(&recorder);
    let error = rows.iter().find(|row| row["request_id"] == "error").unwrap();
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .contains("synthetic body no longer available")
    );
    let empty = rows.iter().find(|row| row["request_id"] == "empty").unwrap();
    assert!(empty.get("body").is_none() && empty.get("error").is_none());
    assert_eq!(page.commands().len(), 3);
    assert!(rows.iter().any(|row| row["payload"] == "invalid synthetic JSON"));
    assert!(rows.iter().any(|row| row["payload"]["pc"] == 1));
}

#[test]
fn denied_request_bodies_are_optional_and_each_deny_cause_is_preserved() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Arc::new(Recorder::create(root.path(), "synthetic").unwrap());
    let capture = Capture::new(Arc::clone(&recorder), RtcHooks::default()).without_request_bodies();
    let paused = event(
        "Fetch.requestPaused",
        json!({"request":{"method":"POST","url":"https://synthetic.example","postData":"synthetic form","postDataEntries":[{"bytes":"c3ludGhldGlj"}]}}),
    );
    for cause in [
        DenyCause::Unclassified,
        DenyCause::LogUnavailable,
        DenyCause::Malformed,
        DenyCause::ForeignSocket,
        DenyCause::Forced,
        DenyCause::Fenced(teams_fence::DenyReason::OrgWide),
    ] {
        capture.http_verdict(&paused, Verdict::Deny(cause));
    }
    capture.http_verdict(&paused, Verdict::Allow);
    let mut not_object = json!(17);
    strip_body(&mut not_object);
    assert_eq!(not_object, 17);
    let rows = entries(&recorder);
    for row in &rows[..6] {
        assert_eq!(row["verdict"], "deny");
        assert!(row["request"].get("postData").is_none());
        assert!(row["request"].get("postDataEntries").is_none());
    }
    assert!(rows[5]["cause"].get("fenced").is_some());
    assert_eq!(rows[6]["verdict"], "allow");
    assert!(rows[6].get("request").is_none());
    assert_eq!(unix_ms(UNIX_EPOCH + Duration::from_secs(u64::MAX / 1000 + 1)), u64::MAX);
}

fn response_events(capture: &Capture, cdp: &Cdp) {
    for (id, kind, length) in [
        ("error", "XHR", 1.0),
        ("empty", "Other", 1.0),
        ("script", "Script", 1.0),
        ("large", "XHR", MAX_BODY + 1.0),
    ] {
        capture.observe(
            cdp,
            &event("Network.responseReceived", json!({"requestId":id,"type":kind})),
        );
        capture.observe(
            cdp,
            &event(
                "Network.loadingFinished",
                json!({"requestId":id,"encodedDataLength":length}),
            ),
        );
    }
}
