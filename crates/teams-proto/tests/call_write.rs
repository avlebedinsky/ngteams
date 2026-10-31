//! The sealed call writes (M3 Task 11) on wiremock, through the `test-support` mock origin: each built in its captured
//! shape (`docs/protocol/calling-echo.md`, `docs/protocol/calling.md`, the fixtures in `tests/fixtures/calling/`), its
//! target what the browser fence's classifier makes of the same bytes, sent only as `Authorized<_>` to the calling
//! service (the flight proxy, the account's skypetoken, the call's headers, the link exactly as handed out), and the
//! fence's call registry fed by the transport: a start and a join with media registered from their answers, an
//! `attach` and an `accept` binding theirs, so under the registry's rule (no build's fence's, `Fence::tracking_calls`
//! here) every later write of the call passes `Fence::check` only on the links the call's services handed out. Under
//! the lab's own rule (at the end) only a call's entry is judged, and the rest goes under its permit, logged.
//!
//! Tokens, ids, MRIs, hosts and links here are placeholders.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::io::{self, Write};
use std::sync::{Arc, Mutex, PoisonError};

use base64::Engine as _;
use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_fence::{
    CallOperation, Contact, DeliveryDenyReason, DenyReason, FailClosed, Fence, FenceLog, Membership, MembershipFuture,
    MembershipSource, Mri, PartialReason, Policy, Request as FenceRequest, Roster, Scope, Target, WriteKind,
    contacting, one_to_one_id,
};
use teams_proto::{
    AcceptCall, AcceptParts, AcceptanceAcknowledgement, AcknowledgeAcceptance, AcknowledgeMediaAnswer, AddParts,
    AddToCall, AnswerMedia, AnswerMediaParts, ApplyChannelParameters, AttachCall, AttachParts, Authorized,
    CALL_START_URL, CALLING_BASE, CALLING_HOST, CallContext, CallLink, CallParticipant, CallWriteError,
    ChannelParameters, ClientInfo, Credentials, DeclineCall, FrameSink, FrameText, FrameWriter, HttpError,
    HttpTransport, InvitationReceipt, Invitee, JoinCall, JoinMedia, JoinParts, LeaveCall, LeaveReason, Links,
    MediaContent, MediaDescription, MediaDescriptions, MeetingInfo, Mutating, Refusal, RelayAnswerError, RelayTokens,
    RenegotiateMedia, ReportRinging, RespondError, Response, ResponseRefused, StartCall, StartParts, Transport,
    UpdateEndpointMetadata, UpdateEndpointState, UpdateMediaDescriptions, UpdateParticipantProperties, VoiceConsent,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const LAB_A: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const LAB_B: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const OUTSIDER: &str = "8:orgid:00000000-0000-4000-8000-0000000000ff";
const ECHO: &str = "28:00000000-0000-4000-8000-0000000000ec";
const GROUP: &str = "19:syntheticgroupchat0abc@thread.v2";
const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const SKYPETOKEN: &str = "placeholder-skypetoken";
const CHAIN: &str = "00000000-0000-4000-8000-0000000000c1";
const PROXY: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep";
/// A's and b's calling endpoints' push URLs.
const PUSH_A: &str = "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint00000/";
const PUSH_B: &str = "https://pub-ent-zz-02-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint00001/";
const ENDPOINT_B: &str = "/v4/f/SyntheticEndpoint00001/";
const FOLLOW_UPS_FIXTURE: &str = include_str!("../../../tests/fixtures/calling/chat/follow_ups.json");

/// The flight proxy's link to `rest` on the conversation service.
fn conv(id: &str, rest: &str) -> String {
    let rest = if rest.is_empty() {
        String::new()
    } else {
        format!("/{rest}")
    };
    format!("{PROXY}/conv-zz-01-prod-aks.conv.skype.com/conv/{id}{rest}?i=0-0-0-0&e=1")
}

/// The flight proxy's link to the call controller's `<kind>/<call>/…/<op>`.
fn cc(kind: &str, call: &str, op: &str) -> String {
    format!("{PROXY}/cc-zz-01-prod-aks.cc.skype.com/cc/v1/{kind}/{call}/1/x1/1/{op}?i=0-0-0-0")
}

fn link(url: &str) -> CallLink {
    CallLink::parse(url).unwrap_or_else(|| panic!("not a call link: {url}"))
}

/// The object id of a work account's or a bot's MRI.
fn object_id(mri: &str) -> &str {
    mri.strip_prefix("8:orgid:")
        .or_else(|| mri.strip_prefix("28:"))
        .unwrap()
}

/// What a's `add` of `invitee` into its call sends.
fn add_parts(invitee: &str) -> AddParts {
    AddParts {
        from: participant(LAB_A),
        invitee: Invitee {
            mri: Mri::new(invitee),
            participant_id: Some("00000000-0000-4000-8000-0000000000e4".into()),
        },
        chat_thread: format!("19:{}_{}@unq.gbl.spaces", object_id(LAB_A), object_id(invitee)),
        tenant_id: "00000000-0000-4000-8000-0000000000d0".into(),
        links: Links::new().with(
            "addParticipantSuccess",
            callback(PUSH_A, "07070707", "conversation", "addParticipantSuccess"),
        ),
    }
}

/// A callback of `push`'s call agent: `<push>callAgent/<agent>/<segment>/<kind>/<name>/`.
fn callback(push: &str, segment: &str, kind: &str, name: &str) -> String {
    format!("{push}callAgent/00000000-0000-4000-8000-0000000000a1/{segment}/{kind}/{name}/")
}

fn participant(mri: &str) -> CallParticipant {
    CallParticipant {
        mri: Mri::new(mri),
        display_name: Some("Lab".into()),
        endpoint_id: "00000000-0000-4000-8000-0000000000e1".into(),
        participant_id: Some("00000000-0000-4000-8000-0000000000e2".into()),
        language_id: Some("en-us".into()),
    }
}

fn context() -> CallContext {
    CallContext::new(CHAIN).with_region(Some("amer".into()), Some("amer01".into()))
}

fn client() -> ClientInfo {
    ClientInfo {
        endpoint_capabilities: Some(1),
        client_endpoint_capabilities: Some(1),
        platform_name: Some("SyntheticNative/1".into()),
    }
}

/// An offer in Teams' dialect (its content type only matters here).
fn media(required_features: Option<&str>) -> MediaContent {
    MediaContent {
        required_features: required_features.map(str::to_owned),
        client_location: Some("ZZ".into()),
        media_leg_id: Some("SYNTHETICMEDIALEG0000000000000000".into()),
        ..MediaContent::new("v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\ns=-\r\n", "application/sdp-ngc-1.0")
    }
}

fn start_parts(push: &str, from: &str, required_features: Option<&str>) -> StartParts {
    StartParts {
        from: participant(from),
        roster_update: callback(push, "0a0a0a0a", "conversation", "rosterUpdate"),
        conversation_links: Links::new()
            .with(
                "conversationEnd",
                callback(push, "0b0b0b0b", "conversation", "conversationEnd"),
            )
            .with(
                "conversationUpdate",
                callback(push, "0c0c0c0c", "conversation", "conversationUpdate"),
            ),
        call_links: Links::new()
            .with("progress", callback(push, "01010101", "call", "progress"))
            .with("mediaAnswer", callback(push, "02020202", "call", "mediaAnswer"))
            .with("acceptance", callback(push, "03030303", "call", "acceptance"))
            .with("end", callback(push, "05050505", "call", "end")),
        media_controller_links: Links::new().with(
            "controlVideoStreaming",
            callback(push, "06060606", "call", "controlVideoStreaming"),
        ),
        modalities: vec!["Audio".into()],
        media: media(required_features),
        client: client(),
    }
}

/// Placeholder tokens, with a skypetoken or without one.
struct Tokens {
    skypetoken: bool,
    rejected: Mutex<Vec<String>>,
}

impl Credentials for Tokens {
    fn bearer(&self, audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(format!("placeholder-token-for-{audience}"))))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(format!("placeholder-token-for-{audience}"))))
    }

    fn skypetoken(&self) -> impl Future<Output = Result<Option<Secret>, TokenError>> + Send {
        future::ready(Ok(self.skypetoken.then(|| Secret::new(SKYPETOKEN))))
    }

    fn skypetoken_rejected(
        &self,
        rejected: &Secret,
    ) -> impl Future<Output = Result<Option<Secret>, TokenError>> + Send {
        self.rejected
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(rejected.expose().to_owned());
        future::ready(Ok(Some(Secret::new(SKYPETOKEN))))
    }
}

