//! Handwritten synthetic caption protocol; all HTTP goes to the local mock origin.
#![cfg(test)]
use std::future::{Future, ready};
use std::sync::Arc;

use base64::Engine as _;
use serde_json::{Value, json};
use teams_auth::{secrets::Secret, token::TokenError};
use teams_proto::captions::{CaptionCommandLink, CaptionWrite, RECORDER_AUDIENCE};
use teams_proto::{CallContext, CallLink, CallParticipant, Credentials, HttpTransport, Transport};
use teams_proto::{CallState, Endpoint, Method, Mri, Request, Response, Service};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const A: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const B: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
const ADD: &str =
    "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-zz.conv.skype.com/conv/Synthetic/addParticipant";
const COMMAND: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/aks-prod-zz-api.callrecorder.teams.cloud.microsoft:21606/v2/oncommand/00000000-0000-4000-8000-000000000001";
const ROOT: &str =
    "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/Synthetic/callAgent/synthetic/segment/conversation/";

struct Tokens {
    correct: bool,
}
fn jwt(value: &Value) -> Secret {
    Secret::new(format!(
        "synthetic.{}.synthetic",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string())
    ))
}
impl Credentials for Tokens {
    fn bearer(&self, audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        ready(Ok(jwt(
            &json!({"aud":audience,"oid":if self.correct {"00000000-0000-4000-8000-00000000000a"} else {"00000000-0000-4000-8000-00000000000b"},"scp":"access_recorder_service"}),
        )))
    }
    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
    fn skypetoken(&self) -> impl Future<Output = Result<Option<Secret>, TokenError>> + Send {
        ready(Ok(Some(jwt(
            &json!({"skypeid":"orgid:00000000-0000-4000-8000-00000000000a"}),
        ))))
    }
}
fn actor() -> CallParticipant {
    CallParticipant {
        mri: Mri::new(A),
        display_name: Some("Lab Speaker".into()),
        endpoint_id: "00000000-0000-4000-8000-0000000000e1".into(),
        participant_id: Some("00000000-0000-4000-8000-0000000000e2".into()),
        language_id: Some("en-us".into()),
    }
}
fn chat() -> String {
    teams_proto::one_to_one_id(A, B).unwrap()
}
fn context() -> CallContext {
    CallContext::new("00000000-0000-4000-8000-0000000000c1")
}
// Populate the same canonical tracker used by the native account outbox.
struct Seed(url::Url, Value);
impl Request for Seed {
    fn service(&self) -> Service {
        Service::Calling
    }
    fn method(&self) -> Method {
        Method::Post
    }
    fn endpoint(&self) -> Endpoint {
        Endpoint::new([] as [&str; 0])
    }
    fn body(&self) -> Option<Value> {
        Some(self.1.clone())
    }
    fn call_url(&self) -> Option<&url::Url> {
        Some(&self.0)
    }
}
fn tracker() -> Arc<teams_call::Tracker> {
    let tracker = Arc::new(teams_call::Tracker::new(Mri::new(A)));
    assert!(tracker.connected("caption-test", ROOT.split("callAgent/").next().unwrap()));
    let actor = actor();
    let seed = Seed(
        url::Url::parse(teams_proto::CALL_START_URL).unwrap(),
        json!({
            "conversationRequest":{"links":{"rosterUpdate":format!("{ROOT}rosterUpdate/")}},
            "groupChat":{"threadId":chat()},
            "participants":{"from":{"id":A,"endpointId":actor.endpoint_id,"participantId":actor.participant_id}},
            "callInvitation":{"links":{"acceptance":format!("{ROOT}acceptance/")}}
        }),
    );
    let attempt = tracker.before(&seed);
    tracker.answered(
        attempt,
        &Response::new(
            200,
            json!({
                "conversationController":ADD.strip_suffix("/addParticipant").unwrap(),
                "links":{"addParticipant":ADD,"leave":ADD.replace("/addParticipant","/leave")}
            })
            .to_string()
            .into_bytes(),
        ),
    );
    tracker.observe_delivery(&teams_call::Delivery {
        connection: "caption-test".into(),
        id: 1,
        path: format!("{}rosterUpdate/", url::Url::parse(ROOT).unwrap().path()),
        trouter_request: None,
        chain_id: None,
        message_id: None,
        body: json!({"participants":{teams_proto::captions::CAPTION_SERVICE_MRI:{
            "version":1,"state":"active","endpoints":{"recorder":{"endpointMetadata":{
                "commandUrl":COMMAND,"spokenLanguage":"en-us","processingModes":{"closedCaptions":{"state":"Inactive"}}
            }}}
        }}})
        .to_string()
        .into_bytes(),
    });
    tracker
}
fn transport(server: &MockServer, correct: bool) -> HttpTransport<Tokens> {
    HttpTransport::with_mock_origin(
        Arc::new(Tokens { correct }),
        "https://teams.microsoft.com/api/chatsvc/amer",
        &server.uri(),
    )
    .unwrap()
    .for_account(Mri::new(A))
    .with_call_state(tracker())
}

#[tokio::test]
async fn invite_and_commands_use_their_actual_separate_credentials_and_closed_caption_mode() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let transport = transport(&server, true);
    let write = CaptionWrite::invite(
        &context(),
        &CallLink::parse(ADD).unwrap(),
        &actor(),
        &chat(),
        (
            &format!("{ROOT}addParticipantSuccess/"),
            &format!("{ROOT}addParticipantFailure/"),
        ),
    )
    .unwrap();
    assert!(transport.deliver_caption(write).await.unwrap().unwrap().is_success());
    let link = CaptionCommandLink::parse(COMMAND).unwrap();
    for write in [
        CaptionWrite::start(&context(), &link, &actor(), &chat()).unwrap(),
        CaptionWrite::language(&context(), &link, &actor(), &chat(), "en-gb").unwrap(),
    ] {
        assert!(transport.deliver_caption(write).await.unwrap().unwrap().is_success());
    }
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3);
    let invite: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(
        invite["participantInvitationData"]["botData"]["mode"],
        "RecordingAndTranscription"
    );
    assert!(
        invite
            .pointer("/participantInvitationData/botData/initiatorUserToken")
            .unwrap()
            .as_str()
            .unwrap()
            .contains('.')
    );
    let start: Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(start["processingModes"], json!(["closedCaptions"]));
    assert_eq!(start["participantSkypeToken"], "");
    assert_eq!(start["participantLegId"], actor().participant_id.unwrap());
    let header = requests[1].headers.get("authorization").unwrap().to_str().unwrap();
    let claims: Value = serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(header.split('.').nth(1).unwrap())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(claims["aud"], RECORDER_AUDIENCE);
    assert!(requests[1].headers.contains_key("x-skypetoken"));
    let language: Value = serde_json::from_slice(&requests[2].body).unwrap();
    assert_eq!(language["action"], "setLanguage");
    assert_eq!(
        language["actionParameters"],
        json!({"spokenLanguage":"en-gb","type":"setLanguage"})
    );
}

#[tokio::test]
async fn wrong_token_actor_and_read_only_refuse_before_any_post() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/unused"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let write = CaptionWrite::start(
        &context(),
        &CaptionCommandLink::parse(COMMAND).unwrap(),
        &actor(),
        &chat(),
    )
    .unwrap();
    assert!(
        transport(&server, false)
            .deliver_caption(write.clone())
            .await
            .unwrap()
            .is_err()
    );
    assert!(
        transport(&server, true)
            .read_only()
            .deliver_caption(write)
            .await
            .unwrap()
            .is_err()
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}
