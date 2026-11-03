//! `proto-lab probe chat`: our own `ic3` token from the stored refresh token, then only `GET`s to the chat base from
//! the account's region GTMs (the conversation list in every header form, and the fence's 1:1 thread read through
//! `LiveMembershipSource`), and a report of statuses, header names and token claims only. Against wiremock: Entra on
//! one mock server, the chat service on another, reached through the `test-support` mock origin. Every value here is
//! made up.

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use proto_lab::commands::export::{self, Checks, ExportError};
use proto_lab::commands::probe::{
    CONVERSATIONS_PATH, Challenge, ChatProbe, HIDDEN, HeaderForm, IC3_RESOURCE, Origin, ProbeError, ProbeRequest,
    probe_chat,
};
use proto_lab::commands::thread_probe::{ThreadProbe, ThreadReadKind, next_page, probe_thread};
use proto_lab::config::record_abc_chat;
use proto_lab::config::{Alias, LabConfig};
use proto_lab::sanitize::Identities;
use serde_json::{Value, json};
use teams_auth::refresh::refresh_token_key;
use teams_auth::secrets::{MemorySecretStore, Secret, SecretStore as _};
use teams_auth::token::{EntraConfig, TokenClient, TokenError};
use teams_fence::{LiveMembershipSource, Membership};
use wiremock::matchers::{body_string_contains, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const GUID_A: &str = "0000000a-0000-4000-8000-00000000000a";
const GUID_B: &str = "0000000b-0000-4000-8000-00000000000b";
const TENANT: &str = "00000000-0000-4000-8000-0000000000ff";
const UPN_A: &str = "bot-a@lab.example.com";
const REFRESH_TOKEN: &str = "made-up-refresh-token-0123456789";
/// A word only the chat service's answers hold: it must never be printed.
const BODY_MARKER: &str = "PlantedBodyMarker";
const AB_CHAT: &str = "19:0000000a-0000-4000-8000-00000000000a_0000000b-0000-4000-8000-00000000000b@unq.gbl.spaces";
const CHAT_PREFIX: &str = "/api/chatsvc/amer";

fn lab_toml(data_dir: &Path) -> String {
    format!(
        "data_dir = \"{}\"\n\n\
         [[accounts]]\nalias = \"a\"\nupn = \"{UPN_A}\"\nmri = \"8:orgid:{GUID_A}\"\n\n\
         [[accounts]]\nalias = \"b\"\nupn = \"bot-b@lab.example.com\"\nmri = \"8:orgid:{GUID_B}\"\n\n\
         [[accounts]]\nalias = \"c\"\nupn = \"bot-c@lab.example.com\"\n",
        data_dir.display()
    )
}

/// A lab in a temp dir, with a's region record holding `chat_base` as its `chatServiceAfd` GTM.
fn lab(dir: &Path, chat_base: Option<&str>) -> TestResult<LabConfig> {
    let config = LabConfig::parse(&lab_toml(dir), dir)?;
    let mut gtms = json!({"middleTier": "https://teams.microsoft.com/api/mt/part/amer-03"});
    if let Some(base) = chat_base {
        gtms["chatServiceAfd"] = json!(base);
    }
    let record = json!({"region": "amer", "partition": "amer03", "regionGtms": gtms, "account": "a"});
    let file = proto_lab::commands::auth::region_file(config.paths(), Alias::A);
    std::fs::create_dir_all(file.parent().ok_or("no parent")?)?;
    std::fs::write(&file, record.to_string())?;
    Ok(config)
}

fn unix(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs())
}

/// An unsigned access token for `ic3`, built at run time.
fn ic3_token() -> String {
    let part = |value: &Value| URL_SAFE_NO_PAD.encode(value.to_string());
    let claims = json!({
        "aud": IC3_RESOURCE,
        "exp": unix(SystemTime::now() + Duration::from_secs(7_200)),
        "scp": "Teams.AccessAsUser.All",
        "tid": TENANT,
    });
    format!(
        "{}.{}.c2ln",
        part(&json!({"alg": "RS256", "typ": "JWT"})),
        part(&claims)
    )
}