/// A `fence.log` the test reads back.
#[derive(Clone, Default)]
struct SharedLog(Arc<Mutex<Vec<u8>>>);

impl SharedLog {
    fn lines(&self) -> Vec<Value> {
        let bytes = self.0.lock().unwrap_or_else(PoisonError::into_inner).clone();
        String::from_utf8(bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

impl Write for SharedLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The lab's chats, read in full: the a–b 1:1 chat and the a–b–c group chat.
struct LabChats;

impl MembershipSource for LabChats {
    fn members<'a>(&'a self, scope: &'a Scope) -> MembershipFuture<'a> {
        Box::pin(async move {
            match scope {
                Scope::Conversation { id } if id == GROUP => {
                    Membership::Complete(vec![Mri::new(LAB_A), Mri::new(LAB_B), Mri::new(LAB_C)])
                }
                Scope::Conversation { id } if Some(id) == one_to_one_id(LAB_A, LAB_B).as_ref() => {
                    Membership::Complete(vec![Mri::new(LAB_A), Mri::new(LAB_B)])
                }
                _ => Membership::Partial(PartialReason::NoSource),
            }
        })
    }
}

/// One account's fence (owning `own`) and native transport, its calling service a mock.
struct Lab {
    server: MockServer,
    log: SharedLog,
    fence: Arc<Fence>,
    tokens: Arc<Tokens>,
    transport: HttpTransport<Tokens>,
}

/// `own`'s lab whose fence decides calls by the call registry's own rule: the transport's registry
/// feeding is tested on it.
async fn lab(own: &str) -> Lab {
    lab_with(own, true).await
}

async fn lab_with(own: &str, skypetoken: bool) -> Lab {
    lab_of(own, skypetoken, true).await
}

/// `own`'s lab whose fence decides calls by the lab's own rule: only a call's entry is judged.
async fn lab_rule(own: &str) -> Lab {
    lab_of(own, true, false).await
}

async fn lab_of(own: &str, skypetoken: bool, tracking: bool) -> Lab {
    let server = MockServer::start().await;
    let log = SharedLog::default();
    let fence = Fence::new(
        Roster::new([Mri::new(LAB_A), Mri::new(LAB_B), Mri::new(LAB_C)], [Mri::new(ECHO)]),
        FenceLog::from_writer(log.clone()),
    )
    .with_own_account(Mri::new(own));
    let fence = if tracking { fence.tracking_calls() } else { fence };
    let fence = Arc::new(fence);
    let tokens = Arc::new(Tokens {
        skypetoken,
        rejected: Mutex::default(),
    });
    let transport =
        HttpTransport::with_mock_origin(Arc::clone(&tokens), Arc::clone(&fence), CHAT_BASE, &server.uri()).unwrap();
    Lab {
        server,
        log,
        fence,
        tokens,
        transport,
    }
}

impl Lab {
    /// The mock answers `verb` at the path of `url` with `answer`.
    async fn answers(&self, verb: &str, url: &str, answer: Value) {
        let at = url::Url::parse(url).unwrap();
        Mock::given(method(verb))
            .and(path(at.path()))
            .respond_with(ResponseTemplate::new(200).set_body_json(answer))
            .mount(&self.server)
            .await;
    }

    async fn authorize<R: Mutating>(&self, write: R) -> Authorized<R> {
        let permit = self
            .fence
            .check(write.target(), &LabChats)
            .await
            .unwrap_or_else(|denied| panic!("denied: {denied}"));
        Authorized::new(write, permit).unwrap()
    }

    /// `write` checked by the fence and delivered; the request the mock received for it.
    async fn send<R: Mutating + Send>(&self, write: R) -> (Response, Request) {
        let authorized = self.authorize(write).await;
        let answer = self.transport.deliver(authorized).await.unwrap();
        let received = self.server.received_requests().await.unwrap();
        (answer, received.last().cloned().unwrap())
    }

    /// What the fence decides for `write` now.
    async fn decide<R: Mutating>(&self, write: &R) -> Result<(), DenyReason> {
        self.fence
            .check(write.target(), &LabChats)
            .await
            .map(drop)
            .map_err(|denied| *denied.reason())
    }

    fn deliver_trouter(&self, at: &str, body: &Value) {
        let path = url::Url::parse(at).unwrap().path().to_owned();
        self.fence.observe_delivery(&delivery(7, &path, body));
    }
}

/// A Trouter delivery on the connection `calling` at `path`.
fn delivery(id: i64, path: &str, body: &Value) -> teams_fence::Delivery {
    teams_fence::Delivery {
        connection: "calling".into(),
        id,
        path: path.into(),
        trouter_request: Some(format!("request-{id}")),
        chain_id: Some("chain".into()),
        message_id: Some(format!("message-{id}")),
        body: serde_json::to_vec(body).unwrap(),
    }
}

/// The request's body as JSON.
fn body_of(request: &Request) -> Value {
    serde_json::from_slice(&request.body).unwrap()
}

fn header<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    request.headers.get(name).and_then(|value| value.to_str().ok())
}

/// The request as the fence's classifier reads it: what the browser fence would decide for these bytes.
fn classified(request: &Request, real_url: &str, policy: &Policy) -> Contact {
    let verb = request.method.as_str().to_owned();
    let body = (!request.body.is_empty()).then_some(request.body.as_slice());
    contacting(&FenceRequest::new(&verb, real_url).with_body(body), policy)
}

/// The captured body of the fixture case named `name`.
fn captured(fixture: &str, name: &str) -> Value {
    let fixture: Value = serde_json::from_str(fixture).unwrap();
    fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == name)
        .unwrap_or_else(|| panic!("no case {name}"))["body"]
        .clone()
}

/// The conversation a start's answer gives, on `id`.
fn conversation_answer(id: &str) -> Value {
    json!({
        "conversationController": conv(id, ""),
        "links": {
            "updateEndpointMetadata": conv(id, "updateEndpointMetadata"),
            "leave": conv(id, "leave"),
            "addParticipantAndModality": conv(id, "add"),
        },
    })
}

/// A's test call, started and registered on the conversation `Synthetic0`.
async fn started_test_call(lab: &Lab) -> (Response, Request) {
    lab.answers("POST", CALL_START_URL, conversation_answer("Synthetic0"))
        .await;
    let start = StartCall::echo_test(&context(), &start_parts(PUSH_A, LAB_A, None), &Mri::new(ECHO)).unwrap();
    lab.send(start).await
}

#[tokio::test]
async fn the_test_call_start_goes_to_the_flight_proxy_with_the_skypetoken_and_registers_its_call() {
    let lab = lab(LAB_A).await;
    let metadata = UpdateEndpointMetadata::new(
        &context(),
        &link(&conv("Synthetic0", "updateEndpointMetadata")),
        &participant(LAB_A),
    )
    .unwrap();
    assert_eq!(
        lab.decide(&metadata).await,
        Err(DenyReason::Partial(PartialReason::UnregisteredCall)),
        "no follow-up before the start's answer registered the call"
    );

    let (answer, request) = started_test_call(&lab).await;

    assert_eq!(answer.status(), 200);
    assert_eq!(request.method.as_str(), "POST");
    assert_eq!(request.url.path(), "/api/v2/epconv");
    assert_eq!(request.url.query(), None);
    assert_eq!(header(&request, "x-skypetoken"), Some(SKYPETOKEN));
    assert_eq!(
        header(&request, "authorization"),
        None,
        "the calling service takes no bearer token"
    );
    assert_eq!(header(&request, "x-microsoft-skype-chain-id"), Some(CHAIN));
    let message = header(&request, "x-microsoft-skype-message-id").unwrap();
    assert_eq!(message.len(), 36, "a fresh GUID: {message}");
    assert_eq!(header(&request, "ms-teams-region"), Some("amer"));
    assert_eq!(header(&request, "ms-teams-partition"), Some("amer01"));
    let target = Target::new(WriteKind::Call, Scope::New)
        .with_added([Mri::new(LAB_A), Mri::new(ECHO)])
        .with_sender(Mri::new(LAB_A));
    let policy = Policy::new().with_echo_callee(Mri::new(ECHO));
    assert_eq!(classified(&request, CALL_START_URL, &policy), Contact::Write(target));
    let body = body_of(&request);
    assert_eq!(body["conversationRequest"]["scenario"], "UserInitiatedTestCall");
    assert_eq!(body["participants"]["to"], json!([]));
    assert_eq!(body["groupChat"], Value::Null);
    assert_eq!(
        body["callInvitation"]["mediaContent"]["contentType"],
        "application/sdp-ngc-1.0"
    );
    assert_eq!(lab.decide(&metadata).await, Ok(()), "the answer registered the call");
}

#[tokio::test]
async fn a_chat_call_start_targets_the_chat_it_carries_with_every_callee() {
    let invitee = |mri: &str| Invitee {
        mri: Mri::new(mri),
        participant_id: Some("00000000-0000-4000-8000-0000000000e3".into()),
    };
    let parts = start_parts(PUSH_A, LAB_A, Some("nonByPass"));
    let one = StartCall::in_chat(&context(), &parts, &[invitee(LAB_B)], None).unwrap();
    let chat = one_to_one_id(LAB_A, LAB_B).unwrap();
    assert_eq!(
        one.target(),
        Target::new(WriteKind::Call, Scope::Conversation { id: chat })
            .with_added([Mri::new(LAB_A), Mri::new(LAB_B)])
            .with_sender(Mri::new(LAB_A))
    );
    let group = StartCall::in_chat(&context(), &parts, &[invitee(LAB_B), invitee(LAB_C)], Some(GROUP)).unwrap();
    assert_eq!(
        group.target(),
        Target::new(WriteKind::Call, Scope::Conversation { id: GROUP.into() })
            .with_added([Mri::new(LAB_A), Mri::new(LAB_B), Mri::new(LAB_C)])
            .with_sender(Mri::new(LAB_A))
    );
    assert_eq!(one.echo(), None);

    let lab = lab(LAB_A).await;
    lab.answers("POST", CALL_START_URL, conversation_answer("Synthetic1"))
        .await;
    let (_, request) = lab.send(one).await;
    assert_eq!(
        classified(&request, CALL_START_URL, &Policy::new()),
        Contact::Write(
            Target::new(
                WriteKind::Call,
                Scope::Conversation {
                    id: one_to_one_id(LAB_A, LAB_B).unwrap()
                }
            )
            .with_added([Mri::new(LAB_A), Mri::new(LAB_B)])
            .with_sender(Mri::new(LAB_A))
        )
    );
    let outsider = StartCall::in_chat(&context(), &parts, &[invitee(OUTSIDER)], None).unwrap();
    assert!(
        lab.decide(&outsider).await.is_err(),
        "a call to someone outside the lab is denied"
    );
}

#[test]
fn a_start_outside_its_captured_shape_is_never_made() {
    let invitee = |mri: &str| Invitee {
        mri: Mri::new(mri),
        participant_id: None,
    };
    let parts = start_parts(PUSH_A, LAB_A, None);
    let unmapped = |result: Result<StartCall, CallWriteError>| matches!(result, Err(CallWriteError::Unmapped));
    assert!(unmapped(StartCall::in_chat(&context(), &parts, &[], None)), "no callee");
    assert!(
        unmapped(StartCall::in_chat(
            &context(),
            &parts,
            &[invitee(LAB_B), invitee(LAB_C)],
            None
        )),
        "two callees and no group chat"
    );
    assert!(
        unmapped(StartCall::in_chat(&context(), &parts, &[invitee(LAB_A)], None)),
        "the caller as its own callee"
    );
    assert!(
        unmapped(StartCall::in_chat(&context(), &parts, &[invitee(ECHO)], None)),
        "a bot as a chat call's callee"
    );
    assert!(
        unmapped(StartCall::in_chat(
            &context(),
            &parts,
            &[invitee(LAB_B)],
            Some("19:abc_def@unq.gbl.spaces")
        )),
        "a group chat id that isn't one"
    );
    let featured = start_parts(PUSH_A, LAB_A, Some("nonByPass"));
    assert!(
        unmapped(StartCall::echo_test(&context(), &featured, &Mri::new(ECHO))),
        "the test call's offer has no required features"
    );
    let mut odd = start_parts(PUSH_A, LAB_A, None);
    odd.call_links = odd
        .call_links
        .with("transfer", callback(PUSH_A, "09090909", "call", "transfer"));
    assert!(
        unmapped(StartCall::echo_test(&context(), &odd, &Mri::new(ECHO))),
        "a callback no capture showed in a start"
    );
}

#[tokio::test]
async fn the_echo_joins_through_an_add_on_the_registered_call_only() {
    let lab = lab(LAB_A).await;
    let add = |invitee: &str| AddToCall::invite(&context(), &link(&conv("Synthetic0", "add")), &add_parts(invitee));
    let echo = add(ECHO).unwrap();
    assert_eq!(echo.target().kind(), WriteKind::Invite);
    assert_eq!(echo.target().added().iter().collect::<Vec<_>>(), [&Mri::new(ECHO)]);
    assert!(matches!(
        echo.target().scope(),
        Scope::CallOperation { operation: CallOperation::Add, url, .. } if *url == conv("Synthetic0", "add")
    ));
    assert_eq!(
        lab.decide(&echo).await,
        Err(DenyReason::Partial(PartialReason::UnregisteredCall))
    );
    started_test_call(&lab).await;
    lab.answers("POST", &conv("Synthetic0", "add"), json!({})).await;
    let (_, request) = lab.send(echo).await;
    assert_eq!(
        request.url.path(),
        "/api/v2/ep/conv-zz-01-prod-aks.conv.skype.com/conv/Synthetic0/add"
    );
    assert_eq!(
        request.url.query(),
        Some("i=0-0-0-0&e=1"),
        "the link's query as handed out"
    );
    assert!(
        matches!(
            lab.decide(&add(OUTSIDER).unwrap()).await,
            Err(DenyReason::Outsiders { .. })
        ),
        "an add is judged per invitee"
    );
    let elsewhere = AddToCall::invite(
        &context(),
        &link(&conv("Synthetic0", "add")),
        &AddParts {
            chat_thread: GROUP.into(),
            ..add_parts(ECHO)
        },
    );
    assert!(
        matches!(elsewhere, Err(CallWriteError::Unmapped)),
        "an add names its own 1:1 chat only"
    );
}

/// A's test call, started, registered and accepted: the acceptance, at the start's own `acceptance` callback, binds
/// the call controller's links; the mock answers every follow-up. Its call controller's call id.
async fn accepted_test_call(lab: &Lab) -> &'static str {
    started_test_call(lab).await;
    // the acceptance, at the start's own `acceptance` callback, binds the call controller's links
    let call = "SyntheticCall0";
    lab.deliver_trouter(
        &callback(PUSH_A, "03030303", "call", "acceptance"),
        &json!({"callAcceptance": {"links": {
            "acknowledgement": cc("callAcceptance", call, "acknowledge"),
            "applyChannelParameters": cc("mcProxy", call, "applyChannelParameters"),
            "mediaRenegotiation": cc("active", call, "renegotiate"),
            "updateMediaDescriptions": cc("active", call, "updateMediaDescriptions"),
        }}}),
    );
    for (verb, url) in [
        ("POST", cc("callAcceptance", call, "acknowledge")),
        ("POST", cc("mcProxy", call, "applyChannelParameters")),
        ("POST", cc("active", call, "renegotiate")),
        ("POST", cc("active", call, "updateMediaDescriptions")),
        ("POST", cc("negotiations", call, "answer")),
        ("POST", cc("mediaAnswers", call, "acknowledge")),
        ("PUT", conv("Synthetic0", "updateEndpointMetadata")),
        ("POST", conv("Synthetic0", "leave")),
    ] {
        lab.answers(verb, &url, json!({})).await;
    }
    call
}

