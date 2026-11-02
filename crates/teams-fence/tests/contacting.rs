//! `teams_fence::contacting`'s table (`docs/plans/20261023-lab-fence-contacting-writes-only.md`, Technical Details):
//! a row per mapped write, carve-out and unreadable form, from the synthetic fixtures under
//! `tests/fixtures/contacting/`, the capture-shaped chat service fixtures of the repo (`tests/fixtures/chatsvc/`), and
//! the web client's test call start. Made-up GUIDs fill in the placeholders; no real MRI or token appears here.

use serde_json::Value;
use teams_fence::{
    CALL_START_URL, CallOperation, Contact, Mri, OWN_VIEW_PROPERTIES, Policy, Request, Scope, Service,
    TEST_CALL_SCENARIO, Target, WriteKind, contacting, is_chat_id, on_contacting_service, service_of,
};
use url::Url;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// A named change to a captured body.
type BodyEdit = Box<dyn Fn(&mut Value)>;

const GUID_A: &str = "00000000-0000-4000-8000-00000000000a";
const GUID_B: &str = "00000000-0000-4000-8000-00000000000b";
const GUID_X: &str = "00000000-0000-4000-8000-0000000000ff";
const GROUP: &str = "0123456789abcdef0123456789abcdef";
const ECHO: &str = "28:00000000-0000-4000-8000-0000000000e0";
const CHAT_SERVICE: &str = include_str!("fixtures/contacting/chat_service.json");
const SERVICES: &str = include_str!("fixtures/contacting/services.json");

fn fill(text: &str) -> String {
    text.replace("<guid:a>", GUID_A)
        .replace("<guid:b>", GUID_B)
        .replace("<guid:x>", GUID_X)
        .replace("<group>", GROUP)
        .replace("<region>", "emea")
        .replace("<tenant>", "contoso")
        .replace("<token>", "not-a-token")
}

fn lab_mri(guid: &str) -> Mri {
    Mri::new(format!("8:orgid:{guid}"))
}

fn one_to_one() -> String {
    format!("19:{GUID_A}_{GUID_B}@unq.gbl.spaces")
}

/// The lab policy the table is read under: the echo service as the test call's callee, lab account a as the account.
fn policy() -> Policy {
    Policy::new()
        .with_echo_callee(Mri::new(ECHO))
        .with_own_account(lab_mri(GUID_A))
}

/// One fixture case.
struct Case {
    name: String,
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
    expect: Contact,
}

impl Case {
    fn verdict(&self, policy: &Policy) -> Contact {
        let mut request = Request::new(&self.method, &self.url).with_body(self.body.as_deref());
        for (name, value) in &self.headers {
            request = request.with_header(name, value);
        }
        contacting(&request, policy)
    }
}

fn cases(fixture: &str) -> TestResult<Vec<Case>> {
    let fixture: Value = serde_json::from_str(&fill(fixture))?;
    let base = fixture["base"].as_str().unwrap_or_default();
    fixture["cases"]
        .as_array()
        .ok_or("no cases")?
        .iter()
        .map(|case| {
            let url = match case["url"].as_str() {
                Some(url) => url.to_owned(),
                None => format!("{base}{}", case["path"].as_str().ok_or("no path")?),
            };
            let body = match (&case["body"], case["raw_body"].as_str()) {
                (Value::Null, None) => None,
                (Value::Null, Some(raw)) => Some(raw.as_bytes().to_vec()),
                (body, _) => Some(serde_json::to_vec(body)?),
            };
            let headers = case["headers"]
                .as_array()
                .map(|headers| {
                    headers
                        .iter()
                        .filter_map(|pair| Some((pair[0].as_str()?.to_owned(), pair[1].as_str()?.to_owned())))
                        .collect()
                })
                .unwrap_or_default();
            Ok(Case {
                name: case["name"].as_str().unwrap_or_default().to_owned(),
                method: case["method"].as_str().ok_or("no method")?.to_owned(),
                url,
                headers,
                body,
                expect: expected(&case["expect"])?,
            })
        })
        .collect()
}

fn expected(expect: &Value) -> TestResult<Contact> {
    match expect.as_str() {
        Some("none") => return Ok(Contact::None),
        Some("unreadable") => return Ok(Contact::Unreadable),
        Some(other) => return Err(format!("an unknown verdict {other}").into()),
        None => {}
    }
    let kind = WriteKind::ALL
        .into_iter()
        .find(|kind| expect["kind"] == kind.as_str())
        .ok_or("an unknown kind")?;
    let scope = match (expect["conversation"].as_str(), expect["event"].as_str()) {
        (Some(id), _) => Scope::Conversation { id: id.to_owned() },
        (None, Some(id)) => Scope::Event { id: id.to_owned() },
        (None, None) if expect["new"] == true => Scope::New,
        (None, None) => return Err("no scope".into()),
    };
    let added: Vec<Mri> = expect["added"]
        .as_array()
        .map(|added| added.iter().filter_map(Value::as_str).map(Mri::new).collect())
        .unwrap_or_default();
    Ok(Contact::Write(Target::new(kind, scope).with_added(added)))
}

fn check_table(fixture: &str, at_least: usize) -> TestResult {
    let cases = cases(fixture)?;
    assert!(cases.len() >= at_least, "{} cases", cases.len());
    let policy = policy();
    let wrong: Vec<String> = cases
        .iter()
        .filter_map(|case| {
            let got = case.verdict(&policy);
            (got != case.expect).then(|| format!("{}: got {got:?}, want {:?}", case.name, case.expect))
        })
        .collect();
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    Ok(())
}

#[test]
fn every_chat_service_row_gives_its_verdict() -> TestResult {
    check_table(CHAT_SERVICE, 100)
}

#[test]
fn every_other_service_row_gives_its_verdict() -> TestResult {
    check_table(SERVICES, 70)
}

/// The writes of the capture-shaped fixtures (`docs/protocol/chat.md`): each fixture's requests, by where they sit in
/// it, with the write they must map to on the lab chat.
const SHAPED: &[(&str, &str, WriteKind)] = &[
    ("edit.json", "/request", WriteKind::Edit),
    ("delete.json", "/request", WriteKind::Delete),
    ("reply.json", "/request", WriteKind::Message),
    ("mention.json", "/request", WriteKind::Message),
    ("reaction.json", "/add/request", WriteKind::Reaction),
    ("reaction.json", "/remove/request", WriteKind::Reaction),
    ("reactions.json", "/set/0", WriteKind::Reaction),
    ("reactions.json", "/set/1", WriteKind::Reaction),
    ("reactions.json", "/set/2", WriteKind::Reaction),
    ("reactions.json", "/set/3", WriteKind::Reaction),
    ("reactions.json", "/taken_back/0", WriteKind::Reaction),
    ("reactions.json", "/taken_back/1", WriteKind::Reaction),
    ("reactions.json", "/taken_back/2", WriteKind::Reaction),
    ("reactions.json", "/taken_back/3", WriteKind::Reaction),
    ("lines.json", "/request", WriteKind::Message),
];

/// The request at `at` in the chat service fixture `name`: method, URL, body and the body as JSON.
fn shaped_request(name: &str, at: &str) -> TestResult<(String, String, Option<Vec<u8>>, Value)> {
    let path = format!("{}/../../tests/fixtures/chatsvc/{name}", env!("CARGO_MANIFEST_DIR"));
    let fixture: Value = serde_json::from_str(&fill(&std::fs::read_to_string(path)?))?;
    let request = fixture.pointer(at).ok_or("no request")?;
    let mut url = format!(
        "https://teams.microsoft.com{}",
        request["path"].as_str().ok_or("no path")?
    );
    if let Some(query) = request["query"].as_str() {
        url = format!("{url}?{query}");
    }
    let body = match &request["body"] {
        Value::Null => None,
        body => Some(serde_json::to_vec(body)?),
    };
    let method = request["method"].as_str().ok_or("no method")?.to_owned();
    Ok((method, url, body, request["body"].clone()))
}