/// Entra's answer to a refresh grant for `ic3`, and nothing else.
async fn entra(token: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth2/v2.0/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("ic3.teams.office.com%2F.default"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "token_type": "Bearer",
            "scope": "https://ic3.teams.office.com/Teams.AccessAsUser.All https://ic3.teams.office.com/.default",
            "expires_in": 7_000,
            "ext_expires_in": 7_000,
            "access_token": token,
        })))
        .mount(&server)
        .await;
    server
}

async fn store() -> TestResult<MemorySecretStore> {
    let store = MemorySecretStore::new();
    store
        .put(&refresh_token_key(UPN_A), &Secret::new(REFRESH_TOKEN))
        .await?;
    Ok(store)
}

fn tokens(entra: &MockServer) -> TestResult<TokenClient> {
    Ok(TokenClient::new(
        EntraConfig::teams_native().with_authority(entra.uri()),
    )?)
}

fn thread() -> Value {
    json!({
        "id": AB_CHAT,
        "type": "Thread",
        "properties": {"threadType": "chat", "uniquerosterthread": "true"},
        "members": [
            {"id": format!("8:orgid:{GUID_A}"), "role": "Admin"},
            {"id": format!("8:orgid:{GUID_B}"), "role": "Admin"},
        ],
        "version": 1_760_000_000_000_u64,
    })
}

fn conversations() -> Value {
    json!({
        "conversations": [{
            "id": AB_CHAT,
            "lastMessage": {"content": BODY_MARKER, "from": format!("8:orgid:{GUID_B}")},
        }],
        "_metadata": {"totalCount": 1},
    })
}

/// The chat service: the conversation list (one page, the web client's query) and the a–b chat's thread.
async fn chat_service(list: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    mount_chat_service(&server, list).await;
    server
}

/// [`chat_service`] on a server already started (a test that needs its address in an answer).
async fn mount_chat_service(server: &MockServer, list: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(format!("{CHAT_PREFIX}/{CONVERSATIONS_PATH}")))
        .and(query_param("view", "msnp24Equivalent"))
        .and(query_param("pageSize", "200"))
        .and(query_param("startTime", "1"))
        .and(query_param("targetType", "All"))
        .respond_with(list)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{CHAT_PREFIX}/v1/threads/{AB_CHAT}")))
        .and(query_param("view", "msnp24Equivalent"))
        .respond_with(ResponseTemplate::new(200).set_body_json(thread()))
        .mount(server)
        .await;
}

fn request<'a>(config: &'a LabConfig, chat: &MockServer) -> ProbeRequest<'a> {
    ProbeRequest {
        config,
        alias: Alias::A,
        partner: Alias::B,
        origin: Origin::Mock(chat.uri()),
    }
}

async fn run(config: &LabConfig, entra: &MockServer, chat: &MockServer) -> TestResult<ChatProbe> {
    let source = LiveMembershipSource::with_mock_origin(&chat.uri())?;
    Ok(probe_chat(&store().await?, &tokens(entra)?, request(config, chat), source).await?)
}

fn shown(probe: &ChatProbe) -> String {
    format!("{}\n{probe:?}", probe.lines(SystemTime::now()).join("\n"))
}