#[tokio::test]
async fn a_started_calls_follow_ups_pass_on_the_links_its_services_handed_out() {
    let lab = lab(LAB_A).await;
    let call = accepted_test_call(&lab).await;
    let ours = |segment: &str, name: &str| callback(PUSH_A, segment, "call", name);
    let acknowledge = AcknowledgeAcceptance::new(
        &context(),
        &link(&cc("callAcceptance", call, "acknowledge")),
        &Links::new().with("mediaRenegotiation", ours("0d0d0d0d", "mediaRenegotiation")),
    )
    .unwrap();
    let (_, request) = lab.send(acknowledge).await;
    assert!(request.url.path().ends_with("/acknowledge"));
    let parameters = ChannelParameters {
        mids: vec!["*".into()],
        media_parameter: "{}".into(),
    };
    lab.send(
        ApplyChannelParameters::new(
            &context(),
            &link(&cc("mcProxy", call, "applyChannelParameters")),
            &parameters,
        )
        .unwrap(),
    )
    .await;
    the_camera_goes_on_by_a_renegotiation_and_media_descriptions(&lab, call).await;
    the_media_negotiation_binds_its_answer_and_acknowledgement(&lab, call).await;
    lab.send(
        UpdateEndpointMetadata::new(
            &context(),
            &link(&conv("Synthetic0", "updateEndpointMetadata")),
            &participant(LAB_A),
        )
        .unwrap(),
    )
    .await;
    let leave = LeaveCall::new(
        &context(),
        &link(&conv("Synthetic0", "leave")),
        &participant(LAB_A),
        LeaveReason::HungUp,
    )
    .unwrap();
    let (_, request) = lab.send(leave).await;
    assert_eq!(body_of(&request)["callTransactionEnd"]["code"], 0);
    let after = UpdateEndpointMetadata::new(
        &context(),
        &link(&conv("Synthetic0", "updateEndpointMetadata")),
        &participant(LAB_A),
    )
    .unwrap();
    assert_eq!(
        lab.decide(&after).await,
        Err(DenyReason::Partial(PartialReason::CallEnded))
    );
}

