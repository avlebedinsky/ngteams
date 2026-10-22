//! `proto-lab probe teams`: our own `chatsvcagg` and `ic3` tokens from the stored refresh token, then only `GET`s to
//! CSA's teams read and the chat service's channel reads, and a report of statuses, key names, types, sizes and counts
//! only. Against wiremock: Entra on one mock server, CSA and the chat service (both on the AFD host) on another,
//! reached through the `test-support` mock origin. Every value here is made up.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use proto_lab::commands::probe::{IC3_RESOURCE, Origin, ProbeError};
use proto_lab::commands::teams_probe::{CSA_RESOURCE, TEAMS_QUERY, TeamsReadKind, probe_teams, shape_lines};
use proto_lab::config::{Alias, LabConfig};
use serde_json::{Value, json};
use teams_auth::refresh::refresh_token_key;
use teams_auth::secrets::{MemorySecretStore, Secret, SecretStore as _};
use teams_auth::token::{EntraConfig, TokenClient};
use wiremock::matchers::{body_string_contains, method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const GUID_A: &str = "0000000a-0000-4000-8000-00000000000a";
const TENANT: &str = "00000000-0000-4000-8000-0000000000ff";
const UPN_A: &str = "bot-a@lab.example.com";
const REFRESH_TOKEN: &str = "made-up-refresh-token-0123456789";
/// A word only the services' answers hold: it must never be printed.
const BODY_MARKER: &str = "PlantedBodyMarker";
const TEAM: &str = "19:team00000000000000000000000001@thread.tacv2";
const OTHER_CHANNEL: &str = "19:chan00000000000000000000000002@thread.tacv2";
const POST: &str = "1700000000001";
const CSA_PREFIX: &str = "/api/csa/amer";
const CHAT_PREFIX: &str = "/api/chatsvc/amer";

/// A lab in a temp dir, with a's region record holding the GTMs `gtms`.
fn lab(dir: &Path, gtms: &Value) -> TestResult<LabConfig> {
    let toml = format!(
        "data_dir = \"{}\"\n\n\
         [[accounts]]\nalias = \"a\"\nupn = \"{UPN_A}\"\nmri = \"8:orgid:{GUID_A}\"\n\n\
         [[accounts]]\nalias = \"b\"\nupn = \"bot-b@lab.example.com\"\n\n\
         [[accounts]]\nalias = \"c\"\nupn = \"bot-c@lab.example.com\"\n",
        dir.display()
    );
    let config = LabConfig::parse(&toml, dir)?;
    let record = json!({"region": "amer", "partition": "amer03", "regionGtms": gtms, "account": "a"});
    let file = proto_lab::commands::auth::region_file(config.paths(), Alias::A);
    std::fs::create_dir_all(file.parent().ok_or("no parent")?)?;
    std::fs::write(&file, record.to_string())?;
    Ok(config)
}

fn gtms() -> Value {
    json!({
        "chatServiceAfd": "https://teams.microsoft.com/api/chatsvc/amer",
        "chatSvcAggAfd": " https://teams.microsoft.com/api/csa/amer ",
    })
}

/// An unsigned access token for `audience`, built at run time.
fn token(audience: &str) -> String {
    let part = |value: &Value| URL_SAFE_NO_PAD.encode(value.to_string());
    let exp = (SystemTime::now() + Duration::from_secs(7_200))
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let claims = json!({"aud": audience, "exp": exp, "scp": "user_impersonation", "tid": TENANT});
    format!(
        "{}.{}.c2ln",
        part(&json!({"alg": "RS256", "typ": "JWT"})),
        part(&claims)
    )
}

/// Entra's answers to refresh grants for CSA and `ic3`, and nothing else.
async fn entra(csa: &str, ic3: &str) -> MockServer {
    let server = MockServer::start().await;
    for (resource, token) in [
        ("chatsvcagg.teams.microsoft.com%2F.default", csa),
        ("ic3.teams.office.com%2F.default", ic3),
    ] {
        Mock::given(method("POST"))
            .and(path("/oauth2/v2.0/token"))
            .and(body_string_contains(resource))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "token_type": "Bearer",
                "scope": "user_impersonation",
                "expires_in": 7_000,
                "ext_expires_in": 7_000,
                "access_token": token,
            })))
            .mount(&server)
            .await;
    }
    server
}

fn channel(id: &str) -> Value {
    json!({"id": id, "displayName": format!("{BODY_MARKER} channel"), "isGeneral": id == TEAM, "isFavorite": false})
}