#[tokio::test]
async fn the_probe_reads_with_our_own_ic3_token_in_every_header_form_and_sends_only_gets() -> TestResult {
    let dir = tempfile::tempdir()?;
    let config = lab(dir.path(), Some("https://teams.microsoft.com/api/chatsvc/amer"))?;
    let token = ic3_token();
    let entra = entra(&token).await;
    let chat = chat_service(
        ResponseTemplate::new(200)
            .insert_header("ContextId", "tcid=0")
            .set_body_json(conversations()),
    )
    .await;
    let probe = run(&config, &entra, &chat).await?;

    // the conversation list once per header form, then the fence's thread read: every one a GET with our token
    let requests = chat.received_requests().await.ok_or("no recording")?;
    assert_eq!(requests.len(), HeaderForm::ALL.len() + 1);
    let bearer = format!("Bearer {token}");
    for request in &requests {
        assert_eq!(request.method.as_str(), "GET");
        assert_eq!(
            request
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some(bearer.as_str())
        );
    }
    let has = |index: usize, name: &str| requests[index].headers.contains_key(name);
    for (index, form) in HeaderForm::ALL.iter().enumerate() {
        assert_eq!(has(index, "x-ms-migration"), form.migration, "{form:?}");
        assert_eq!(has(index, "behavioroverride"), form.behavior_override, "{form:?}");
    }
    let thread_read = &requests[HeaderForm::ALL.len()];
    assert_eq!(thread_read.url.path(), format!("{CHAT_PREFIX}/v1/threads/{AB_CHAT}"));
    assert!(!thread_read.headers.contains_key("x-ms-migration"));

    assert!(matches!(probe.thread, Membership::Complete(_)), "{:?}", probe.thread);
    assert_eq!(probe.conversations.len(), HeaderForm::ALL.len());
    let text = shown(&probe);
    assert!(!text.contains(&token), "{text}");
    assert!(!text.contains(BODY_MARKER), "{text}");
    assert!(!text.contains(REFRESH_TOKEN), "{text}");
    assert!(!text.contains(GUID_A) && !text.contains(GUID_B), "{text}");
    assert!(text.contains(&format!("aud {IC3_RESOURCE}")), "{text}");
    assert!(text.contains("Teams.AccessAsUser.All"), "{text}");
    assert!(text.contains(TENANT), "{text}");
    assert!(text.contains("200"), "{text}");
    assert!(text.contains("contextid"), "{text}");
    assert!(!text.contains("tcid=0"), "header values aren't shown: {text}");
    assert!(text.contains("complete"), "{text}");
    Ok(())
}

#[tokio::test]
async fn a_refusal_shows_its_status_and_the_challenge_s_parameter_names_but_no_values() -> TestResult {
    let dir = tempfile::tempdir()?;
    let config = lab(dir.path(), Some("https://teams.microsoft.com/api/chatsvc/amer"))?;
    let entra = entra(&ic3_token()).await;
    let claims = "eyJhY2Nlc3NfdG9rZW4iOnsibmJmIjp7ImVzc2VudGlhbCI6dHJ1ZX19fQ";
    let challenge = format!(
        "Bearer authorization_uri=\"https://login.microsoftonline.com/common/oauth2/authorize\", \
         error=\"insufficient_claims\", claims=\"{claims}\""
    );
    let chat = chat_service(
        ResponseTemplate::new(401)
            .insert_header("WWW-Authenticate", challenge.as_str())
            .set_body_string(BODY_MARKER),
    )
    .await;
    let probe = run(&config, &entra, &chat).await?;
    let text = shown(&probe);
    assert!(text.contains("401"), "{text}");
    assert!(text.contains("insufficient_claims"), "{text}");
    for name in ["authorization_uri", "error", "claims"] {
        assert!(text.contains(name), "{name}: {text}");
    }
    assert!(!text.contains(claims), "{text}");
    assert!(!text.contains("oauth2/authorize"), "{text}");
    assert!(!text.contains(BODY_MARKER), "{text}");
    Ok(())
}

#[tokio::test]
async fn a_region_hop_is_reported_by_its_host_and_never_followed() -> TestResult {
    let dir = tempfile::tempdir()?;
    let config = lab(dir.path(), Some("https://teams.microsoft.com/api/chatsvc/amer"))?;
    let entra = entra(&ic3_token()).await;
    let chat = chat_service(ResponseTemplate::new(404).insert_header(
        "Location",
        "https://emea.ng.msg.teams.microsoft.com/v1/users/ME/conversations?secretish=1",
    ))
    .await;
    let probe = run(&config, &entra, &chat).await?;
    let text = shown(&probe);
    assert!(text.contains("404"), "{text}");
    assert!(text.contains("emea.ng.msg.teams.microsoft.com"), "{text}");
    assert!(!text.contains("secretish"), "{text}");
    // the probe's own five reads, no more (a 404 isn't followed by any client; the 3xx case is below)
    let requests = chat.received_requests().await.ok_or("no recording")?;
    assert_eq!(requests.len(), HeaderForm::ALL.len() + 1);
    Ok(())
}