/// The caller's renegotiation and media descriptions, on the links the acceptance bound.
async fn the_camera_goes_on_by_a_renegotiation_and_media_descriptions(lab: &Lab, call: &str) {
    let ours = |segment: &str, name: &str| callback(PUSH_A, segment, "call", name);
    let mut video = media(Some("nonByPass"));
    video.negotiation_tag = Some("tag;v_1".into());
    let renegotiate = RenegotiateMedia::new(
        &context(),
        &link(&cc("active", call, "renegotiate")),
        &participant(LAB_A),
        &["Audio".into(), "Video".into()],
        &Links::new()
            .with("mediaAnswer", ours("0e0e0e0e", "mediaAnswer"))
            .with("rejection", ours("0e0e0e0e", "rejection")),
        &video,
    )
    .unwrap();
    lab.send(renegotiate).await;
    let descriptions = MediaDescriptions {
        descriptions: vec![MediaDescription {
            mid: "1".into(),
            direction: "sendrecv".into(),
            label: Some("main-video".into()),
        }],
        request_id: Some(2),
    };
    let (_, request) = lab
        .send(
            UpdateMediaDescriptions::new(
                &context(),
                &link(&cc("active", call, "updateMediaDescriptions")),
                &descriptions,
                Some("tag;v_1"),
            )
            .unwrap(),
        )
        .await;
    assert_eq!(
        body_of(&request),
        json!({"UpdateMediaDescriptions": {"mediaDescriptions": {
            "descriptions": [{"mid": "1", "direction": "sendrecv", "label": "main-video"}],
            "negotiationTag": "tag;v_1",
            "requestId": 2,
        }}})
    );
}

/// A renegotiation delivered at the call's own `mediaRenegotiation` callback binds its `answer` link; the answer to
/// ours, at the `mediaAnswer` callback, binds its acknowledgement.
async fn the_media_negotiation_binds_its_answer_and_acknowledgement(lab: &Lab, call: &str) {
    let answer = AnswerMedia::new(
        &context(),
        &link(&cc("negotiations", call, "answer")),
        &AnswerMediaParts {
            sender: participant(LAB_A),
            modalities: vec!["Audio".into(), "Video".into()],
            links: Links::new().with(
                "mediaAcknowledgement",
                callback(PUSH_A, "0f0f0f0f", "call", "mediaAcknowledgement"),
            ),
            media_controller_links: Links::new(),
            media: media(None),
        },
    )
    .unwrap();
    assert_eq!(
        lab.decide(&answer).await,
        Err(DenyReason::Partial(PartialReason::UnregisteredCall))
    );
    lab.deliver_trouter(
        &callback(PUSH_A, "0d0d0d0d", "call", "mediaRenegotiation"),
        &json!({"mediaNegotiation": {"links": {"mediaAnswer": cc("negotiations", call, "answer")}}}),
    );
    lab.send(answer).await;
    lab.deliver_trouter(
        &callback(PUSH_A, "0e0e0e0e", "call", "mediaAnswer"),
        &json!({"mediaAnswer": {"links": {"mediaAcknowledgement": cc("mediaAnswers", call, "acknowledge")}}}),
    );
    let (_, request) = lab
        .send(AcknowledgeMediaAnswer::new(&context(), &link(&cc("mediaAnswers", call, "acknowledge"))).unwrap())
        .await;
    assert!(request.body.is_empty(), "a media acknowledgement has no body");
    // sent as the web client sends it: an empty JSON body, its length said (without, the call controller took 22 s)
    let header = |name: &str| {
        request
            .headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    assert_eq!(header("content-length").as_deref(), Some("0"));
    assert_eq!(header("content-type").as_deref(), Some("application/json"));
}

/// An invitation from a to b, delivered at b's calling endpoint: a 1:1 call's (107), or a group call's (109) in the
/// a–b–c chat.
fn invitation(group: bool, caller: &str) -> Value {
    let mut gp = json!({
        "callNotification": {
            "from": {"id": caller},
            "to": {"id": LAB_B},
            "links": {"attach": cc("forked", "SyntheticCall1", "attach")},
        },
        "conversationInvitation": {"conversationController": conv("Synthetic2", ""), "isMultiParty": group},
    });
    if group {
        gp["groupChat"] = json!({"threadId": GROUP, "messageId": "0"});
    }
    let gp = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&gp).unwrap());
    json!({"evt": if group { 109 } else { 107 }, "gp": gp})
}

fn attach_parts() -> AttachParts {
    AttachParts {
        from: participant(LAB_B),
        end: callback(PUSH_B, "0a0a0a0a", "call", "end"),
        roster_update: callback(PUSH_B, "0b0b0b0b", "conversation", "rosterUpdate"),
        conversation_links: Links::new().with(
            "conversationEnd",
            callback(PUSH_B, "0c0c0c0c", "conversation", "conversationEnd"),
        ),
        conversation: link(&conv("Synthetic2", "")),
        client: client(),
    }
}

/// B's answered-call links, as the attach's answer hands them out.
fn attach_answer() -> Value {
    json!({
        "callInvitation": {"links": {
            "progress": cc("incoming", "SyntheticCall1", "progress"),
            "acceptance": cc("incoming", "SyntheticCall1", "accept"),
            "callLeg": cc("incoming", "SyntheticCall1", "reject"),
        }},
        "additionalActionResponses": [{
            "url": conv("Synthetic2", ""),
            "output": {
                "conversationController": conv("Synthetic2", ""),
                "links": {
                    "updateEndpointState": conv("Synthetic2", "updateEndpointState"),
                    "updateParticipantProperties": conv("Synthetic2", "updateParticipantProperties"),
                    "leave": conv("Synthetic2", "leave"),
                },
            },
        }],
    })
}

/// The answered call's links, as the accept's answer hands them out (inline, as the captured acceptance's answer).
fn accept_answer() -> Value {
    json!({"callAcceptanceAcknowledgement": {"links": {
        "applyChannelParameters": cc("mcProxy", "SyntheticCall1", "applyChannelParameters"),
        "mediaRenegotiation": cc("active", "SyntheticCall1", "renegotiate"),
    }}})
}

/// B, offered a's invitation, attached and told the call it rings.
async fn ringing_callee() -> Lab {
    let lab = lab(LAB_B).await;
    lab.fence
        .observe_delivery(&delivery(3, ENDPOINT_B, &invitation(false, LAB_A)));
    lab.answers("POST", &cc("forked", "SyntheticCall1", "attach"), attach_answer())
        .await;
    lab.answers("POST", &cc("incoming", "SyntheticCall1", "accept"), accept_answer())
        .await;
    for (verb, op) in [("POST", "progress"), ("DELETE", "reject")] {
        lab.answers(verb, &cc("incoming", "SyntheticCall1", op), json!({}))
            .await;
    }
    let attach = AttachCall::new(
        &context(),
        &link(&cc("forked", "SyntheticCall1", "attach")),
        &attach_parts(),
    )
    .unwrap();
    let (_, request) = lab.send(attach).await;
    let body = body_of(&request);
    assert_eq!(body["attach"]["requireMediaContent"], false, "an attach asks no media");
    assert_eq!(body["additionalActions"][0]["name"], "join");
    assert_eq!(body["additionalActions"][0]["url"], conv("Synthetic2", ""));
    assert_eq!(
        body["additionalActions"][0]["input"].get("callInvitation"),
        None,
        "roster only"
    );
    let ringing = ReportRinging::new(
        &context(),
        &link(&cc("incoming", "SyntheticCall1", "progress")),
        &participant(LAB_B),
    )
    .unwrap();
    lab.send(ringing).await;
    lab
}

