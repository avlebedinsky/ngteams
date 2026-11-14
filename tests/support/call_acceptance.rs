/// The acceptance uses its delivery response, or the documented HTTP fallback when the delivery response is unavailable.
/// Validate the optional request before removing its one position from the ordered call operations.
pub fn assert_operations(
    operations: &[String],
    all: &[wiremock::Request],
    call: &str,
    responses: usize,
    expected: &[&str],
) -> bool {
    let http: Vec<_> = all
        .iter()
        .filter(|request| {
            request.url.path().contains("/callAcceptance/") && request.url.path().ends_with("/acknowledge")
        })
        .collect();
    assert!(http.len() <= 1, "one acceptance acknowledgement route: {operations:?}");
    let acceptance = acceptance_position(operations, http.len());
    if acceptance.is_some() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/calling/echo/answers.json")).unwrap();
        let expected = fixture["acceptance_callback"]["body"]["callAcceptance"]["links"]["acknowledgement"]
            .as_str()
            .unwrap()
            .replace("<guid:5>", call);
        let url = http[0].url.join(&expected).unwrap();
        assert_eq!(http[0].method.as_str(), "POST");
        assert_eq!(http[0].url.path(), url.path(), "the link belongs to this accepted call");
        assert_eq!(http[0].url.query(), url.query());
        assert_acknowledgement_payload(http[0], all);
        eprintln!("call {call}: acceptance acknowledged once by its admitted HTTP fallback");
    } else {
        eprintln!("acceptance acknowledged once by its delivery response");
    }
    let ordered: Vec<_> = operations
        .iter()
        .enumerate()
        .filter(|(at, _)| Some(*at) != acceptance)
        .map(|(_, operation)| operation.as_str())
        .collect();
    assert_eq!(ordered, expected);
    assert_eq!(responses + http.len(), 1, "one way, with no duplicate acknowledgement");
    !http.is_empty()
}

fn acceptance_position(operations: &[String], requests: usize) -> Option<usize> {
    let mut positions = operations
        .iter()
        .enumerate()
        .filter_map(|(at, operation)| (operation == "acceptanceAcknowledge").then_some(at));
    let acceptance = positions.next();
    assert!(positions.next().is_none(), "one acceptance operation: {operations:?}");
    assert_eq!(usize::from(acceptance.is_some()), requests, "{operations:?}");
    if let Some(at) = acceptance {
        assert!(at > 1 && at < operations.len() - 1, "{operations:?}");
        assert!(
            operations[2..at].iter().all(|operation| operation == "add"),
            "acceptance acknowledgement precedes later call operations: {operations:?}"
        );
    }
    acceptance
}

fn assert_acknowledgement_payload(request: &wiremock::Request, requests: &[wiremock::Request]) {
    let start = requests
        .iter()
        .find(|request| {
            request.url.path().ends_with("/epconv")
                && serde_json::from_slice::<serde_json::Value>(&request.body).is_ok()
        })
        .unwrap();
    let started: serde_json::Value = serde_json::from_slice(&start.body).unwrap();
    let acceptance = started["callInvitation"]["links"]["acceptance"].as_str().unwrap();
    let callback = acceptance.strip_suffix("/call/acceptance/").unwrap();
    let agent = format!("{}/", callback.rsplit_once('/').unwrap().0);
    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body.as_object().unwrap().len(), 1);
    let acknowledgement = body["callAcceptanceAcknowledgement"].as_object().unwrap();
    assert_eq!(acknowledgement.len(), 1);
    let links = acknowledgement["links"].as_object().unwrap();
    assert_eq!(links.len(), 7);
    for name in [
        "mediaRenegotiation",
        "transfer",
        "replacement",
        "balanceUpdate",
        "retargetCompletion",
        "controlVideoStreaming",
        "updateMediaDescriptions",
    ] {
        let link = links[name].as_str().unwrap();
        assert!(
            link.starts_with(&agent),
            "the fallback hands out only this call agent's callbacks"
        );
        assert!(link.ends_with(&format!("/call/{name}/")));
    }
}
pub fn is_acceptance_response(frame: &str) -> bool {
    let Some(text) = frame.strip_prefix("3:::") else {
        return false;
    };
    let Ok(envelope) = serde_json::from_str::<serde_json::Value>(text) else {
        return false;
    };
    if envelope["status"].as_u64() != Some(200) {
        return false;
    }
    let Some(body) = envelope["body"].as_str() else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(body).is_ok_and(|body| body["callAcceptanceAcknowledgement"].is_object())
}

#[test]
fn acceptance_responses_exclude_camera_receipts_and_unrelated_frames() {
    for (body, expected) in [
        (
            serde_json::json!({"callAcceptanceAcknowledgement": {"links": {}}}),
            true,
        ),
        (serde_json::json!({"mediaAcknowledgement": {"code": 0}}), false),
        (serde_json::json!({"callAcceptanceAcknowledgement": null}), false),
    ] {
        let frame = format!("3:::{}", serde_json::json!({"status": 200, "body": body.to_string()}));
        assert_eq!(is_acceptance_response(&frame), expected);
    }
    for frame in ["3:::{\"status\":200,\"body\":\"\"}", "3:::{", "2::", ""] {
        assert!(!is_acceptance_response(frame));
    }
}

#[test]
fn acceptance_positions_distinguish_setup_from_camera_receipts() {
    let operations: Vec<_> = [
        "start",
        "updateEndpointMetadata",
        "add",
        "renegotiate",
        "acknowledge",
        "leave",
    ]
    .map(String::from)
    .into();
    assert_eq!(acceptance_position(&operations, 0), None);
    for at in [2, 3] {
        let mut operations = operations.clone();
        operations.insert(at, "acceptanceAcknowledge".into());
        assert_eq!(acceptance_position(&operations, 1), Some(at));
    }
}

#[test]
#[should_panic(expected = "acceptance acknowledgement precedes later call operations")]
fn an_acceptance_after_the_camera_receipt_is_rejected() {
    let operations = [
        "start",
        "updateEndpointMetadata",
        "add",
        "renegotiate",
        "acknowledge",
        "acceptanceAcknowledge",
        "leave",
    ]
    .map(String::from);
    acceptance_position(&operations, 1);
}