#[tokio::test]
async fn a_redirect_is_reported_by_its_status_and_host_and_never_followed() -> TestResult {
    let dir = tempfile::tempdir()?;
    let config = lab(dir.path(), Some("https://teams.microsoft.com/api/chatsvc/amer"))?;
    let entra = entra(&ic3_token()).await;
    // the redirect points at the mock itself, at a path that answers: a client that follows it would be recorded there
    let chat = MockServer::start().await;
    let followed = "/followed";
    Mock::given(method("GET"))
        .and(path(followed))
        .respond_with(ResponseTemplate::new(200).set_body_json(conversations()))
        .mount(&chat)
        .await;
    let location = format!("{}{followed}?secretish=1", chat.uri());
    mount_chat_service(
        &chat,
        ResponseTemplate::new(307).insert_header("Location", location.as_str()),
    )
    .await;
    let probe = run(&config, &entra, &chat).await?;
    let text = shown(&probe);
    assert!(text.contains("307"), "{text}");
    assert!(text.contains("Location on 127.0.0.1 (not followed)"), "{text}");
    assert!(!text.contains("secretish"), "{text}");
    assert!(!text.contains(BODY_MARKER), "{text}");
    let requests = chat.received_requests().await.ok_or("no recording")?;
    assert!(
        requests.iter().all(|request| request.url.path() != followed),
        "the redirect was followed"
    );
    assert_eq!(requests.len(), HeaderForm::ALL.len() + 1);
    Ok(())
}