/// B accepts the invitation it was offered: the accept's answer binds the answered call's media links (its
/// `applyChannelParameters` and `mediaRenegotiation`), which the fence denied before it.
async fn the_accept_binds_the_answered_calls_media_links(lab: &Lab) {
    lab.answers(
        "POST",
        &cc("mcProxy", "SyntheticCall1", "applyChannelParameters"),
        json!({}),
    )
    .await;
    let parameters = ApplyChannelParameters::new(
        &context(),
        &link(&cc("mcProxy", "SyntheticCall1", "applyChannelParameters")),
        &ChannelParameters {
            mids: vec!["*".into()],
            media_parameter: "{}".into(),
        },
    )
    .unwrap();
    assert_eq!(
        lab.decide(&parameters).await,
        Err(DenyReason::Partial(PartialReason::UnregisteredCall)),
        "the answered call's links are bound only by the accept's answer"
    );
    let call_link = |name: &str| callback(PUSH_B, "20202020", "call", name);
    let accept = AcceptCall::new(
        &context(),
        &link(&cc("incoming", "SyntheticCall1", "accept")),
        &AcceptParts {
            accepted_by: participant(LAB_B),
            modalities: vec!["Audio".into()],
            call_links: Links::new().with("mediaRenegotiation", call_link("mediaRenegotiation")),
            media_controller_links: Links::new().with("controlVideoStreaming", call_link("controlVideoStreaming")),
            media: media(Some("nonByPass")),
            client: client(),
        },
    )
    .unwrap();
    let (_, request) = lab.send(accept).await;
    let body = body_of(&request);
    assert_eq!(body["callAcceptance"]["acceptedBy"]["id"], LAB_B);
    assert_eq!(body["callAcceptance"]["callKeepAliveInterval"], Value::Null);
    assert_eq!(body["callAcceptance"]["mediaContent"]["requiredFeatures"], "nonByPass");
    // the accept's answer bound the answered call's media links
    assert_eq!(lab.decide(&parameters).await, Ok(()));
    let (_, request) = lab.send(parameters).await;
    assert!(request.url.path().ends_with("/applyChannelParameters"));
    let renegotiate = RenegotiateMedia::new(
        &context(),
        &link(&cc("active", "SyntheticCall1", "renegotiate")),
        &participant(LAB_B),
        &["Audio".into(), "Video".into()],
        &Links::new().with("mediaAnswer", call_link("mediaAnswer")),
        &media(Some("nonByPass")),
    )
    .unwrap();
    assert_eq!(lab.decide(&renegotiate).await, Ok(()));
}

#[tokio::test]
async fn an_offered_invitation_is_attached_answered_and_left_on_its_own_links() {
    let lab = ringing_callee().await;
    for op in ["updateEndpointState", "updateParticipantProperties", "leave"] {
        lab.answers("POST", &conv("Synthetic2", op), json!({})).await;
    }
    let state = UpdateEndpointState::new(
        &context(),
        &link(&conv("Synthetic2", "updateEndpointState")),
        &participant(LAB_B),
        false,
        1,
    )
    .unwrap();
    lab.send(state).await;
    the_accept_binds_the_answered_calls_media_links(&lab).await;
    let consent = VoiceConsent {
        value: "0".into(),
        sequence: 1,
    };
    let properties = UpdateParticipantProperties::new(
        &context(),
        &link(&conv("Synthetic2", "updateParticipantProperties")),
        &participant(LAB_B),
        &consent,
        &callback(PUSH_B, "30303030", "conversation", "updateParticipantPropertiesStatus"),
        "00000000-0000-4000-8000-0000000000f0",
    )
    .unwrap();
    lab.send(properties).await;
    let leave = LeaveCall::new(
        &context(),
        &link(&conv("Synthetic2", "leave")),
        &participant(LAB_B),
        LeaveReason::HungUp,
    )
    .unwrap();
    lab.send(leave).await;
}

#[tokio::test]
async fn a_decline_is_the_captured_rejection_of_an_offered_invitation() {
    let lab = ringing_callee().await;
    let decline = DeclineCall::new(&context(), &link(&cc("incoming", "SyntheticCall1", "reject"))).unwrap();
    assert_eq!(decline.target().kind(), WriteKind::CallDecline);
    let (_, request) = lab.send(decline).await;
    assert_eq!(request.method.as_str(), "DELETE");
    assert_eq!(
        body_of(&request),
        captured(
            FOLLOW_UPS_FIXTURE,
            "the callee declines (the attach answer's callLeg link)"
        )
    );
    let again = AcceptCall::new(
        &context(),
        &link(&cc("incoming", "SyntheticCall1", "accept")),
        &AcceptParts {
            accepted_by: participant(LAB_B),
            modalities: vec!["Audio".into()],
            call_links: Links::new(),
            media_controller_links: Links::new(),
            media: media(None),
            client: client(),
        },
    )
    .unwrap();
    assert!(
        lab.decide(&again).await.is_err(),
        "a declined invitation is answered no more"
    );
}

#[tokio::test]
async fn an_invitation_from_outside_the_lab_is_never_attached() {
    let lab = lab(LAB_B).await;
    lab.fence
        .observe_delivery(&delivery(3, ENDPOINT_B, &invitation(false, OUTSIDER)));
    let attach = AttachCall::new(
        &context(),
        &link(&cc("forked", "SyntheticCall1", "attach")),
        &attach_parts(),
    )
    .unwrap();
    assert_eq!(
        lab.decide(&attach).await,
        Err(DenyReason::Partial(PartialReason::UnregisteredCall))
    );
}

/// A transport of `lab`'s fence and tokens whose every service is a closed port: each write it sends gets a network
/// error, no answer.
fn unreachable(lab: &Lab) -> HttpTransport<Tokens> {
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    HttpTransport::with_mock_origin(
        Arc::clone(&lab.tokens),
        Arc::clone(&lab.fence),
        CHAT_BASE,
        &format!("http://127.0.0.1:{closed}"),
    )
    .unwrap()
}

#[tokio::test]
async fn an_attach_its_service_didn_t_take_leaves_its_invitation_to_the_next_attempt() {
    // a 401 (the skypetoken renewed for the next attempt), a throttle, a refusal and no answer at all: the transport
    // binds nothing and retires nothing; whether another attempt goes, or the invitation is let go, is the call
    // machine's to say (`Fence::let_go`)
    let cases = [401, 503, 404, 0]
        .into_iter()
        .flat_map(|failure| [(failure, true), (failure, false)]);
    for (failure, tracking) in cases {
        let lab = lab_of(LAB_B, true, tracking).await;
        let invited = delivery(3, ENDPOINT_B, &invitation(false, LAB_A));
        assert_eq!(lab.fence.observe_delivery(&invited), teams_fence::Observed::Offered);
        let attach_at = cc("forked", "SyntheticCall1", "attach");
        let attach = || AttachCall::new(&context(), &link(&attach_at), &attach_parts()).unwrap();
        let authorized = lab.authorize(attach()).await;
        if failure == 0 {
            let error = unreachable(&lab).deliver(authorized).await.unwrap_err();
            assert!(!matches!(error, HttpError::Refused(_)), "{error:?}");
        } else {
            Mock::given(method("POST"))
                .and(path(url::Url::parse(&attach_at).unwrap().path()))
                .respond_with(ResponseTemplate::new(failure))
                .up_to_n_times(1)
                .with_priority(1)
                .mount(&lab.server)
                .await;
            let answer = lab.transport.deliver(authorized).await.unwrap();
            assert_eq!(answer.status(), failure);
        }
        // the next attempt goes on the same invitation, and its answer binds the invitation's links: it is answered
        lab.answers("POST", &attach_at, attach_answer()).await;
        let (answer, _) = lab.send(attach()).await;
        assert_eq!(answer.status(), 200, "{failure} (tracking: {tracking})");
        let call_link = |name: &str| callback(PUSH_B, "20202020", "call", name);
        let accept = AcceptCall::new(
            &context(),
            &link(&cc("incoming", "SyntheticCall1", "accept")),
            &AcceptParts {
                accepted_by: participant(LAB_B),
                modalities: vec!["Audio".into()],
                call_links: Links::new().with("mediaRenegotiation", call_link("mediaRenegotiation")),
                media_controller_links: Links::new().with("controlVideoStreaming", call_link("controlVideoStreaming")),
                media: media(Some("nonByPass")),
                client: client(),
            },
        )
        .unwrap();
        assert_eq!(lab.decide(&accept).await, Ok(()), "{failure} (tracking: {tracking})");
    }
}

