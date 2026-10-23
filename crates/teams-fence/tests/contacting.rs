//! `teams_fence::contacting`'s table (`docs/plans/20261023-lab-fence-contacting-writes-only.md`, Technical Details):
//! a row per mapped write, carve-out and unreadable form, from the synthetic fixtures under
//! `tests/fixtures/contacting/`, the capture-shaped chat service fixtures of the repo (`tests/fixtures/chatsvc/`), and
//! the web client's test call start. Made-up GUIDs fill in the placeholders; no real MRI or token appears here.

use serde_json::Value;
use teams_fence::{
    CALL_START_URL, Contact, Mri, OWN_VIEW_PROPERTIES, Policy, Request, Scope, Service, TEST_CALL_SCENARIO, Target,
    WriteKind, contacting, is_chat_id, service_of,
};
use url::Url;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

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
    let scope = match expect["conversation"].as_str() {
        Some(id) => Scope::Conversation { id: id.to_owned() },
        None if expect["new"] == true => Scope::New,
        None => return Err("no scope".into()),
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

#[test]
fn the_test_call_start_is_a_call_with_the_caller_and_the_echo_service() {
    let expected = Target::new(WriteKind::Call, Scope::New).with_added([Mri::new(CALLER), Mri::new(ECHO)]);
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
        // a call to a lab account: not mapped yet (the calling family's rows are M3's)
        call_start(lab_b, ""),
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