#[test]
fn a_challenge_shows_only_name_shaped_names_and_never_a_value_fragment() {
    // an escaped quote inside a quoted value doesn't end it, and the real `error` that follows is read
    let challenge = Challenge::read(r#"Bearer realm="lab \"private\" tenant", error="invalid_token""#);
    assert_eq!(challenge.scheme, "Bearer");
    assert_eq!(challenge.parameters, ["realm", "error"]);
    assert_eq!(challenge.error.as_deref(), Some("invalid_token"));

    // a token68 is no list of names
    let challenge = Challenge::read("Negotiate YIIabc+/==");
    assert_eq!(challenge.scheme, "Negotiate");
    assert_eq!(challenge.parameters, [HIDDEN]);

    // a name with anything but letters, digits, '-', '_' and '.' before its '=' may be part of a value
    let challenge = Challenge::read(r#"Bearer x"y=1, no value here, error=insufficient_claims, a b=2"#);
    assert_eq!(challenge.parameters, [HIDDEN, HIDDEN, "error", HIDDEN]);
    assert_eq!(challenge.error.as_deref(), Some("insufficient_claims"));

    // text after a quoted value up to the next comma is no parameter of its own; nor is a scheme that isn't name-shaped
    let challenge = Challenge::read(r#"realm="a"leak=1, error="x y""#);
    assert_eq!(challenge.scheme, HIDDEN);
    let challenge = Challenge::read(r#"Bearer realm="a"leak=1, error="x y""#);
    assert_eq!(challenge.parameters, ["realm", "error"]);
    assert_eq!(
        challenge.error, None,
        "an error value that isn't a plain code isn't shown"
    );
    let shown = format!("{challenge:?}");
    assert!(!shown.contains("leak") && !shown.contains("x y"), "{shown}");
}

#[test]
fn a_bad_save_name_or_destination_is_refused_before_anything_is_read() -> TestResult {
    let dir = tempfile::tempdir()?;
    let reference_dir = dir.path().join("reference");
    for name in ["../x", "a.txt", "/abs", "chat/.hidden"] {
        let refused = export::check_destination(&reference_dir, name);
        assert!(matches!(refused, Err(ExportError::BadName(_))), "{name}: {refused:?}");
    }
    let repository = tempfile::tempdir()?;
    std::fs::create_dir(repository.path().join(".git"))?;
    let refused = export::check_destination(&repository.path().join("reference"), "chat/probe-a");
    assert!(matches!(refused, Err(ExportError::InsideRepository(_))), "{refused:?}");
    let path = export::check_destination(&reference_dir, "chat/probe-a")?;
    assert!(path.ends_with("reference/chat/probe-a.jsonl"), "{}", path.display());
    assert!(!reference_dir.exists(), "a check writes nothing");
    Ok(())
}

#[tokio::test]
async fn a_gtm_value_with_stray_whitespace_is_trimmed() -> TestResult {
    let dir = tempfile::tempdir()?;
    let config = lab(dir.path(), Some(" https://teams.microsoft.com/api/chatsvc/amer/ "))?;
    let entra = entra(&ic3_token()).await;
    let chat = chat_service(ResponseTemplate::new(200).set_body_json(conversations())).await;
    let probe = run(&config, &entra, &chat).await?;
    assert!(matches!(probe.thread, Membership::Complete(_)), "{:?}", probe.thread);
    assert_eq!(probe.base, "https://teams.microsoft.com/api/chatsvc/amer");
    Ok(())
}

#[tokio::test]
async fn a_base_that_isnt_the_chat_service_is_refused_before_any_request() -> TestResult {
    for base in [
        None,
        Some("https://evil.example.com/api/chatsvc/amer"),
        Some("https://teams.microsoft.com/api/mt/part/amer-03"),
        Some("https://teams.microsoft.com/api/chatsvc/amer?x=1"),
        Some("http://teams.microsoft.com/api/chatsvc/amer"),
    ] {
        let dir = tempfile::tempdir()?;
        let config = lab(dir.path(), base)?;
        let entra = entra(&ic3_token()).await;
        let chat = chat_service(ResponseTemplate::new(200).set_body_json(conversations())).await;
        let error = run(&config, &entra, &chat).await.err().ok_or("probed")?;
        let error = error.downcast::<ProbeError>()?;
        assert!(
            matches!(*error, ProbeError::NoChatBase | ProbeError::NotAChatBase(_)),
            "{base:?}: {error}"
        );
        for server in [&entra, &chat] {
            let requests = server.received_requests().await.ok_or("no recording")?;
            assert!(requests.is_empty(), "{base:?}");
        }
    }
    Ok(())
}

#[tokio::test]
async fn without_a_stored_refresh_token_nothing_reaches_the_chat_service() -> TestResult {
    let dir = tempfile::tempdir()?;
    let config = lab(dir.path(), Some("https://teams.microsoft.com/api/chatsvc/amer"))?;
    let entra = entra(&ic3_token()).await;
    let chat = chat_service(ResponseTemplate::new(200).set_body_json(conversations())).await;
    let source = LiveMembershipSource::with_mock_origin(&chat.uri())?;
    let error = probe_chat(
        &MemorySecretStore::new(),
        &tokens(&entra)?,
        request(&config, &chat),
        source,
    )
    .await
    .err()
    .ok_or("probed")?;
    assert!(
        matches!(error, ProbeError::Token(TokenError::NotSignedIn { .. })),
        "{error}"
    );
    assert!(chat.received_requests().await.ok_or("no recording")?.is_empty());
    Ok(())
}

#[tokio::test]
async fn an_account_without_an_mri_or_the_same_account_twice_is_refused() -> TestResult {
    let dir = tempfile::tempdir()?;
    let config = lab(dir.path(), Some("https://teams.microsoft.com/api/chatsvc/amer"))?;
    let entra = entra(&ic3_token()).await;
    let chat = chat_service(ResponseTemplate::new(200).set_body_json(conversations())).await;
    for (alias, partner) in [(Alias::A, Alias::C), (Alias::A, Alias::A)] {
        let source = LiveMembershipSource::with_mock_origin(&chat.uri())?;
        let probe = ProbeRequest {
            alias,
            partner,
            ..request(&config, &chat)
        };
        let error = probe_chat(&store().await?, &tokens(&entra)?, probe, source)
            .await
            .err()
            .ok_or("probed")?;
        assert!(
            matches!(error, ProbeError::NoMri(_) | ProbeError::NoChat(..)),
            "{alias} {partner}: {error}"
        );
    }
    assert!(entra.received_requests().await.ok_or("no recording")?.is_empty());
    assert!(chat.received_requests().await.ok_or("no recording")?.is_empty());
    Ok(())
}

#[tokio::test]
async fn saving_writes_a_sanitized_reference_copy_and_never_inside_a_repository() -> TestResult {
    let dir = tempfile::tempdir()?;
    let config = lab(dir.path(), Some("https://teams.microsoft.com/api/chatsvc/amer"))?;
    let token = ic3_token();
    let entra = entra(&token).await;
    let chat = chat_service(ResponseTemplate::new(200).set_body_json(conversations())).await;
    let probe = run(&config, &entra, &chat).await?;
    let events = probe.reference_events();
    // one event per conversation-list read, each with its form, status and answer
    assert_eq!(events.len(), HeaderForm::ALL.len());
    let identities = Identities::from_lab(&config);

    let reference_dir = export::reference_dir(config.paths().data_dir());
    let exported = export::export_events(&events, "chat/probe-a", &reference_dir, &identities, Checks::Warn)?;
    let text = std::fs::read_to_string(&exported.path)?;
    assert!(exported.path.starts_with(&reference_dir), "{}", exported.path.display());
    assert_eq!(std::fs::metadata(&exported.path)?.permissions().mode() & 0o777, 0o600);
    assert!(text.contains(BODY_MARKER), "the answer is kept: {text}");
    assert!(
        text.contains("<mri:b>"),
        "the lab's MRIs become their placeholders: {text}"
    );
    assert!(!text.contains(GUID_A) && !text.contains(GUID_B), "{text}");
    assert!(!text.contains(&token), "{text}");
    assert!(
        !text.contains("127.0.0.1"),
        "the mock origin isn't what was recorded: {text}"
    );

    // a reference dir inside a git work tree is refused, and nothing is written there
    let repository = tempfile::tempdir()?;
    std::fs::create_dir(repository.path().join(".git"))?;
    let inside = repository.path().join("reference");
    let refused = export::export_events(&events, "chat/probe-a", &inside, &identities, Checks::Warn);
    assert!(matches!(refused, Err(ExportError::InsideRepository(_))), "{refused:?}");
    assert!(!inside.exists());
    Ok(())
}

const GROUP: &str = "19:0123456789abcdef0123456789abcdef@thread.v2";
const GROUP_ENCODED: &str = "19%3A0123456789abcdef0123456789abcdef%40thread.v2";
const GUID_C: &str = "0000000c-0000-4000-8000-00000000000c";

fn group_member(guid: &str) -> Value {
    json!({"id": format!("8:orgid:{guid}"), "role": "Admin"})
}

fn group_thread() -> Value {
    json!({
        "id": GROUP,
        "type": "Thread",
        "properties": {"threadType": "chat", "topic": BODY_MARKER},
        "members": [group_member(GUID_A), group_member(GUID_B), group_member(GUID_C)],
        "rosterVersion": 7,
        "version": 1_760_000_000_000_u64,
    })
}

/// The group chat's reads (the id as the client puts it in a path segment: `:` and `@` aren't encoded there): the
/// thread, the batch read, the member list in one page and in pages of one (the second
/// page's next link names another chat's list, which mustn't be followed), the filtered view, the own view.
async fn group_service(server: &MockServer) {
    let members = format!("{CHAT_PREFIX}/v1/threads/{GROUP}/members");
    let mounts = [
        (format!("{CHAT_PREFIX}/v1/threads/{GROUP}"), None, group_thread()),
        (
            format!("{CHAT_PREFIX}/v1/threads"),
            Some(("threadIds", GROUP)),
            json!([group_thread()]),
        ),
        (
            members.clone(),
            Some(("pageSize", "100")),
            json!({"members": [group_member(GUID_A), group_member(GUID_B), group_member(GUID_C)]}),
        ),
        (
            format!("{CHAT_PREFIX}/v1/threads/{GROUP}/filtered"),
            None,
            group_thread(),
        ),
        (
            format!("{CHAT_PREFIX}/v1/users/ME/conversations/{GROUP}"),
            None,
            json!({"id": GROUP, "threadProperties": {"membercount": "3"}}),
        ),
    ];
    for (at, param, body) in mounts {
        let mut mock = Mock::given(method("GET"))
            .and(path(at))
            .and(query_param("view", "msnp24Equivalent"));
        if let Some((name, value)) = param {
            mock = mock.and(query_param(name, value));
        }
        mock.respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }
    // pages of one: the first names the second by a full URL on the regional host, the second another chat's list
    let second = format!(
        "https://amer.ng.msg.teams.microsoft.com/v1/threads/{GROUP_ENCODED}/members?view=msnp24Equivalent&pageSize=1&\
         syncState=page2"
    );
    let elsewhere = "https://amer.ng.msg.teams.microsoft.com/v1/threads/19%3Aother%40thread.v2/members?view=\
                     msnp24Equivalent&syncState=page3";
    Mock::given(method("GET"))
        .and(path(members.clone()))
        .and(query_param("syncState", "page2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "members": [group_member(GUID_B)],
            "_metadata": {"nextLink": elsewhere},
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(members))
        .and(query_param("pageSize", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "members": [group_member(GUID_A)],
            "_metadata": {"nextLink": second},
        })))
        .with_priority(10)
        .mount(server)
        .await;
}

async fn run_thread(config: &LabConfig, entra: &MockServer, chat: &MockServer) -> TestResult<ThreadProbe> {
    let source = LiveMembershipSource::with_mock_origin(&chat.uri())?;
    let origin = Origin::Mock(chat.uri());
    Ok(probe_thread(&store().await?, &tokens(entra)?, config, Alias::A, &origin, source).await?)
}

/// The thread probe of a lab whose `chats.toml` records the group chat, against [`group_service`]: the lab, the
/// token Entra handed out, the chat service and the probe.
async fn probed_group(dir: &Path) -> TestResult<(LabConfig, String, MockServer, ThreadProbe)> {
    let config = lab(dir, Some("https://teams.microsoft.com/api/chatsvc/amer"))?;
    record_abc_chat(config.paths(), GROUP)?;
    let token = ic3_token();
    let entra = entra(&token).await;
    let chat = MockServer::start().await;
    group_service(&chat).await;
    let probe = run_thread(&config, &entra, &chat).await?;
    Ok((config, token, chat, probe))
}

#[tokio::test]
async fn the_thread_probe_reads_the_group_chat_s_member_reads_with_gets_only() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (_config, token, chat, probe) = probed_group(dir.path()).await?;
    let requests = chat.received_requests().await.ok_or("no recording")?;
    let bearer = format!("Bearer {token}");
    for request in &requests {
        assert_eq!(request.method.as_str(), "GET");
        let sent = request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok());
        assert_eq!(sent, Some(bearer.as_str()));
    }
    // the second page was followed on the chat base, the other chat's list never
    let queries: Vec<String> = requests
        .iter()
        .map(|request| request.url.query().unwrap_or_default().to_owned())
        .collect();
    assert!(
        queries.iter().any(|query| query.contains("syncState=page2")),
        "{queries:?}"
    );
    assert!(!queries.iter().any(|query| query.contains("page3")), "{queries:?}");
    assert!(!requests.iter().any(|request| request.url.path().contains("other")));
    let kinds: Vec<(ThreadReadKind, usize)> = probe.reads.iter().map(|read| (read.kind, read.page)).collect();
    assert_eq!(
        kinds,
        [
            (ThreadReadKind::Thread, 1),
            (ThreadReadKind::Threads, 1),
            (ThreadReadKind::Members, 1),
            (ThreadReadKind::MembersPaged, 1),
            (ThreadReadKind::MembersPaged, 2),
            (ThreadReadKind::Filtered, 1),
            (ThreadReadKind::MyConversation, 1),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn the_thread_probe_shows_keys_and_counts_and_no_value() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (_config, token, _chat, probe) = probed_group(dir.path()).await?;
    let text = format!("{}\n{probe:?}", probe.lines(SystemTime::now()).join("\n"));
    for hidden in [
        token.as_str(),
        BODY_MARKER,
        REFRESH_TOKEN,
        GUID_A,
        GUID_B,
        GUID_C,
        "0123456789abcdef",
    ] {
        assert!(!text.contains(hidden), "{hidden}: {text}");
    }
    assert!(text.contains("3 member(s)"), "{text}");
    assert!(
        text.contains("members same as the thread read's; rosterVersion same"),
        "{text}"
    );
    assert!(text.contains("paging keys [nextLink]"), "{text}");
    assert!(
        text.contains("members different as the thread read's"),
        "a page of one: {text}"
    );
    Ok(())
}

#[tokio::test]
async fn the_thread_probe_s_reference_copy_keeps_the_answers_sanitized() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (config, token, _chat, probe) = probed_group(dir.path()).await?;
    let events = probe.reference_events();
    assert_eq!(events.len(), 7);
    let reference_dir = export::reference_dir(config.paths().data_dir());
    let exported = export::export_events(
        &events,
        "chat/thread-a",
        &reference_dir,
        &Identities::from_lab(&config),
        Checks::Warn,
    )?;
    let copy = std::fs::read_to_string(&exported.path)?;
    assert!(copy.contains("\"read\":\"thread_members (paged)\""), "{copy}");
    assert!(
        copy.contains("<mri:a>") && copy.contains("19:<thread:1>@thread.v2"),
        "{copy}"
    );
    // never the mock origin or the token; a chat's topic is a name someone gave it, so the sanitizer takes it out too
    for hidden in [
        token.as_str(),
        "127.0.0.1",
        BODY_MARKER,
        GUID_A,
        GUID_C,
        "0123456789abcdef",
    ] {
        assert!(!copy.contains(hidden), "{hidden}: {copy}");
    }
    Ok(())
}

#[tokio::test]
async fn the_thread_probe_needs_a_recorded_group_chat_before_anything_is_read() -> TestResult {
    let dir = tempfile::tempdir()?;
    let config = lab(dir.path(), Some("https://teams.microsoft.com/api/chatsvc/amer"))?;
    let entra = entra(&ic3_token()).await;
    let chat = MockServer::start().await;
    let refused = run_thread(&config, &entra, &chat).await;
    let error = refused.err().ok_or("the probe ran without a group chat")?;
    assert!(error.to_string().contains("no a-b-c group chat"), "{error}");
    assert!(chat.received_requests().await.unwrap_or_default().is_empty());
    assert!(entra.received_requests().await.unwrap_or_default().is_empty());
    Ok(())
}

#[test]
fn a_next_page_is_followed_only_on_the_same_member_list_and_only_on_the_current_origin() -> TestResult {
    let current = url::Url::parse(&format!(
        "https://teams.microsoft.com/api/chatsvc/amer/v1/threads/{GROUP_ENCODED}/members?view=msnp24Equivalent&\
         pageSize=1"
    ))?;
    let regional = format!(
        "https://amer.ng.msg.teams.microsoft.com/v1/threads/{GROUP_ENCODED}/members?view=msnp24Equivalent&syncState=x"
    );
    let next = next_page(&json!({"_metadata": {"nextLink": regional}}), &current).ok_or("not followed")?;
    assert_eq!(
        next.host_str(),
        Some("teams.microsoft.com"),
        "re-based on the current origin"
    );
    assert_eq!(next.path(), current.path());
    assert_eq!(next.query(), Some("view=msnp24Equivalent&syncState=x"));
    let other_chat =
        "https://amer.ng.msg.teams.microsoft.com/v1/threads/19%3Aother%40thread.v2/members?view=msnp24Equivalent";
    for refused in [
        json!({"_metadata": {"nextLink": other_chat}}),
        json!({"_metadata": {"nextLink": "https://example.com/v1/threads/x/members"}}),
        json!({"_metadata": {"nextLink": "https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations"}}),
        json!({"_metadata": {"nextLink": 3}}),
        json!({"_metadata": {"backwardLink": regional}}),
        json!([{"nextLink": regional}]),
    ] {
        assert_eq!(next_page(&refused, &current), None, "{refused}");
    }
    let relative = next_page(
        &json!({"nextLink": "members?view=msnp24Equivalent&syncState=y"}),
        &current,
    )
    .ok_or("relative not followed")?;
    assert_eq!(relative.query(), Some("view=msnp24Equivalent&syncState=y"));
    Ok(())
}