#[tokio::test]
async fn a_join_with_media_of_a_running_group_call_registers_it() {
    let lab = lab(LAB_B).await;
    lab.fence
        .observe_delivery(&delivery(4, ENDPOINT_B, &invitation(true, LAB_A)));
    let parts = JoinParts {
        from: participant(LAB_B),
        roster_update: callback(PUSH_B, "40404040", "conversation", "rosterUpdate"),
        conversation_links: Links::new().with(
            "conversationEnd",
            callback(PUSH_B, "41414141", "conversation", "conversationEnd"),
        ),
        group_chat: GROUP.into(),
        meeting: Some(MeetingInfo {
            organizer_id: LAB_A["8:orgid:".len()..].into(),
            tenant_id: "00000000-0000-4000-8000-0000000000d0".into(),
        }),
        client: client(),
    };
    let subscribe = JoinCall::subscribe(&context(), &link(&conv("Synthetic2", "")), &parts).unwrap();
    assert!(matches!(
        subscribe.target().scope(),
        Scope::CallOperation { operation: CallOperation::Join, joins: Some(chat), .. } if chat == GROUP
    ));
    let join_media = JoinMedia {
        call_links: Links::new().with("acceptance", callback(PUSH_B, "43434343", "call", "acceptance")),
        media_controller_links: Links::new(),
        modalities: vec!["Audio".into(), "ScreenViewer".into()],
        media: media(None),
    };
    let enter = JoinCall::enter(&context(), &link(&conv("Synthetic2", "")), &parts, &join_media).unwrap();
    assert_eq!(enter.target().kind(), WriteKind::Call);
    lab.answers("POST", &conv("Synthetic2", ""), conversation_answer("Synthetic2"))
        .await;
    lab.send(enter).await;
    let metadata = UpdateEndpointMetadata::new(
        &context(),
        &link(&conv("Synthetic2", "updateEndpointMetadata")),
        &participant(LAB_B),
    )
    .unwrap();
    assert_eq!(
        lab.decide(&metadata).await,
        Ok(()),
        "the join's answer registered the call"
    );
}