fn verdict(method: &str, url: &str, body: Option<&[u8]>) -> Contact {
    contacting(&Request::new(method, url).with_body(body), &policy())
}

#[test]
fn the_capture_shaped_fixtures_map_to_their_writes_on_the_lab_chat() -> TestResult {
    for (name, at, kind) in SHAPED {
        let (method, url, body, json_body) = shaped_request(name, at)?;
        let mentioned = json_body["properties"]["mentions"]
            .as_str()
            .is_some_and(|mentions| mentions != "[]");
        let target =
            Target::new(*kind, Scope::Conversation { id: one_to_one() }).with_added(mentioned.then(|| lab_mri(GUID_B)));
        assert_eq!(
            verdict(&method, &url, body.as_deref()),
            Contact::Write(target),
            "{name} {at}"
        );
    }
    Ok(())
}

#[test]
fn a_mention_in_a_group_chat_is_a_message_there_adding_the_mentioned() -> TestResult {
    let (method, url, body, _) = shaped_request("mention-group.json", "/request")?;
    let target = Target::new(
        WriteKind::Message,
        Scope::Conversation {
            id: format!("19:{GROUP}@thread.v2"),
        },
    )
    .with_added([lab_mri(GUID_B)]);
    assert_eq!(verdict(&method, &url, body.as_deref()), Contact::Write(target));
    Ok(())
}

#[test]
fn a_mention_of_everyone_is_a_message_to_its_group_chat_adding_nobody() -> TestResult {
    let (method, url, body, _) = shaped_request("mention-everyone.json", "/request")?;
    let group = format!("19:{GROUP}@thread.v2");
    let target = Target::new(WriteKind::Message, Scope::Conversation { id: group.clone() });
    assert_eq!(verdict(&method, &url, body.as_deref()), Contact::Write(target));
    Ok(())
}