fn message(id: &str, link_suffix: &str) -> Value {
    json!({
        "id": id,
        "type": "Message",
        "messagetype": "RichText/Html",
        "conversationid": TEAM,
        "conversationLink": format!("https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations/{TEAM}{link_suffix}"),
        "content": format!("<p>{BODY_MARKER} text</p>"),
        "imdisplayname": format!("{BODY_MARKER} Person"),
        "properties": {"0123456789abcdef0123": "id-like key"},
    })
}

/// CSA and the chat service on one AFD origin: one team with two channels, its general one listed and read.
async fn services() -> MockServer {
    let server = MockServer::start().await;
    let teams = json!({
        "teams": [{"id": TEAM, "displayName": format!("{BODY_MARKER} team"), "isDeleted": false,
                   "channels": [channel(OTHER_CHANNEL), channel(TEAM)]}],
        "chats": [],
    });
    let list = json!({"conversations": [{"id": TEAM}, {"id": "19:other@unq.gbl.spaces"}], "_metadata": {}});
    let thread = json!({"id": TEAM, "type": "Thread",
                        "properties": {"threadType": "space", "productThreadType": "TeamsTeam"},
                        "members": [{"id": format!("8:orgid:{GUID_A}")}]});
    let page = json!({"messages": [message("1700000000002", &format!(";messageid={POST}")), message(POST, "")],
                      "_metadata": {"syncState": "opaque"}});
    let chain = json!({"messages": [message(POST, "")], "_metadata": {}});
    let routes = [
        (format!("^{CSA_PREFIX}/api/v3/teams/users/me$"), teams),
        (format!("^{CHAT_PREFIX}/v1/users/ME/conversations$"), list),
        (format!("^{CHAT_PREFIX}/v1/threads/[^/]+$"), thread),
        (
            format!("^{CHAT_PREFIX}/v1/users/ME/conversations/[^/;]+/messages$"),
            page,
        ),
        (
            format!("^{CHAT_PREFIX}/v1/users/ME/conversations/[^/]+;messageid=[^/]+/messages$"),
            chain,
        ),
    ];
    for (route, body) in routes {
        Mock::given(method("GET"))
            .and(path_regex(route))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
    }
    server
}

async fn store() -> TestResult<MemorySecretStore> {
    let store = MemorySecretStore::new();
    store
        .put(&refresh_token_key(UPN_A), &Secret::new(REFRESH_TOKEN))
        .await?;
    Ok(store)
}

#[tokio::test]
async fn the_teams_probe_reads_csa_and_the_channels_by_get_only_and_shows_no_value() -> TestResult {
    use TeamsReadKind::{Conversations, Messages, ReplyChain, Teams, Thread};
    let dir = tempfile::tempdir()?;
    let config = lab(dir.path(), &gtms())?;
    let (csa, ic3) = (token(CSA_RESOURCE), token(IC3_RESOURCE));
    let entra = entra(&csa, &ic3).await;
    let services = services().await;
    let tokens = TokenClient::new(EntraConfig::teams_native().with_authority(entra.uri()))?;
    let probe = probe_teams(
        &store().await?,
        &tokens,
        &config,
        Alias::A,
        &Origin::Mock(services.uri()),
    )
    .await?;

    let kinds: Vec<TeamsReadKind> = probe.reads.iter().map(|read| read.kind).collect();
    // the general channel first; each page names a reply's post, so both chains are read
    assert_eq!(
        kinds,
        [
            Teams,
            Conversations,
            Thread,
            Messages,
            ReplyChain,
            Thread,
            Messages,
            ReplyChain
        ]
    );
    let requests = services.received_requests().await.ok_or("no recording")?;
    assert_eq!(requests.len(), kinds.len());
    for request in &requests {
        assert_eq!(request.method.as_str(), "GET");
        assert!(request.body.is_empty());
        let header = |name: &str| request.headers.get(name).and_then(|value| value.to_str().ok());
        assert_eq!(header("accept"), Some("application/json"));
        if request.url.path().starts_with(CSA_PREFIX) {
            assert_eq!(header("authorization"), Some(format!("Bearer {csa}").as_str()));
            assert_eq!(request.url.query(), Some(TEAMS_QUERY));
            assert!(!request.headers.contains_key("x-ms-migration"));
        } else {
            assert_eq!(header("authorization"), Some(format!("Bearer {ic3}").as_str()));
            assert_eq!(header("x-ms-migration"), Some("True"));
        }
    }
    let chain = requests
        .iter()
        .find(|request| request.url.path().contains(";messageid="))
        .ok_or("no reply chain read")?;
    assert!(chain.url.path().ends_with(&format!(";messageid={POST}/messages")));

    let text = format!("{}\n{probe:?}", probe.lines(SystemTime::now()).join("\n"));
    for hidden in [
        csa.as_str(),
        ic3.as_str(),
        BODY_MARKER,
        REFRESH_TOKEN,
        GUID_A,
        TEAM,
        OTHER_CHANNEL,
        POST,
    ] {
        assert!(!text.contains(hidden), "{hidden} shown: {text}");
    }
    for shown in [
        "aud https://chatsvcagg.teams.microsoft.com",
        "teams: 1; channels: 2, of them 1 with the team's id",
        "channel.isGeneral true: 1",
        "in the chat service's list: 1 of 1 teams, 1 of 2 channels",
        "threadType space",
        "productThreadType TeamsTeam",
        "members: 1",
        "messagetype: RichText/Html 2",
        "conversationid with ;messageid=: 0; conversationLink with ;messageid=: 1",
        "teams[].channels[].displayName: string",
        "messages[].properties.?: string",
    ] {
        assert!(text.contains(shown), "{shown} not shown: {text}");
    }
    Ok(())
}