#[tokio::test]
async fn without_a_skypetoken_no_call_write_is_sent() {
    let lab = lab_with(LAB_A, false).await;
    let start = StartCall::echo_test(&context(), &start_parts(PUSH_A, LAB_A, None), &Mri::new(ECHO)).unwrap();
    let authorized = lab.authorize(start).await;
    let error = lab.transport.deliver(authorized).await.unwrap_err();
    assert!(matches!(error, HttpError::Refused(Refusal::NoSkypetoken)), "{error:?}");
    assert!(lab.server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_start_under_another_fences_permit_is_refused_unsent() {
    let lab = lab(LAB_A).await;
    let other = lab_with(LAB_A, true).await;
    let start = StartCall::echo_test(&context(), &start_parts(PUSH_A, LAB_A, None), &Mri::new(ECHO)).unwrap();
    let permit = other.fence.check(start.target(), &FailClosed).await.unwrap();
    let authorized = Authorized::new(start, permit).unwrap();
    let error = lab.transport.deliver(authorized).await.unwrap_err();
    assert!(matches!(error, HttpError::Refused(Refusal::NotPended)), "{error:?}");
    assert!(lab.server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_refused_skypetoken_is_renewed_for_later_and_the_write_not_resent() {
    let lab = lab(LAB_A).await;
    Mock::given(method("POST"))
        .and(path("/api/v2/epconv"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&lab.server)
        .await;
    let start = StartCall::echo_test(&context(), &start_parts(PUSH_A, LAB_A, None), &Mri::new(ECHO)).unwrap();
    let authorized = lab.authorize(start).await;
    let answer = lab.transport.deliver(authorized).await.unwrap();
    assert_eq!(answer.status(), 401);
    assert_eq!(lab.server.received_requests().await.unwrap().len(), 1);
    assert_eq!(
        *lab.tokens.rejected.lock().unwrap_or_else(PoisonError::into_inner),
        [SKYPETOKEN]
    );
}

#[test]
fn a_media_contents_debug_leaves_its_sdp_out() {
    let pwd = "IcePwd+7rT/9qWx3ZpLmN0bV";
    let content = MediaContent::new(
        format!("v=0\r\na=ice-ufrag:Uf9+rAg/\r\na=ice-pwd:{pwd}\r\nc=IN IP4 192.0.2.7\r\n"),
        "application/sdp-ngc-1.0",
    );
    let shown = format!("{content:?}");
    for hidden in [pwd, "Uf9+rAg/", "192.0.2.7", "ice-"] {
        assert!(!shown.contains(hidden), "{shown}");
    }
    assert!(shown.contains("application/sdp-ngc-1.0"), "{shown}");
}

#[test]
fn the_calling_constants_are_the_fences_own() {
    assert_eq!(CALL_START_URL, teams_fence::CALL_START_URL);
    assert_eq!(CALL_START_URL, format!("{CALLING_BASE}/epconv"));
    assert_eq!(CALLING_HOST, teams_fence::CALL_PROXY_HOST);
    assert_eq!(CALLING_BASE, format!("https://{CALLING_HOST}/api/v2"));
    assert_eq!(teams_proto::TEST_CALL_SCENARIO, teams_fence::TEST_CALL_SCENARIO);
}

#[test]
fn a_call_link_is_a_link_on_the_flight_proxy_kept_as_given() {
    for good in [
        conv("Synthetic0", "leave"),
        conv("Synthetic0", ""),
        cc("forked", "SyntheticCall1", "attach"),
    ] {
        assert_eq!(link(&good).as_str(), good);
    }
    for bad in [
        "https://api.flightproxy.teams.microsoft.com/api/v2/epconv",
        "http://api.flightproxy.teams.microsoft.com/api/v2/ep/host/conv/x",
        "https://api.flightproxy.teams.microsoft.com:8443/api/v2/ep/host/conv/x",
        "https://user@api.flightproxy.teams.microsoft.com/api/v2/ep/host/conv/x",
        "https://api.flightproxy.teams.microsoft.com/api/v2/ep/host/conv/x#frag",
        "https://api.flightproxy.teams.microsoft.com/api/v2/ep/host",
        "https://api.flightproxy.teams.microsoft.com/api/v2/ep/host/conv/%2e%2e",
        "https://api.flightproxy.teams.microsoft.com/api/v2/ep/host//x",
        "https://api.flightproxy.teams.microsoft.com/api/v3/ep/host/conv/x",
        "https://evil.example/api/v2/ep/host/conv/x",
        "https://API.flightproxy.teams.microsoft.com/api/v2/ep/host/conv/x",
    ] {
        assert!(CallLink::parse(bad).is_none(), "{bad}");
    }
}

#[test]
fn a_write_on_another_operations_link_is_never_made() {
    let progress = ReportRinging::new(
        &context(),
        &link(&cc("incoming", "SyntheticCall1", "accept")),
        &participant(LAB_B),
    );
    assert!(matches!(progress, Err(CallWriteError::Unmapped)));
    let decline = DeclineCall::new(&context(), &link(&cc("forked", "SyntheticCall1", "attach")));
    assert!(matches!(decline, Err(CallWriteError::Unmapped)));
    let leave = LeaveCall::new(
        &context(),
        &link(&conv("Synthetic0", "add")),
        &participant(LAB_A),
        LeaveReason::Cancelled { rang_seconds: 9 },
    );
    assert!(matches!(leave, Err(CallWriteError::Unmapped)));
}

#[tokio::test]
async fn the_cancel_of_a_ringing_call_carries_how_long_it_rang() {
    let lab = lab(LAB_A).await;
    started_test_call(&lab).await;
    lab.answers("POST", &conv("Synthetic0", "leave"), json!({})).await;
    let cancel = LeaveCall::new(
        &context(),
        &link(&conv("Synthetic0", "leave")),
        &participant(LAB_A),
        LeaveReason::Cancelled { rang_seconds: 9 },
    )
    .unwrap();
    let (_, request) = lab.send(cancel).await;
    let body = body_of(&request);
    assert_eq!(body["callTransactionEnd"]["code"], 487);
    assert_eq!(
        body["callTransactionEnd"]["callQualityDiagnosticsInformation"]["cancelationDuration"],
        9
    );
}

#[tokio::test]
async fn every_call_write_is_logged_by_the_fence_before_it_goes() {
    let lab = lab(LAB_A).await;
    started_test_call(&lab).await;
    let lines = lab.log.lines();
    assert!(
        lines
            .iter()
            .any(|line| line["kind"] == "call" && line["verdict"] == "allow"),
        "{lines:?}"
    );
}

#[tokio::test]
async fn a_read_on_the_calling_service_is_refused() {
    struct CallingRead;
    impl teams_proto::Read for CallingRead {
        fn service(&self) -> teams_proto::Service {
            teams_proto::Service::Calling
        }

        fn endpoint(&self) -> teams_proto::Endpoint {
            teams_proto::Endpoint::new(["ep", "host", "x"])
        }
    }
    let lab = lab(LAB_A).await;
    let error = lab.transport.read(&CallingRead).await.unwrap_err();
    assert!(matches!(error, HttpError::Refused(Refusal::Calling)), "{error:?}");
}

/// What the calling socket was handed.
#[derive(Clone, Default)]
struct Socket(Arc<Mutex<Vec<String>>>);

impl Socket {
    fn texts(&self) -> Vec<String> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("the socket is closed")]
struct Closed;

impl FrameWriter for Socket {
    type Error = Closed;

    fn write(&mut self, text: FrameText) -> impl Future<Output = Result<(), Closed>> + Send {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(text.into_string());
        future::ready(Ok(()))
    }
}

/// The JSON of a `3:::` frame.
fn response_json(text: &str) -> Value {
    serde_json::from_str(text.strip_prefix("3:::").unwrap()).unwrap()
}

/// A's started call's acceptance, delivered with `id` on the connection `calling`.
fn acceptance(lab: &Lab, id: i64) -> teams_fence::Delivery {
    let at = callback(PUSH_A, "03030303", "call", "acceptance");
    let body = json!({"callAcceptance": {"links": {"acknowledgement": cc("callAcceptance", "SyntheticCall0", "acknowledge")}}});
    let acceptance = delivery(id, url::Url::parse(&at).unwrap().path(), &body);
    lab.fence.observe_delivery(&acceptance);
    acceptance
}

#[tokio::test]
async fn the_acceptance_is_acknowledged_once_as_a_typed_frame_the_fence_permitted() {
    let lab = lab(LAB_A).await;
    started_test_call(&lab).await;
    let acceptance = acceptance(&lab, 7);
    let socket = Socket::default();
    let mut sink = FrameSink::new(socket.clone()).with_call_responses(Arc::clone(&lab.fence), "calling");
    let links = Links::new().with(
        "mediaRenegotiation",
        callback(PUSH_A, "0d0d0d0d", "call", "mediaRenegotiation"),
    );
    let ack = AcceptanceAcknowledgement::new(&acceptance, "cv.1", 5, links).unwrap();

    sink.respond(ack.clone(), &LabChats).await.unwrap();

    let texts = socket.texts();
    assert_eq!(texts.len(), 1);
    let sent = response_json(&texts[0]);
    let acknowledgement = json!({"callAcceptanceAcknowledgement": {"links": {
        "mediaRenegotiation": callback(PUSH_A, "0d0d0d0d", "call", "mediaRenegotiation"),
    }}});
    assert_eq!(
        sent,
        json!({
            "id": 7,
            "status": 200,
            "headers": {
                "MS-CV": "cv.1",
                "trouter-request": "request-7",
                "trouter-client": "{\"cd\":5}",
                "X-Microsoft-Skype-Chain-ID": "chain",
                "X-Microsoft-Skype-Message-ID": "message-7",
            },
            "body": acknowledgement.to_string(),
        })
    );
    assert!(
        texts[0].starts_with("3:::{\"id\":7,\"status\":200,\"headers\":{\"MS-CV\""),
        "the captured key order: {}",
        texts[0]
    );
    assert!(matches!(
        sink.respond(ack, &LabChats).await,
        Err(RespondError::NotPending)
    ));
    assert_eq!(socket.texts().len(), 1, "a delivery is answered once");
    let logged = lab.log.lines();
    assert!(
        logged
            .iter()
            .any(|line| line["response"] == "call_acceptance_acknowledgement" && line["verdict"] == "allow"),
        "{logged:?}"
    );
}

#[tokio::test]
async fn a_response_decided_apart_goes_only_through_its_own_connection_s_sink() {
    let (lab, other) = (lab(LAB_A).await, lab(LAB_A).await);
    started_test_call(&lab).await;
    let socket = Socket::default();
    let sink = |fence: &Arc<Fence>, connection: &str| {
        FrameSink::new(socket.clone()).with_call_responses(Arc::clone(fence), connection)
    };
    let mut own = sink(&lab.fence, "calling");
    let decider = own.decider().unwrap();
    assert!(
        FrameSink::new(socket.clone()).decider().is_none(),
        "a sink that answers none"
    );
    let decide = |id| {
        let acceptance = acceptance(&lab, id);
        let ack = AcceptanceAcknowledgement::new(&acceptance, "cv.1", 5, Links::new()).unwrap();
        decider.decide(ack, &LabChats)
    };
    // permitted for the connection "calling" of this fence: refused on another connection's sink, another fence's,
    // and a sink that answers none, nothing written
    let permitted = decide(7).await.unwrap();
    assert!(matches!(
        sink(&lab.fence, "another").send_permitted(permitted).await,
        Err(RespondError::Denied(DeliveryDenyReason::Uncorrelated))
    ));
    let permitted = decide(8).await.unwrap();
    assert!(matches!(
        sink(&other.fence, "calling").send_permitted(permitted).await,
        Err(RespondError::Denied(DeliveryDenyReason::Uncorrelated))
    ));
    let permitted = decide(9).await.unwrap();
    assert!(matches!(
        FrameSink::new(socket.clone()).send_permitted(permitted).await,
        Err(RespondError::NoCallResponses)
    ));
    assert!(socket.texts().is_empty());
    // and sent by its own
    let last = acceptance(&lab, 10);
    let ack = || AcceptanceAcknowledgement::new(&last, "cv.1", 5, Links::new()).unwrap();
    let permitted = decider.decide(ack(), &LabChats).await.unwrap();
    own.send_permitted(permitted).await.unwrap();
    assert_eq!(response_json(&socket.texts()[0])["id"], 10);
    assert_eq!(
        decider.decide(ack(), &LabChats).await.err(),
        Some(ResponseRefused::NotPending),
        "decided once"
    );
}

#[tokio::test]
async fn an_acknowledgement_handing_out_another_endpoints_link_is_denied_unsent() {
    let lab = lab(LAB_A).await;
    started_test_call(&lab).await;
    let acceptance = acceptance(&lab, 8);
    let socket = Socket::default();
    let mut sink = FrameSink::new(socket.clone()).with_call_responses(Arc::clone(&lab.fence), "calling");
    let foreign = Links::new().with(
        "mediaRenegotiation",
        callback(PUSH_B, "0d0d0d0d", "call", "mediaRenegotiation"),
    );
    let ack = AcceptanceAcknowledgement::new(&acceptance, "cv.1", 5, foreign).unwrap();
    assert!(matches!(
        sink.respond(ack, &LabChats).await,
        Err(RespondError::Denied(DeliveryDenyReason::Shape))
    ));
    assert!(socket.texts().is_empty());
}

#[tokio::test]
async fn an_offered_invitation_gets_its_receipt_and_no_other_does() {
    let lab = lab(LAB_B).await;
    let offered = delivery(3, ENDPOINT_B, &invitation(false, LAB_A));
    lab.fence.observe_delivery(&offered);
    let socket = Socket::default();
    let mut sink = FrameSink::new(socket.clone()).with_call_responses(Arc::clone(&lab.fence), "calling");
    sink.respond(InvitationReceipt::new(&offered, 2).unwrap(), &LabChats)
        .await
        .unwrap();
    assert_eq!(
        socket.texts(),
        [
            r#"3:::{"id":3,"status":200,"headers":{"trouter-request":"request-3","trouter-client":"{\"cd\":2}"},"body":""}"#
        ]
    );

    let ignored = delivery(4, ENDPOINT_B, &invitation(false, OUTSIDER));
    lab.fence.observe_delivery(&ignored);
    assert!(matches!(
        sink.respond(InvitationReceipt::new(&ignored, 2).unwrap(), &LabChats)
            .await,
        Err(RespondError::NotPending)
    ));
    let again = delivery(5, ENDPOINT_B, &invitation(false, LAB_A));
    lab.fence.observe_delivery(&again);
    let mut other = FrameSink::new(socket.clone()).with_call_responses(Arc::clone(&lab.fence), "another connection");
    assert!(matches!(
        other
            .respond(InvitationReceipt::new(&again, 2).unwrap(), &LabChats)
            .await,
        Err(RespondError::NotPending)
    ));
    let mut plain = FrameSink::new(socket.clone());
    assert!(matches!(
        plain
            .respond(InvitationReceipt::new(&again, 2).unwrap(), &LabChats)
            .await,
        Err(RespondError::NoCallResponses)
    ));
    assert_eq!(socket.texts().len(), 1, "only the offered invitation's receipt went");
}

#[test]
fn a_response_needs_the_headers_its_delivery_carried() {
    let mut bare = delivery(9, ENDPOINT_B, &json!({}));
    bare.chain_id = None;
    assert!(AcceptanceAcknowledgement::new(&bare, "cv", 1, Links::new()).is_none());
    assert!(InvitationReceipt::new(&bare, 1).is_some());
    bare.trouter_request = None;
    assert!(InvitationReceipt::new(&bare, 1).is_none());
    let shown = format!(
        "{:?}",
        InvitationReceipt::new(&delivery(9, ENDPOINT_B, &json!({})), 1).unwrap()
    );
    assert!(!shown.contains("request-9"), "{shown}");
}

#[tokio::test]
async fn the_relay_credentials_are_a_read_with_the_ic3_token() {
    let lab = lab(LAB_A).await;
    Mock::given(method("GET"))
        .and(path("/trap/tokens"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tokens": [{"realm": "\"rtcmedia\"", "username": "placeholder-user", "password": "placeholder-password"}],
            "expires": 604_800,
        })))
        .mount(&lab.server)
        .await;
    let answer = lab.transport.read(&RelayTokens).await.unwrap();
    let request = lab.server.received_requests().await.unwrap().pop().unwrap();
    assert_eq!(request.url.query(), None);
    assert_eq!(
        header(&request, "authorization"),
        Some("Bearer placeholder-token-for-https://ic3.teams.office.com")
    );
    assert_eq!(header(&request, "api-version"), Some("2"));
    assert_eq!(header(&request, "x-ms-migration"), Some("True"));
    assert_eq!(header(&request, "x-skypetoken"), None);
    let relay = RelayTokens::read_answer(answer.body()).unwrap();
    assert_eq!(relay.expires, std::time::Duration::from_hours(168));
    let [credential] = relay.credentials.as_slice() else {
        panic!("one credential")
    };
    assert_eq!(credential.realm, "rtcmedia");
    assert_eq!(credential.username, "placeholder-user");
    assert_eq!(credential.password.expose(), "placeholder-password");
    assert!(!format!("{credential:?}").contains("placeholder-password"));
}

#[test]
fn a_relay_answer_outside_its_shape_reads_nothing() {
    for answer in [
        json!({}),
        json!({"tokens": [], "expires": 1}),
        json!({"tokens": [{"realm": "r", "username": "u"}], "expires": 1}),
        json!({"tokens": [{"realm": "r", "username": "", "password": "p"}], "expires": 1}),
        json!({"tokens": [{"realm": "r", "username": "u", "password": "p"}]}),
        json!({"tokens": [{"realm": "r", "username": "u", "password": "p"}], "expires": "1"}),
    ] {
        assert_eq!(
            RelayTokens::read_answer(&serde_json::to_vec(&answer).unwrap()).err(),
            Some(RelayAnswerError::Malformed),
            "{answer}"
        );
    }
}

// --- the lab's call rule: the transport sends what a call sends once it was let in, each under its permit ---

#[tokio::test]
async fn under_the_lab_rule_a_call_s_follow_ups_go_with_their_permits_and_no_registered_call() {
    let lab = lab_rule(LAB_A).await;
    assert!(!lab.fence.tracks_calls());
    lab.answers("PUT", &conv("Synthetic1", "updateEndpointMetadata"), json!({}))
        .await;
    lab.answers(
        "POST",
        &cc("mcProxy", "SyntheticCall0", "applyChannelParameters"),
        json!({}),
    )
    .await;
    lab.answers("POST", &conv("Synthetic1", "leave"), json!({})).await;
    // nothing was started, pended or registered: each goes all the same, under its own permit
    lab.send(
        UpdateEndpointMetadata::new(
            &context(),
            &link(&conv("Synthetic1", "updateEndpointMetadata")),
            &participant(LAB_A),
        )
        .unwrap(),
    )
    .await;
    let (_, request) = lab
        .send(
            ApplyChannelParameters::new(
                &context(),
                &link(&cc("mcProxy", "SyntheticCall0", "applyChannelParameters")),
                &ChannelParameters {
                    mids: vec!["*".into()],
                    media_parameter: "{}".into(),
                },
            )
            .unwrap(),
        )
        .await;
    assert!(request.url.path().ends_with("/applyChannelParameters"));
    lab.send(
        LeaveCall::new(
            &context(),
            &link(&conv("Synthetic1", "leave")),
            &participant(LAB_A),
            LeaveReason::HungUp,
        )
        .unwrap(),
    )
    .await;
    assert_eq!(lab.server.received_requests().await.unwrap().len(), 3);
    let follow_ups = lab
        .log
        .lines()
        .into_iter()
        .filter(|line| line["kind"] == "call_follow_up" && line["verdict"] == "allow")
        .count();
    assert_eq!(follow_ups, 3, "each one logged by the fence before it went");
}

#[tokio::test]
async fn under_the_lab_rule_an_offered_invitation_is_answered_with_nothing_bound_and_its_receipt_goes() {
    let lab = lab_rule(LAB_B).await;
    let offered = delivery(3, ENDPOINT_B, &invitation(false, LAB_A));
    lab.fence.observe_delivery(&offered);
    let socket = Socket::default();
    let mut sink = FrameSink::new(socket.clone()).with_call_responses(Arc::clone(&lab.fence), "calling");
    sink.respond(InvitationReceipt::new(&offered, 2).unwrap(), &LabChats)
        .await
        .unwrap();
    assert_eq!(socket.texts().len(), 1);
    // no attach went, so no answer bound the acceptance link: the accept goes on the leg its link names
    lab.answers("POST", &cc("incoming", "SyntheticCall1", "accept"), json!({}))
        .await;
    let accept = AcceptCall::new(
        &context(),
        &link(&cc("incoming", "SyntheticCall1", "accept")),
        &AcceptParts {
            accepted_by: participant(LAB_B),
            modalities: vec!["Audio".into()],
            call_links: Links::new(),
            media_controller_links: Links::new(),
            media: media(None),
            client: client(),
        },
    )
    .unwrap();
    let (_, request) = lab.send(accept).await;
    assert!(request.url.path().ends_with("/accept"));
    let lines = lab.log.lines();
    assert!(
        lines
            .iter()
            .any(|line| line["kind"] == "call_answer" && line["verdict"] == "allow"),
        "{lines:?}"
    );
}

#[tokio::test]
async fn under_the_lab_rule_an_outsider_s_invitation_is_never_answered() {
    let lab = lab_rule(LAB_B).await;
    let ignored = delivery(4, ENDPOINT_B, &invitation(false, OUTSIDER));
    assert_eq!(lab.fence.observe_delivery(&ignored), teams_fence::Observed::Ignored);
    let accept = AcceptCall::new(
        &context(),
        &link(&cc("incoming", "SyntheticCall1", "accept")),
        &AcceptParts {
            accepted_by: participant(LAB_B),
            modalities: vec!["Audio".into()],
            call_links: Links::new(),
            media_controller_links: Links::new(),
            media: media(None),
            client: client(),
        },
    )
    .unwrap();
    assert_eq!(
        lab.decide(&accept).await,
        Err(DenyReason::Partial(PartialReason::UnregisteredCall))
    );
    let decline = DeclineCall::new(&context(), &link(&cc("incoming", "SyntheticCall1", "reject"))).unwrap();
    assert_eq!(
        lab.decide(&decline).await,
        Err(DenyReason::Partial(PartialReason::UnregisteredCall))
    );
    assert!(lab.server.received_requests().await.unwrap().is_empty());
}