/// A mention of everyone reads only in its captured form: `mentionType` `everyone`, its `mri` the group chat it is
/// posted to. Naming another chat, posted to a 1:1 chat, or its key in another case, it is unreadable; a `mentionType`
/// in another case is no mention of everyone, so its `mri` is added as a participant (which no live member read takes).
#[test]
fn a_mention_of_everyone_in_any_other_form_is_unreadable_or_a_participant() {
    let group = format!("19:{GROUP}@thread.v2");
    let post = |chat: &str, entry: &str| {
        let mentions = serde_json::to_string(&format!("[{entry}]")).unwrap_or_default();
        let body = format!(
            r#"{{"content":"<p>x</p>","messagetype":"RichText/Html","contenttype":"Text","properties":{{"mentions":{mentions}}}}}"#
        );
        let url = format!("https://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/conversations/{chat}/messages");
        verdict("POST", &url, Some(body.as_bytes()))
    };
    let entry = |mri: &str, key: &str, kind: &str| {
        format!(r#"{{"itemid":"0","mri":"{mri}","{key}":"{kind}","displayName":"Everyone"}}"#)
    };
    let other = "19:fedcba9876543210fedcba9876543210@thread.v2";
    assert_eq!(
        post(&group, &entry(other, "mentionType", "everyone")),
        Contact::Unreadable
    );
    assert_eq!(
        post(&one_to_one(), &entry(&one_to_one(), "mentionType", "everyone")),
        Contact::Unreadable
    );
    assert_eq!(
        post(&group, &entry(&group, "MentionType", "everyone")),
        Contact::Unreadable
    );
    let capitalised = Target::new(WriteKind::Message, Scope::Conversation { id: group.clone() })
        .with_added([Mri::new(group.clone())]);
    assert_eq!(
        post(&group, &entry(&group, "mentionType", "Everyone")),
        Contact::Write(capitalised)
    );
}

#[test]
fn the_account_s_own_mri_reads_as_me_only_when_the_policy_names_it() {
    let url = format!("https://teams.microsoft.com/api/chatsvc/emea/v1/users/8:orgid:{GUID_A}/properties?name=locale");
    let request = Request::new("PUT", &url).with_body(Some(br#"{"locale":"en-us"}"#));
    assert_eq!(contacting(&request, &policy()), Contact::None);
    assert_eq!(contacting(&request, &Policy::new()), Contact::Unreadable);
}

/// Typing and a read receipt reach nobody on exactly the chat ids `Fence::check` permits them on without a member read
/// ([`is_chat_id`]); a chat-like id in any other form (a self 1:1, halves that aren't GUIDs, a `@thread.v2` id with `.`
/// or `=` or over 128 characters, `meeting_` with nothing after it) is unreadable.
#[test]
fn typing_and_read_receipts_reach_nobody_only_on_a_chat_id_the_fence_takes() {
    let chats = [
        one_to_one(),
        format!("19:{GROUP}@thread.v2"),
        "19:meeting_lab0001@thread.v2".to_owned(),
    ];
    let odd = [
        format!("19:{GUID_A}_{GUID_A}@unq.gbl.spaces"),
        "19:alice_bob@unq.gbl.spaces".to_owned(),
        "19:group.chat@thread.v2".to_owned(),
        "19:groupchat=@thread.v2".to_owned(),
        format!("19:{}@thread.v2", "a".repeat(129)),
        "19:meeting_@thread.v2".to_owned(),
    ];
    let base = "https://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/conversations";
    let typing = br#"{"content":"","contenttype":"Application/Message","messagetype":"Control/Typing"}"#;
    let horizon = br#"{"consumptionhorizon":"1;2;3"}"#;
    for (id, reaches_nobody) in chats
        .iter()
        .map(|id| (id, true))
        .chain(odd.iter().map(|id| (id, false)))
    {
        assert_eq!(is_chat_id(id), reaches_nobody, "{id}");
        let expect = if reaches_nobody {
            Contact::None
        } else {
            Contact::Unreadable
        };
        let posted = verdict("POST", &format!("{base}/{id}/messages"), Some(typing));
        assert_eq!(posted, expect, "typing on {id}");
        let receipt = verdict(
            "PUT",
            &format!("{base}/{id}/properties?name=consumptionhorizon"),
            Some(horizon),
        );
        assert_eq!(receipt, expect, "read receipt on {id}");
    }
}

#[test]
fn every_own_view_property_is_carved_out_on_a_chat() {
    for name in OWN_VIEW_PROPERTIES {
        let url = format!(
            "https://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/conversations/{}/properties?name={name}",
            one_to_one()
        );
        assert_eq!(verdict("PUT", &url, Some(b"{}")), Contact::None, "{name}");
    }
}

/// The bodies `proto-lab`'s `fence_targets.rs` pins as ones the chat service could read differently from the fence:
/// a thread's members (`POST threads`, `PUT threads/<chat>/members`), each unreadable.
fn ambiguous_members() -> Vec<String> {
    let lab = lab_mri(GUID_A);
    let lab = lab.as_str();
    let outsider = lab_mri(GUID_X);
    let outsider = outsider.as_str();
    vec![
        // a key in another case beside it: a case-insensitive reader could bind either
        format!(r#"{{"members":[{{"id":"{lab}"}}],"Members":[{{"id":"{outsider}"}}]}}"#),
        format!(r#"{{"Members":[{{"id":"{outsider}"}}],"members":[{{"id":"{lab}"}}]}}"#),
        format!(r#"{{"members":[{{"id":"{lab}","Id":"{outsider}"}}]}}"#),
        format!(r#"{{"members":[{{"ID":"{outsider}","id":"{lab}"}}]}}"#),
        // the same key twice: a reader could keep either
        format!(r#"{{"members":[{{"id":"{lab}"}}],"members":[{{"id":"{outsider}"}}]}}"#),
        format!(r#"{{"members":[{{"id":"{outsider}","id":"{lab}"}}]}}"#),
        // a case-insensitive key alone, which the fence would miss
        format!(r#"{{"Members":[{{"id":"{outsider}"}}]}}"#),
        format!(r#"{{"members":[{{"ID":"{outsider}"}}]}}"#),
        // `ſ` folds to `s` in a case-insensitive comparison
        format!(r#"{{"members":[{{"id":"{lab}"}}],"memberſ":[{{"id":"{outsider}"}}]}}"#),
        // a duplicate anywhere, even off the read path
        format!(r#"{{"members":[{{"id":"{lab}","properties":{{"x":1,"X":2}}}}]}}"#),
        // unexpected shapes
        format!(r#"{{"members":{{"id":"{lab}"}}}}"#),
        format!(r#"{{"members":["{lab}"]}}"#),
        format!(r#"{{"members":[{{"id":"{lab}"}},null]}}"#),
        format!(r#"{{"members":[{{"id":["{lab}"]}}]}}"#),
        format!(r#"[{{"members":[{{"id":"{lab}"}}]}}]"#),
        format!(r#"{{"members":[{{"id":"{lab}"}}]}} {{"members":[{{"id":"{outsider}"}}]}}"#),
        format!(r#"{{"members":[{{"id":"{lab}"}}],}}"#),
        r#"{"members":[]}"#.to_owned(),
    ]
}

#[test]
fn a_body_the_chat_service_could_read_differently_is_unreadable() {
    let threads = "https://teams.microsoft.com/api/chatsvc/emea/v1/threads";
    let members = format!("https://teams.microsoft.com/api/chatsvc/emea/v1/threads/19%3A{GROUP}%40thread.v2/members");
    let messages = format!(
        "https://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/conversations/{}/messages",
        one_to_one()
    );
    // the clean shapes map
    let clean = format!(
        r#"{{"members":[{{"id":"{}","role":"Admin"}}]}}"#,
        lab_mri(GUID_A).as_str()
    );
    assert!(matches!(
        verdict("POST", threads, Some(clean.as_bytes())),
        Contact::Write(_)
    ));
    assert!(matches!(
        verdict("PUT", &members, Some(clean.as_bytes())),
        Contact::Write(_)
    ));
    let message = br#"{"messagetype":"Text","content":"hi"}"#;
    assert!(matches!(verdict("POST", &messages, Some(message)), Contact::Write(_)));

    for body in ambiguous_members() {
        assert_eq!(
            verdict("POST", threads, Some(body.as_bytes())),
            Contact::Unreadable,
            "{body}"
        );
        assert_eq!(
            verdict("PUT", &members, Some(body.as_bytes())),
            Contact::Unreadable,
            "{body}"
        );
    }
    let ambiguous_messages = [
        r#"{"messagetype":"Text","MessageType":"Control/Typing"}"#,
        r#"{"messagetype":"Control/Typing","messagetype":"Text"}"#,
        r#"{"MessageType":"Text"}"#,
        r#"{"messagetype":"Text","content":"hi","properties":{"a":1,"A":2}}"#,
        r#"{"messagetype":"Text"} {"messagetype":"Text"}"#,
        r#"["messagetype","Text"]"#,
    ];
    for body in ambiguous_messages {
        assert_eq!(
            verdict("POST", &messages, Some(body.as_bytes())),
            Contact::Unreadable,
            "{body}"
        );
    }
}

#[test]
fn a_write_is_mapped_only_on_the_chat_service_hosts() {
    let body = br#"{"messagetype": "Text", "content": "hi"}"#;
    let chat = format!("19%3A{GUID_A}_{GUID_B}%40unq.gbl.spaces");
    let path = format!("v1/users/ME/conversations/{chat}/messages");
    for url in [
        format!("https://teams.microsoft.com/api/chatsvc/emea/{path}"),
        format!("https://teams.cloud.microsoft/api/chatsvc/amer/{path}"),
        format!("https://emea.ng.msg.teams.microsoft.com/{path}"),
    ] {
        assert!(matches!(verdict("POST", &url, Some(body)), Contact::Write(_)), "{url}");
    }
    let unreadable = [
        format!("http://teams.microsoft.com/api/chatsvc/emea/{path}"),
        format!("https://teams.microsoft.com:444/api/chatsvc/emea/{path}"),
        format!("https://teams.microsoft.com/api/mt/emea/{path}"),
        format!("https://msg.teams.microsoft.com/{path}"),
        format!("https://emea.ng.msg.teams.microsoft.com/api/{path}"),
        format!("https://chatsvcagg.teams.microsoft.com/{path}"),
        format!("https://graph.microsoft.com/v1.0/chats/{chat}/messages"),
        // the live read's checks: a region of lowercase letters, digits and `-`, every regional host label sound
        format!("https://teams.microsoft.com/api/chatsvc/EMEA/{path}"),
        format!("https://teams.microsoft.com/api/chatsvc/x%20y/{path}"),
        format!("https://teams.microsoft.com/api/chatsvc//{path}"),
        format!("https://a_b.msg.teams.microsoft.com/{path}"),
        format!("https://a..b.msg.teams.microsoft.com/{path}"),
    ];
    for url in &unreadable {
        assert_eq!(verdict("POST", url, Some(body)), Contact::Unreadable, "{url}");
    }
    // a lookalike under another domain is no contacting service at all
    let lookalike = format!("https://teams.microsoft.com.example.com/api/chatsvc/emea/{path}");
    assert_eq!(verdict("POST", &lookalike, Some(body)), Contact::None);
}

#[test]
fn a_request_s_debug_shows_no_header_value_query_or_body() {
    let request = Request::new(
        "POST",
        "https://user:pass@teams.microsoft.com:8443/api/x?sig=query-secret#frag",
    )
    .with_header("Authorization", "Bearer header-secret")
    .with_body(Some(b"body-secret"));
    let shown = format!("{request:?}");
    for secret in ["pass", "header-secret", "query-secret", "sig", "frag", "body-secret"] {
        assert!(!shown.contains(secret), "{secret} in {shown}");
    }
    for kept in ["POST", "https://teams.microsoft.com:8443/api/x", "Authorization", "11"] {
        assert!(shown.contains(kept), "{kept} not in {shown}");
    }
}

// --- the test call start ---

const CALLER: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";

/// A call start as the web client posts it (made-up ids and links, the SDP cut to a stub), with `to` and the
/// conversation's scenario filled in.
fn call_start(to: &str, scenario: &str) -> String {
    format!(
        r#"{{"conversationRequest":{{"conversationType":null,"subject":null,"suppressDialout":true,"roster":{{"type":"Delta","rosterUpdate":"https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/x/callAgent/y/1/conversation/rosterUpdate/"}},"properties":{{"allowConversationWithoutHost":true}},"links":{{}}{scenario}}},"contentSharing":null,"participants":{{"from":{{"id":"{CALLER}","displayName":"Lab A","endpointId":"00000000-0000-4000-8000-000000000001","participantId":"00000000-0000-4000-8000-000000000002","languageId":"en-us"}},"to":[{to}]}},"capabilities":null,"endpointCapabilities":1,"clientEndpointCapabilities":1,"endpointMetadata":{{}},"groupContext":null,"groupChat":null,"meetingInfo":null,"meetingData":null,"endpointState":{{}},"callInvitation":{{"callModalities":["Audio"],"links":{{}},"mediaContent":{{"blob":"v=0\r\n","contentType":"application/sdp-ngc-1.0"}}}},"debugContent":{{}},"participantPropertyBag":{{}}}}"#
    )
}

fn test_call() -> String {
    call_start("", &format!(r#","scenario":"{TEST_CALL_SCENARIO}""#))
}

fn start_verdict(body: &str, policy: &Policy) -> Contact {
    contacting(
        &Request::new("POST", CALL_START_URL).with_body(Some(body.as_bytes())),
        policy,
    )
}

// --- a meeting's call start ---

const MEETING_CHAT: &str = "19:meeting_MadeUpMeeting0001@thread.v2";
const TENANT: &str = "00000000-0000-4000-8000-0000000000f1";

/// A meeting's call start as the web client posts it from its pre-join (made-up ids and links, the SDP cut to a stub).
fn meeting_start() -> String {
    format!(
        r#"{{"conversationRequest":{{"conversationType":null,"subject":null,"suppressDialout":true,"roster":{{"type":"Delta","rosterUpdate":"https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/x/callAgent/y/1/conversation/rosterUpdate/"}},"properties":{{"allowConversationWithoutHost":true}},"links":{{}}}},"contentSharing":null,"participants":{{"from":{{"id":"{CALLER}","displayName":"Lab A"}},"to":[]}},"capabilities":null,"endpointCapabilities":1,"clientEndpointCapabilities":1,"endpointMetadata":{{}},"groupContext":null,"groupChat":{{"threadId":"{MEETING_CHAT}","messageId":"0"}},"meetingInfo":{{"tenantId":"{TENANT}","organizerId":"00000000-0000-4000-8000-00000000000a","meetingType":1}},"meetingData":null,"endpointState":{{}},"callInvitation":{{"callModalities":["Audio","ScreenViewer"],"links":{{}},"mediaContent":{{"blob":"v=0\r\n","contentType":"application/sdp-ngc-1.0"}}}},"debugContent":{{}},"participantPropertyBag":{{}}}}"#
    )
}

fn meeting_target() -> Target {
    Target::new(
        WriteKind::Call,
        Scope::Conversation {
            id: MEETING_CHAT.to_owned(),
        },
    )
    .with_added([Mri::new(CALLER)])
    .with_sender(Mri::new(CALLER))
}

#[test]
fn a_meetings_start_is_a_call_in_its_chat_by_the_sender_alone() {
    assert_eq!(
        start_verdict(&meeting_start(), &Policy::new()),
        Contact::Write(meeting_target())
    );
}

#[test]
fn a_meetings_subscription_from_its_pre_join_is_the_same_write() {
    let start: serde_json::Value = serde_json::from_str(&meeting_start()).unwrap();
    let mut subscription = start.clone();
    let object = subscription.as_object_mut().unwrap();
    object.remove("callInvitation");
    object.remove("contentSharing");
    object.remove("meetingData");
    object["participants"].as_object_mut().unwrap().remove("to");
    object["meetingInfo"].as_object_mut().unwrap().remove("meetingType");
    assert_eq!(
        start_verdict(&subscription.to_string(), &Policy::new()),
        Contact::Write(meeting_target())
    );
}

#[test]
fn a_meetings_start_off_its_captured_shape_is_unreadable() {
    let base = meeting_start();
    let off = [
        // someone named to ring, in a chat or a meeting
        base.replace(
            r#""to":[]"#,
            r#""to":[{"id":"8:orgid:00000000-0000-4000-8000-00000000000b"}]"#,
        ),
        // a chat that isn't a meeting's
        base.replace(MEETING_CHAT, "19:abcdef0123456789@thread.v2"),
        base.replace(MEETING_CHAT, "19:abcdef0123456789@thread.tacv2"),
        // a channel's message, an organizer or a tenant that doesn't read
        base.replace(r#""messageId":"0""#, r#""messageId":"1700000000000""#),
        base.replace(
            r#""organizerId":"00000000-0000-4000-8000-00000000000a""#,
            r#""organizerId":"nobody""#,
        ),
        base.replace(TENANT, "nowhere"),
        // no meeting named at all
        base.replace(r#""meetingInfo":{"#, r#""meeting":{"#),
        // a sender that isn't a work account
        base.replace(&format!(r#""id":"{CALLER}""#), r#""id":"28:bot""#),
        // a key nobody reviewed
        base.replace(r#""meetingData":null"#, r#""meetingData":{"meetingCode":"1"}"#),
    ];
    for body in &off {
        assert_eq!(start_verdict(body, &Policy::new()), Contact::Unreadable, "{body}");
    }
}

#[test]
fn the_test_call_start_is_a_call_with_the_caller_and_the_echo_service() {
    let expected = Target::new(WriteKind::Call, Scope::New)
        .with_added([Mri::new(CALLER), Mri::new(ECHO)])
        .with_sender(Mri::new(CALLER));
    assert_eq!(start_verdict(&test_call(), &policy()), Contact::Write(expected.clone()));
    let named = call_start(
        &format!(r#"{{"id":"{ECHO}"}}"#),
        &format!(r#","scenario":"{TEST_CALL_SCENARIO}""#),
    );
    assert_eq!(start_verdict(&named, &policy()), Contact::Write(expected));
}

#[test]
fn a_test_call_start_is_unreadable_without_the_echo_service_in_the_policy() {
    assert_eq!(start_verdict(&test_call(), &Policy::new()), Contact::Unreadable);
}

#[test]
fn a_call_start_off_the_captured_shape_or_to_anyone_else_is_unreadable() {
    let scenario = format!(r#","scenario":"{TEST_CALL_SCENARIO}""#);
    let lab_b = r#"{"id":"8:orgid:00000000-0000-4000-8000-00000000000b"}"#;
    let off = [
        // a call to a lab account in the test call's scenario, or with the echo service besides
        call_start(lab_b, &scenario),
        call_start(&format!(r#"{{"id":"{ECHO}"}},{lab_b}"#), &scenario),
        call_start("", r#","scenario":"SomethingElse""#),
        test_call().replace(r#""groupChat":null"#, r#""groupChat":{"threadId":"19:x@thread.v2"}"#),
        test_call().replace(r#""to":[]"#, r#""to":[],"cc":[{"id":"8:orgid:x"}]"#),
        test_call().replace(r#""groupChat":null"#, r#""groupChat":null,"GroupChat":{}"#),
        format!("{}{{}}", test_call()),
    ];
    for body in &off {
        assert_eq!(start_verdict(body, &policy()), Contact::Unreadable, "{body}");
    }
    for url in [
        format!("{CALL_START_URL}?x=1"),
        format!("{CALL_START_URL}/x"),
        CALL_START_URL.replace("https:", "http:"),
    ] {
        let body = test_call();
        let request = Request::new("POST", &url).with_body(Some(body.as_bytes()));
        assert_eq!(contacting(&request, &policy()), Contact::Unreadable, "{url}");
    }
    assert_eq!(
        contacting(
            &Request::new("PUT", CALL_START_URL).with_body(Some(test_call().as_bytes())),
            &policy()
        ),
        Contact::Unreadable
    );
}

// --- a call start from a chat (`tests/fixtures/calling/chat/starts.json`) ---

const CHAT_STARTS: &str = include_str!("../../../tests/fixtures/calling/chat/starts.json");
const GUID_C: &str = "00000000-0000-4000-8000-00000000000c";

/// The chat call start fixture's cases, its placeholders filled in.
fn chat_start_cases() -> TestResult<Vec<Value>> {
    let mut text = fill(CHAT_STARTS).replace("<guid:c>", GUID_C);
    for n in 2..=7 {
        text = text.replace(
            &format!("<guid:{n}>"),
            &format!("00000000-0000-4000-8000-00000000010{n}"),
        );
    }
    let fixture: Value = serde_json::from_str(&text)?;
    Ok(fixture["cases"].as_array().ok_or("cases")?.clone())
}

/// The 1:1 start of the fixture (a calls b), as JSON to change.
fn one_to_one_start() -> TestResult<Value> {
    Ok(chat_start_cases()?[0]["body"].clone())
}

/// The group start of the fixture (a calls b and c in the group chat), as JSON to change.
fn group_start() -> TestResult<Value> {
    Ok(chat_start_cases()?[1]["body"].clone())
}

#[test]
fn a_call_start_from_a_chat_is_a_call_in_the_chat_it_carries_with_the_caller_and_every_callee() -> TestResult {
    for case in chat_start_cases()? {
        let body = case["body"].to_string();
        let added: Vec<Mri> = case["added"]
            .as_array()
            .ok_or("added")?
            .iter()
            .map(|mri| mri.as_str().map(Mri::new).ok_or("an MRI"))
            .collect::<Result<_, _>>()?;
        let expected = Target::new(
            WriteKind::Call,
            Scope::Conversation {
                id: case["chat"].as_str().ok_or("chat")?.to_owned(),
            },
        )
        .with_added(added)
        .with_sender(Mri::new(
            case["body"]
                .pointer("/participants/from/id")
                .and_then(Value::as_str)
                .ok_or("a caller")?,
        ));
        assert_eq!(
            start_verdict(&body, &policy()),
            Contact::Write(expected.clone()),
            "{}",
            case["name"]
        );
        // no echo service needed: it calls lab accounts in a chat
        assert_eq!(
            start_verdict(&body, &Policy::new()),
            Contact::Write(expected),
            "{}",
            case["name"]
        );
    }
    // the 1:1 chat is named by the two accounts as the chat service names it, whichever calls
    let mut b_calls_a = one_to_one_start()?;
    b_calls_a["participants"]["from"]["id"] = Value::from(format!("8:orgid:{GUID_B}"));
    b_calls_a["participants"]["to"][0]["id"] = Value::from(format!("8:orgid:{GUID_A}"));
    let Contact::Write(target) = start_verdict(&b_calls_a.to_string(), &policy()) else {
        return Err("b's call to a isn't a write".into());
    };
    assert_eq!(target.scope(), &Scope::Conversation { id: one_to_one() });
    Ok(())
}

#[test]
fn a_call_start_from_a_chat_naming_someone_outside_the_lab_is_still_read_for_the_fence_to_deny() -> TestResult {
    // the callee's form is a work account's: the fence judges who it is (`tests/calls.rs`)
    let mut start = group_start()?;
    start["participants"]["to"][1]["id"] = Value::from(format!("8:orgid:{GUID_X}"));
    let Contact::Write(target) = start_verdict(&start.to_string(), &policy()) else {
        return Err("a group start naming a coworker isn't a write".into());
    };
    assert!(target.added().contains(&lab_mri(GUID_X)));
    Ok(())
}

/// The fixture's `start` with the value at the JSON pointer `at` set to `value`, or removed when `value` is `None`.
fn changed(start: &Value, at: &str, value: Option<Value>) -> String {
    let mut start = start.clone();
    let (parent, key) = at.rsplit_once('/').unwrap_or_default();
    if let Some(object) = start.pointer_mut(parent).and_then(Value::as_object_mut) {
        match value {
            Some(value) => drop(object.insert(key.to_owned(), value)),
            None => drop(object.remove(key)),
        }
    }
    start.to_string()
}

#[test]
fn a_call_start_from_a_chat_off_its_captured_shape_is_unreadable() -> TestResult {
    let one = one_to_one_start()?;
    let group = group_start()?;
    let callee = |guid: &str| serde_json::json!({"id": format!("8:orgid:{guid}"), "participantId": GUID_X});
    let callees = |guids: &[&str]| Some(Value::from(guids.iter().map(|guid| callee(guid)).collect::<Vec<_>>()));
    let text = |text: &str| Some(Value::from(text));
    let changes = [
        // the chat it is tied to can't be told
        (&one, "/groupChat", None),
        (&one, "/participants/to", callees(&[GUID_B, GUID_X])),
        (&group, "/groupChat/threadId", text(&one_to_one())),
        (&group, "/groupChat/threadId", text("19:abc@thread.tacv2")),
        (
            &group,
            "/groupChat/threadId",
            text(&format!("19:meeting_{GROUP}@thread.v2")),
        ),
        (&group, "/groupChat/messageId", text("1")),
        (&group, "/groupChat/parentMessageId", Some(Value::Null)),
        (&group, "/groupChat/threadId", None),
        // its callees: none, twice the same, the caller, or not a work account's id
        (&group, "/participants/to", callees(&[])),
        (&group, "/participants/to", callees(&[GUID_B, GUID_B])),
        (&group, "/participants/to", callees(&[GUID_B, GUID_A])),
        (&group, "/participants/to/0/id", text(ECHO)),
        (&group, "/participants/to/0/id", text("8:orgid:not-a-guid")),
        (&one, "/participants/to", None),
        (&one, "/participants/from/id", text("8:live:someone")),
        // a call into a meeting, a group context, another scenario, a key nobody reviewed
        (&one, "/meetingInfo", Some(serde_json::json!({"organizerId": GUID_A}))),
        (&one, "/groupContext", Some(serde_json::json!({}))),
        (&one, "/conversationRequest/scenario", text("SomethingElse")),
        (&one, "/conversationRequest/scenario", text(TEST_CALL_SCENARIO)),
        (&one, "/callInvitation/replaces", text("call-1")),
        (
            &one,
            "/callInvitation/mediaContent/requiredFeatures",
            Some(Value::from(1)),
        ),
        (&one, "/unreviewed", Some(Value::Null)),
    ];
    let off: Vec<String> = changes
        .into_iter()
        .map(|(start, at, value)| changed(start, at, value))
        .collect();
    for body in &off {
        assert_eq!(start_verdict(body, &policy()), Contact::Unreadable, "{body}");
    }
    let body = one.to_string();
    for (method, url) in [
        ("POST", format!("{CALL_START_URL}?x=1")),
        ("POST", format!("{CALL_START_URL}/x")),
        ("PUT", CALL_START_URL.to_owned()),
    ] {
        let request = Request::new(method, &url).with_body(Some(body.as_bytes()));
        assert_eq!(contacting(&request, &policy()), Contact::Unreadable, "{method} {url}");
    }
    Ok(())
}

const CHAT_FOLLOW_UPS: &str = include_str!("../../../tests/fixtures/calling/chat/follow_ups.json");

#[test]
fn the_caller_s_cancel_while_its_callee_rings_is_its_leave() -> TestResult {
    let fixture: Value = serde_json::from_str(&fill(CHAT_FOLLOW_UPS).replace("<guid:3>", GUID_X))?;
    let case = &fixture["cases"][0];
    let (method, url) = (
        case["method"].as_str().ok_or("method")?,
        case["url"].as_str().ok_or("url")?,
    );
    let verdict = |body: &Value| {
        let body = body.to_string();
        contacting(&Request::new(method, url).with_body(Some(body.as_bytes())), &policy())
    };
    let Contact::Write(target) = verdict(&case["body"]) else {
        return Err("the cancel's leave isn't a write".into());
    };
    let Scope::CallOperation { operation, from, .. } = target.scope() else {
        return Err("the cancel's leave isn't a call operation".into());
    };
    assert_eq!(*operation, CallOperation::Leave);
    assert_eq!(from.as_ref(), Some(&lab_mri(GUID_A)));
    // only the diagnostics the capture had
    let mut other = case["body"].clone();
    other["callTransactionEnd"]["callQualityDiagnosticsInformation"]["networkScore"] = Value::from(1);
    assert_eq!(verdict(&other), Contact::Unreadable);
    let mut text = case["body"].clone();
    other = text.clone();
    other["callTransactionEnd"]["callQualityDiagnosticsInformation"]["cancelationDuration"] = Value::from("9");
    assert_eq!(verdict(&other), Contact::Unreadable);
    text["callTransactionEnd"]["callQualityDiagnosticsInformation"] = Value::Null;
    assert_eq!(verdict(&text), Contact::Unreadable);
    Ok(())
}

#[test]
fn the_callee_s_attach_while_it_rings_is_a_ring_time_send_on_its_exact_url() -> TestResult {
    let fixture: Value = serde_json::from_str(&fill(CHAT_FOLLOW_UPS))?;
    let case = &fixture["cases"][1];
    let (method, url) = (
        case["method"].as_str().ok_or("method")?,
        case["url"].as_str().ok_or("url")?,
    );
    let verdict = |method: &str, body: &Value| {
        let body = body.to_string();
        contacting(&Request::new(method, url).with_body(Some(body.as_bytes())), &policy())
    };
    let Contact::Write(target) = verdict(method, &case["body"]) else {
        return Err("the attach isn't a write".into());
    };
    assert_eq!(target.kind(), WriteKind::CallRinging);
    assert!(target.added().is_empty());
    let Scope::CallOperation {
        operation,
        url: at,
        from,
        callbacks,
        joins,
    } = target.scope()
    else {
        return Err("the attach isn't a call operation".into());
    };
    assert_eq!(*operation, CallOperation::Attach);
    assert_eq!(at.as_str(), Url::parse(url)?.as_str());
    assert_eq!(from.as_ref(), Some(&lab_mri(GUID_B)));
    let action = &case["body"]["additionalActions"][0];
    assert_eq!(joins.as_deref(), action["url"].as_str());
    // the leg's end, the roster's and the conversation's six callbacks
    assert_eq!(callbacks.len(), 8);
    assert!(callbacks.iter().any(|link| link.ends_with("/call/end/")));
    // only the captured method and shape: one `join` action that waits, no media, no other action, no other key
    assert_eq!(verdict("PUT", &case["body"]), Contact::Unreadable);
    let edits: [(&str, BodyEdit); 7] = [
        (
            "a second action",
            Box::new(|body| {
                let action = body["additionalActions"][0].clone();
                if let Some(actions) = body["additionalActions"].as_array_mut() {
                    actions.push(action);
                }
            }),
        ),
        (
            "no action",
            Box::new(|body| body["additionalActions"] = Value::Array(Vec::new())),
        ),
        (
            "another action",
            Box::new(|body| body["additionalActions"][0]["name"] = Value::from("accept")),
        ),
        (
            "not waiting",
            Box::new(|body| body["additionalActions"][0]["waitForResponse"] = Value::from(false)),
        ),
        (
            "media in the join",
            Box::new(|body| {
                body["additionalActions"][0]["input"]["callInvitation"] = serde_json::json!({"mediaContent": {}});
            }),
        ),
        (
            "an acceptance",
            Box::new(|body| body["callAcceptance"] = serde_json::json!({})),
        ),
        (
            "no sender",
            Box::new(|body| body["additionalActions"][0]["input"]["participants"] = serde_json::json!({})),
        ),
    ];
    for (name, edit) in edits {
        let mut body = case["body"].clone();
        edit(&mut body);
        assert_eq!(verdict(method, &body), Contact::Unreadable, "{name}");
    }
    Ok(())
}

// --- the operations on a call (the call registry resolves them: `tests/calls.rs`) ---

const ECHO_REQUESTS: &str = include_str!("../../../tests/fixtures/calling/echo/requests.json");

#[test]
fn the_echo_call_s_follow_ups_are_operations_on_their_exact_urls() -> TestResult {
    let fixture: Value = serde_json::from_str(
        &fill(ECHO_REQUESTS)
            .replace("<guid:echo>", "00000000-0000-4000-8000-0000000000e0")
            .replace("<guid:5>", GUID_X),
    )?;
    let mut operations = Vec::new();
    for case in fixture["cases"].as_array().ok_or("cases")? {
        let Some(expected) = case["expected"]
            .as_str()
            .and_then(|name| name.strip_prefix("call_operation|"))
        else {
            continue;
        };
        let (method, url) = (
            case["method"].as_str().ok_or("method")?,
            case["url"].as_str().ok_or("url")?,
        );
        let body = case["body"].to_string();
        let Contact::Write(target) = contacting(&Request::new(method, url).with_body(Some(body.as_bytes())), &policy())
        else {
            return Err(format!("{} isn't a write", case["name"]).into());
        };
        let Scope::CallOperation {
            operation,
            url: at,
            from,
            ..
        } = target.scope()
        else {
            return Err(format!("{} isn't a call operation", case["name"]).into());
        };
        let mut parts = expected.split('|');
        assert_eq!(Some(operation.as_str()), parts.next(), "{}", case["name"]);
        assert_eq!(at.as_str(), Url::parse(url)?.as_str());
        assert_eq!(from.as_ref().map(Mri::as_str), parts.next(), "{}", case["name"]);
        let invitees: Vec<&str> = target.added().iter().map(Mri::as_str).collect();
        assert_eq!(invitees, parts.collect::<Vec<_>>(), "{}", case["name"]);
        assert_eq!(target.kind(), operation.kind());
        operations.push(*operation);
    }
    operations.sort();
    let mut echo = vec![
        CallOperation::UpdateEndpointMetadata,
        CallOperation::Add,
        CallOperation::Leave,
        CallOperation::Acknowledge,
        CallOperation::ApplyChannelParameters,
    ];
    echo.sort();
    assert_eq!(
        operations, echo,
        "every operation the echo call's capture showed has its row"
    );
    Ok(())
}

// --- the services, the method and the write kinds ---

/// URLs and the contacting service each goes to, if any.
const SERVICES_BY_URL: &[(&str, Option<Service>)] = &[
    (
        "https://teams.microsoft.com/api/chatsvc/amer/v1/threads",
        Some(Service::ChatService),
    ),
    (
        "https://amer.ng.msg.teams.microsoft.com/v1/threads",
        Some(Service::ChatService),
    ),
    (
        "https://TEAMS.MICROSOFT.COM./Api/ChatSvc/amer/v1",
        Some(Service::ChatService),
    ),
    (
        "https://teams.live.com/api/chatsvc/consumer/v1/threads",
        Some(Service::ChatService),
    ),
    (
        "https://api.flightproxy.teams.microsoft.com/api/v2/epconv",
        Some(Service::Calling),
    ),
    (
        "https://api.fl.teams.microsoft.com/api/v2/epconv",
        Some(Service::Calling),
    ),
    (
        "https://conv-zz-01-prod-aks.conv.skype.com/conv/x",
        Some(Service::Calling),
    ),
    ("https://cc-zz-01.cc.skype.com/cc/v1/x", Some(Service::Calling)),
    ("https://broker-zz-01.broker.skype.com/api/v1/x", Some(Service::Calling)),
    ("https://graph.microsoft.com/v1.0/$batch", Some(Service::Graph)),
    (
        "https://outlook.office.com/api/v2.0/me/sendmail",
        Some(Service::Outlook),
    ),
    ("https://outlook.office365.com/owa/", Some(Service::Outlook)),
    (
        "https://outlook.cloud.microsoft/owa/service.svc",
        Some(Service::Outlook),
    ),
    ("https://outlook.live.com/owa/", Some(Service::Outlook)),
    (
        "https://substrate.office.com/search/api/v1/suggestions",
        Some(Service::Outlook),
    ),
    (
        "https://contoso-my.sharepoint.com/personal/x",
        Some(Service::SharePoint),
    ),
    ("https://westus1-mediap.svc.ms/x", Some(Service::SharePoint)),
    ("https://onedrive.live.com/x", Some(Service::SharePoint)),
    ("https://api.onedrive.com/v1.0/x", Some(Service::SharePoint)),
    (
        "https://contoso-my.sharepoint-df.com/personal/x",
        Some(Service::SharePoint),
    ),
    ("https://my.microsoftpersonalcontent.com/x", Some(Service::SharePoint)),
    (
        "https://us-prod.asyncgw.teams.microsoft.com/v1/objects/",
        Some(Service::Ams),
    ),
    ("https://us-api.asm.skype.com/v1/objects/", Some(Service::Ams)),
    ("https://teams.microsoft.com/asm/emea/v1/objects", Some(Service::Ams)),
    (
        "https://teams.microsoft.com/api/mt/part/amer-03/beta/users/fetch",
        Some(Service::MiddleTier),
    ),
    (
        "https://teams.cloud.microsoft/api/apps/part/amer-03/beta/x",
        Some(Service::MiddleTier),
    ),
    ("https://teams.microsoft.com/api/csa/amer/api/v1/x", Some(Service::Csa)),
    ("https://chatsvcagg.teams.microsoft.com/api/v1/x", Some(Service::Csa)),
    (
        "https://teams.cloud.microsoft/fabric/amer/templates/api/teams/x/channels",
        Some(Service::Provisioning),
    ),
    (
        "https://templates.teams.microsoft.com/api/x",
        Some(Service::Provisioning),
    ),
    // reaching nobody: presence, the registrar, push, auth, telemetry, Trouter, the page
    ("https://teams.microsoft.com/ups/noam/v1/me/endpoints/", None),
    ("https://teams.microsoft.com/registrar/prod/V2/registrations", None),
    ("https://teams.microsoft.com/api/tps/api/v1/x", None),
    ("https://teams.microsoft.com/api/authsvc/v1.0/authz", None),
    ("https://login.microsoftonline.com/common/oauth2/v2.0/token", None),
    ("https://teams.events.data.microsoft.com/OneCollector/1.0/", None),
    ("https://go.trouter.teams.microsoft.com/v4/c", None),
    ("https://teams.microsoft.com/v2/", None),
    // lookalikes under other domains, and hosts that aren't names
    ("https://notgraph.microsoft.com/v1.0/$batch", None),
    ("https://graph.microsoft.com.example.com/v1.0/$batch", None),
    ("https://evilskype.com/x", None),
    ("https://127.0.0.1/api/chatsvc/amer/v1/threads", None),
    // a Teams origin path is told by its first two segments, each decoded on its own: a later one that doesn't
    // decode leaves the service to its row
    (
        "https://teams.microsoft.com/api/chatsvc/emea/v1/threads/%FF",
        Some(Service::ChatService),
    ),
    (
        "https://teams.microsoft.com/api/mt/part/amer-03/beta/users/%C0",
        Some(Service::MiddleTier),
    ),
    // first segments that can't be read name no service (and a mutating request there is unreadable)
    ("https://teams.microsoft.com//api/chatsvc/emea/v1/threads", None),
    ("https://teams.microsoft.com/api/chatsvc%2Femea/v1/threads", None),
    ("https://teams.microsoft.com/%FF/chatsvc/emea/v1/threads", None),
];

#[test]
fn each_contacting_service_is_told_by_its_host_or_path() -> TestResult {
    for (url, service) in SERVICES_BY_URL {
        assert_eq!(service_of(&Url::parse(url)?), *service, "{url}");
    }
    Ok(())
}

#[test]
fn a_socket_is_on_a_contacting_service_by_the_same_host_and_path_rules() -> TestResult {
    for (url, service) in SERVICES_BY_URL {
        let url = Url::parse(url)?;
        if service.is_some() {
            assert!(on_contacting_service(&url), "{url}");
        }
        // the scheme plays no part: a socket's URL is read as a request's
        let mut socket = url.clone();
        let scheme = if url.scheme() == "http" { "ws" } else { "wss" };
        if socket.set_scheme(scheme).is_ok() {
            assert_eq!(on_contacting_service(&socket), on_contacting_service(&url), "{socket}");
        }
    }
    for url in [
        // first segments that can't be read: where a frame could reach people
        "wss://teams.microsoft.com//api/chatsvc/emea/v1/threads",
        "wss://teams.microsoft.com/api/chatsvc%2Femea/v1",
        "wss://api.flightproxy.teams.microsoft.com/",
        "wss://go.skype.com/x",
    ] {
        assert!(on_contacting_service(&Url::parse(url)?), "{url}");
    }
    for url in [
        "wss://go.trouter.teams.microsoft.com/v4/c",
        "wss://augloop.office.com/",
        "wss://teams.microsoft.com/v2/",
        "ws://127.0.0.1:9/echo",
    ] {
        assert!(!on_contacting_service(&Url::parse(url)?), "{url}");
    }
    Ok(())
}

#[test]
fn a_request_is_mutating_by_its_method_its_body_or_a_method_override() {
    let url = "https://example.com/";
    for method in ["GET", "get", "HEAD", "OPTIONS"] {
        assert!(!Request::new(method, url).is_mutating(), "{method}");
    }
    for method in ["POST", "PUT", "PATCH", "DELETE", "MERGE", "PROPFIND", ""] {
        assert!(Request::new(method, url).is_mutating(), "{method}");
    }
    assert!(Request::new("GET", url).with_body(Some(b"")).is_mutating());
    for header in ["X-HTTP-Method-Override", "x-http-method", "X-Method-Override"] {
        assert!(
            Request::new("GET", url).with_header(header, "DELETE").is_mutating(),
            "{header}"
        );
        assert!(
            !Request::new("GET", url).with_header(header, " get ").is_mutating(),
            "{header}"
        );
    }
    assert!(!Request::new("GET", url).with_header("X-Other", "DELETE").is_mutating());
}

#[test]
fn only_typing_read_receipts_and_own_availability_are_not_contacting() {
    let quiet = [WriteKind::Typing, WriteKind::ReadReceipt, WriteKind::OwnAvailability];
    for kind in WriteKind::ALL {
        assert_eq!(kind.is_contacting(), !quiet.contains(&kind), "{kind}");
    }
}

#[test]
fn a_keep_alive_is_an_empty_participant_update_to_a_call_leg_s_own_link_naming_nobody() -> TestResult {
    let fixture: Value = serde_json::from_str(&fill(CHAT_FOLLOW_UPS))?;
    let cases = fixture["cases"].as_array().ok_or("cases")?;
    let keep_alives: Vec<&Value> = cases
        .iter()
        .filter(|case| case["name"].as_str().is_some_and(|name| name.contains("keep-alive")))
        .collect();
    assert_eq!(keep_alives.len(), 2, "the caller's and the callee's");
    let verdict = |method: &str, at: &str, body: &str| {
        contacting(&Request::new(method, at).with_body(Some(body.as_bytes())), &policy())
    };
    for case in keep_alives {
        let url = case["url"].as_str().ok_or("url")?;
        let body = case["body"].to_string();
        let Contact::Write(target) = verdict("POST", url, &body) else {
            return Err(format!("{}: not a write", case["name"]).into());
        };
        assert_eq!(target.kind(), WriteKind::CallFollowUp);
        assert!(target.added().is_empty());
        let Scope::CallOperation {
            operation,
            url: at,
            from,
            callbacks,
            joins,
        } = target.scope()
        else {
            return Err("not a call operation".into());
        };
        assert_eq!(*operation, CallOperation::KeepAlive);
        assert_eq!(at.as_str(), Url::parse(url)?.as_str());
        assert!(from.is_none() && callbacks.is_empty() && joins.is_none());
        // only that body, that method, that link
        for other in [
            r#"{"callParticipantUpdate":{"sharedLineCallContent":{}}}"#,
            r#"{"callParticipantUpdate":{},"x":1}"#,
            r#"{"callParticipantUpdate":null}"#,
            "{}",
            "",
        ] {
            assert_eq!(verdict("POST", url, other), Contact::Unreadable, "{other}");
        }
        assert_eq!(verdict("PUT", url, &body), Contact::Unreadable);
        assert_eq!(verdict("DELETE", url, &body), Contact::Unreadable);
        let deeper = url.replacen('?', "/more?", 1);
        assert_eq!(verdict("POST", &deeper, &body), Contact::Unreadable);
        let mut shorter = Url::parse(url)?;
        let segments: Vec<String> = shorter.path_segments().ok_or("segments")?.map(str::to_owned).collect();
        shorter.set_path(&segments[..segments.len() - 1].join("/"));
        assert_eq!(verdict("POST", shorter.as_str(), &body), Contact::Unreadable);
        let elsewhere = url.replace("/active/", "/incoming/");
        assert_eq!(verdict("POST", &elsewhere, &body), Contact::Unreadable);
    }
    Ok(())
}

/// A team's and a channel's made-up ids.
const TEAM: &str = "19:team0001@thread.tacv2";
const GROUP_ID: &str = "00000000-0000-4000-8000-0000000000aa";
const CHANNEL_GROUP: &str = "OID:00000000-0000-4000-8000-0000000000bb@00000000-0000-4000-8000-0000000000cc";

fn on_team(kind: WriteKind, id: &str) -> Contact {
    Contact::Write(Target::new(kind, Scope::Team { id: id.to_owned() }))
}

#[test]
fn a_standard_channel_as_the_dialog_sent_it_is_a_write_on_its_team() {
    let url = format!("https://teams.cloud.microsoft/api/mt/part/amer-03/beta/teams/{TEAM}/channels");
    for layout in ["Conversational", "PostReply"] {
        let body = format!(
            r#"{{"displayName":"a","description":"b","groupId":"{GROUP_ID}","channelType":"Standard","chatModalityType":"{layout}"}}"#
        );
        assert_eq!(
            verdict("POST", &url, Some(body.as_bytes())),
            on_team(WriteKind::ChannelChange, TEAM),
            "{layout}"
        );
    }
}

#[test]
fn a_channel_in_any_other_form_stays_unreadable() {
    let url = format!("https://teams.cloud.microsoft/api/mt/part/amer-03/beta/teams/{TEAM}/channels");
    for body in [
        // a shared channel names people and teams beyond its own
        r#"{"displayName":"a","groupId":"g","channelType":"Shared"}"#.to_owned(),
        // a key no capture showed
        r#"{"displayName":"a","groupId":"g","channelType":"Standard","users":["8:orgid:x"]}"#.to_owned(),
        // a key the fence would miss in another case
        r#"{"displayName":"a","groupId":"g","ChannelType":"Standard"}"#.to_owned(),
        "not json".to_owned(),
    ] {
        assert_eq!(
            verdict("POST", &url, Some(body.as_bytes())),
            Contact::Unreadable,
            "{body}"
        );
    }
    assert_eq!(verdict("POST", &url, None), Contact::Unreadable);
    assert_eq!(verdict("DELETE", &url, None), Contact::Unreadable);
    let not_a_team = "https://teams.cloud.microsoft/api/mt/part/amer-03/beta/teams/19:x@thread.v2/channels";
    let body = br#"{"displayName":"a","channelType":"Standard"}"#;
    assert_eq!(verdict("POST", not_a_team, Some(body)), Contact::Unreadable);
}

#[test]
fn a_channel_shown_to_everyone_is_a_write_on_that_channel() {
    let url = format!("https://teams.cloud.microsoft/api/mt/part/amer-03/beta/teams/{TEAM}/setDefaultChannel");
    assert_eq!(verdict("PUT", &url, None), on_team(WriteKind::ChannelChange, TEAM));
    assert_eq!(verdict("PUT", &url, Some(b"{}")), Contact::Unreadable);
}

#[test]
fn a_private_channel_as_the_dialog_sent_it_is_a_write_on_the_team_it_names() {
    let url =
        format!("https://teams.cloud.microsoft/fabric/amer/templates/api/teams/{GROUP_ID}/channels?forceSync=false");
    let body = format!(
        r#"{{"spaceThreadId":"{TEAM}","displayName":"a","description":"b","channelType":"Private","sharedWithParentTeam":true,"sessionKey":"k","chatModalityType":"Conversational"}}"#
    );
    assert_eq!(
        verdict("POST", &url, Some(body.as_bytes())),
        on_team(WriteKind::ChannelChange, TEAM)
    );
    let no_team = br#"{"displayName":"a","channelType":"Private"}"#;
    assert_eq!(verdict("POST", &url, Some(no_team)), Contact::Unreadable);
    let shared = format!(r#"{{"spaceThreadId":"{TEAM}","displayName":"a","channelType":"Shared"}}"#);
    assert_eq!(verdict("POST", &url, Some(shared.as_bytes())), Contact::Unreadable);
    let no_query = format!("https://teams.cloud.microsoft/fabric/amer/templates/api/teams/{GROUP_ID}/channels");
    assert_eq!(verdict("POST", &no_query, Some(body.as_bytes())), Contact::Unreadable);
}

#[test]
fn a_private_channel_s_people_are_added_to_it_by_their_accounts() {
    let url = format!(
        "https://teams.cloud.microsoft/fabric/amer/templates/api/teams/{GROUP_ID}/channels/{CHANNEL_GROUP}/users?forceSync=false"
    );
    let body = format!(r#"{{"users":[{{"id":"OID:{GUID_B}@00000000-0000-4000-8000-0000000000cc","role":1}}]}}"#);
    assert_eq!(
        verdict("POST", &url, Some(body.as_bytes())),
        Contact::Write(
            Target::new(
                WriteKind::MemberChange,
                Scope::Channel {
                    team: GROUP_ID.to_owned(),
                    channel: CHANNEL_GROUP.to_owned(),
                }
            )
            .with_added([lab_mri(GUID_B)])
        )
    );
    for body in [
        r#"{"users":[]}"#.to_owned(),
        r#"{"users":[{"id":"8:orgid:x","role":1}]}"#.to_owned(),
        r#"{"users":[{"id":"OID:x@y"}]}"#.to_owned(),
    ] {
        assert_eq!(
            verdict("POST", &url, Some(body.as_bytes())),
            Contact::Unreadable,
            "{body}"
        );
    }
}

#[test]
fn the_provisioning_service_is_a_contacting_service() {
    let base = "https://teams.cloud.microsoft/fabric/amer/templates/api";
    // anything it doesn't know
    assert_eq!(
        verdict("POST", &format!("{base}/teams"), Some(b"{}")),
        Contact::Unreadable,
        "a team made"
    );
    assert_eq!(
        verdict("POST", "https://templates.teams.microsoft.com/api/teams", Some(b"{}")),
        Contact::Unreadable
    );
    // the group's warm-up names nobody, and a read reaches nobody
    let warm = format!("{base}/teams/{GROUP_ID}/channelsWarmup");
    assert_eq!(verdict("POST", &warm, None), Contact::None);
    assert_eq!(verdict("POST", &warm, Some(b"{}")), Contact::Unreadable);
    assert_eq!(
        verdict(
            "GET",
            &format!("{base}/teams/{GROUP_ID}/channels/{CHANNEL_GROUP}"),
            None
        ),
        Contact::None
    );
}