#[tokio::test]
async fn a_csa_base_off_the_afd_hosts_is_refused_before_anything_is_sent() -> TestResult {
    for base in [
        "https://chatsvcagg.teams.microsoft.com/api/csa/amer",
        "https://teams.microsoft.com/api/csa/amer/extra",
        "https://teams.microsoft.com/api/chatsvc/amer",
        "http://teams.microsoft.com/api/csa/amer",
        "https://teams.microsoft.com/api/csa/AMER",
    ] {
        let dir = tempfile::tempdir()?;
        let mut gtms = gtms();
        gtms["chatSvcAggAfd"] = json!(base);
        let config = lab(dir.path(), &gtms)?;
        let entra = entra(&token(CSA_RESOURCE), &token(IC3_RESOURCE)).await;
        let services = services().await;
        let tokens = TokenClient::new(EntraConfig::teams_native().with_authority(entra.uri()))?;
        let outcome = probe_teams(
            &store().await?,
            &tokens,
            &config,
            Alias::A,
            &Origin::Mock(services.uri()),
        )
        .await;
        assert!(
            matches!(outcome, Err(ProbeError::NotACsaBase(_))),
            "{base}: {outcome:?}"
        );
        assert!(entra.received_requests().await.ok_or("no recording")?.is_empty());
        assert!(services.received_requests().await.ok_or("no recording")?.is_empty());
    }
    Ok(())
}

#[tokio::test]
async fn a_region_without_a_csa_base_is_reported_as_such_before_anything_is_sent() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut gtms = gtms();
    gtms.as_object_mut().ok_or("not an object")?.remove("chatSvcAggAfd");
    let config = lab(dir.path(), &gtms)?;
    let entra = entra(&token(CSA_RESOURCE), &token(IC3_RESOURCE)).await;
    let services = services().await;
    let tokens = TokenClient::new(EntraConfig::teams_native().with_authority(entra.uri()))?;
    let outcome = probe_teams(
        &store().await?,
        &tokens,
        &config,
        Alias::A,
        &Origin::Mock(services.uri()),
    )
    .await;
    let Err(error) = outcome else {
        return Err("the probe ran without a CSA base".into());
    };
    assert!(matches!(error, ProbeError::NoCsaBase), "{error:?}");
    // the chat base is fine: the report names CSA's GTM, not the chat service's
    let shown = error.to_string();
    assert!(
        shown.contains("chatSvcAggAfd") && !shown.contains("chatService"),
        "{shown}"
    );
    assert!(entra.received_requests().await.ok_or("no recording")?.is_empty());
    assert!(services.received_requests().await.ok_or("no recording")?.is_empty());
    Ok(())
}

#[test]
fn a_shape_shows_key_paths_and_types_and_hides_id_like_keys() {
    let value = json!({
        "a": [{"b": 1}, {"b": "x", "c": null}],
        "19:id@thread.tacv2": true,
        "0000000a-0000-4000-8000-00000000000a": 1,
        "key12345": 2,
    });
    let lines = shape_lines(&value, 4);
    assert_eq!(
        lines,
        [
            ".: object",
            "?: bool|number",
            "a: array",
            "a[]: object",
            "a[].b: number|string",
            "a[].c: null"
        ]
    );
}
