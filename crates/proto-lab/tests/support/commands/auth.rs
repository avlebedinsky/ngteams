//! The native auth controller on loopback token/authz endpoints and in-memory secrets; its profile callback is synthetic.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use teams_auth::secrets::MemorySecretStore;
use teams_auth::token::{NATIVE_CLIENT_REDIRECT, TEAMS_NATIVE_CLIENT_ID};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::*;

const UPN: &str = "synthetic-a@example.test";
const OID: &str = "00000000-0000-4000-8000-00000000000a";

fn config(root: &std::path::Path) -> LabConfig {
    let mut text = format!("data_dir = {:?}\n", root.join("lab-data").to_string_lossy());
    for alias in ["a", "b", "c"] {
        use std::fmt::Write as _;
        writeln!(
            text,
            "[[accounts]]\nalias = {alias:?}\nupn = \"synthetic-{alias}@example.test\""
        )
        .unwrap();
        if alias == "a" {
            writeln!(text, "mri = \"8:orgid:{OID}\"").unwrap();
        }
    }
    LabConfig::parse(&text, root).unwrap()
}

fn id_token(upn: &str) -> String {
    let oid = if upn == UPN {
        OID
    } else {
        "00000000-0000-4000-8000-00000000000b"
    };
    let encode = |value: serde_json::Value| URL_SAFE_NO_PAD.encode(value.to_string());
    format!(
        "{}.{}.c2ln",
        encode(json!({"alg":"RS256","typ":"JWT","kid":"synthetic"})),
        encode(
            json!({"aud":TEAMS_NATIVE_CLIENT_ID,"oid":oid,"tid":"00000000-0000-4000-8000-000000000001","preferred_username":upn})
        )
    )
}

async fn tokens(upn: &str) -> MockServer {
    let server = MockServer::start().await;
    let id = id_token(upn);
    Mock::given(method("POST")).and(path("/oauth2/v2.0/token"))
        .respond_with(move |request: &Request| {
            let scope = url::form_urlencoded::parse(&request.body).find(|(name, _)| name == "scope").unwrap().1.into_owned();
            ResponseTemplate::new(200).set_body_json(json!({"token_type":"Bearer","expires_in":3600,
                "scope":scope,"access_token":"made-up-access-token-never-print","refresh_token":"made-up-refresh-never-print","id_token":id}))
        }).mount(&server).await;
    server
}

fn redirect(authorization: &Authorization) -> (Secret, SessionReport) {
    let url = url::Url::parse(authorization.url()).unwrap();
    let state = url
        .query_pairs()
        .find(|(name, _)| name == "state")
        .unwrap()
        .1
        .into_owned();
    let mut redirect = url::Url::parse(NATIVE_CLIENT_REDIRECT).unwrap();
    redirect
        .query_pairs_mut()
        .append_pair("code", "made-up-code")
        .append_pair("state", &state);
    (Secret::new(redirect.to_string()), SessionReport::default())
}

fn native_target(root: &std::path::Path) -> NativeTarget {
    NativeTarget::resolve(Alias::A, None, None, Some(root.as_os_str().to_owned())).unwrap()
}

fn assert_native_files(root: &std::path::Path, report: &AuthReport) {
    let target = NativeTarget::resolve(Alias::A, None, None, Some(root.as_os_str().to_owned())).unwrap();
    assert_eq!(KEYRING_APPLICATION, "ngteams-lab");
    assert_eq!(target.root(), root.join(".local/share/ngteams-lab/a"));
    let saved: toml::Value =
        toml::from_str(&std::fs::read_to_string(target.root().join("accounts.toml")).unwrap()).unwrap();
    let account = &saved["account"][0];
    assert_eq!(saved["account"].as_array().unwrap().len(), 1);
    assert_eq!(account["upn"].as_str(), Some(UPN));
    assert_eq!(account["oid"].as_str(), Some(OID));
    assert_eq!(account["tid"].as_str(), Some("00000000-0000-4000-8000-000000000001"));
    assert!(account["idp_hosts"].as_array().unwrap().is_empty());
    let native_region = target.root().join(format!("regions/{OID}.json"));
    assert_eq!(
        Region::from_json(&serde_json::from_slice(&std::fs::read(native_region).unwrap()).unwrap()),
        Some(report.skypetoken.as_ref().unwrap().as_ref().unwrap().region.clone())
    );
}

#[tokio::test]
async fn code_exchange_refresh_all_audiences_and_authz_run_on_the_supplied_clients() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let tokens = tokens(UPN).await;
    let authz = MockServer::start().await;
    let mut fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../../../../tests/fixtures/auth/authz-ok.json")).unwrap();
    fixture["response"]["body"]["tokens"]["skypeToken"] = json!("made-up-skype-token-never-print");
    Mock::given(method("POST"))
        .and(path("/authz"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture["response"]["body"].clone()))
        .mount(&authz)
        .await;
    let client = AuthzClient::with_endpoint(&format!("{}/authz", authz.uri())).unwrap();
    let store = MemorySecretStore::new();
    let options = AuthOptions {
        timeout: Duration::from_secs(1),
        skypetoken: true,
        other_audiences: true,
    };
    let report = authorize_with(
        &config,
        Alias::A,
        &store,
        &options,
        EntraConfig::teams_native().with_authority(tokens.uri()),
        AuthSteps {
            target: native_target(root.path()),
            redirect: async |authorization: &Authorization| Ok(redirect(authorization)),
            skype: async |tokens: &Tokens| skypetoken_with(&store, UPN, tokens, &client).await,
        },
    )
    .await
    .unwrap();
    assert_native_files(root.path(), &report);
    assert_eq!(report.audiences.len(), OTHER_AUDIENCES.len());
    assert!(report.audiences.iter().all(|outcome| outcome.token.is_ok()));
    assert!(matches!(report.skypetoken, Some(Ok(_))));
    assert_eq!(
        tokens.received_requests().await.unwrap().len(),
        2 + OTHER_AUDIENCES.len()
    );
    assert_eq!(authz.received_requests().await.unwrap().len(), 1);
    assert!(
        store
            .get(&teams_auth::refresh::refresh_token_key(UPN))
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .get(&teams_auth::authz::skypetoken_key(UPN))
            .await
            .unwrap()
            .is_some()
    );
    let shown = report.lines(SystemTime::now()).join("\n");
    for secret in [
        "made-up-access-token-never-print",
        "made-up-refresh-never-print",
        "made-up-skype-token-never-print",
        "made-up-code",
    ] {
        assert!(!shown.contains(secret));
    }
}

#[tokio::test]
async fn optional_grants_are_not_requested_and_a_refused_authz_is_a_reported_failure() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let tokens = tokens(UPN).await;
    let store = MemorySecretStore::new();
    let options = AuthOptions {
        timeout: Duration::from_secs(1),
        skypetoken: false,
        other_audiences: false,
    };
    let report = authorize_with(
        &config,
        Alias::A,
        &store,
        &options,
        EntraConfig::teams_native().with_authority(tokens.uri()),
        AuthSteps {
            target: native_target(root.path()),
            redirect: async |authorization: &Authorization| Ok(redirect(authorization)),
            skype: async |_: &Tokens| panic!("skypetoken disabled"),
        },
    )
    .await
    .unwrap();
    assert!(report.audiences.is_empty() && report.skypetoken.is_none());
    assert_eq!(tokens.received_requests().await.unwrap().len(), 2);
    let options = AuthOptions {
        skypetoken: true,
        ..options
    };
    let report = authorize_with(
        &config,
        Alias::A,
        &store,
        &options,
        EntraConfig::teams_native().with_authority(tokens.uri()),
        AuthSteps {
            target: native_target(root.path()),
            redirect: async |authorization: &Authorization| Ok(redirect(authorization)),
            skype: async |_: &Tokens| {
                Err(AuthzError::Refused {
                    status: 403,
                    code: None,
                })
            },
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        report.skypetoken,
        Some(Err(AuthzError::Refused { status: 403, .. }))
    ));
}

#[tokio::test]
async fn the_native_entry_keeps_the_configured_spelling_used_by_secret_keys() {
    let root = tempfile::tempdir().unwrap();
    let configured = UPN.to_ascii_uppercase();
    let mut config_text = format!("data_dir = {:?}\n", root.path().join("browser").to_string_lossy());
    for alias in ["a", "b", "c"] {
        writeln!(
            config_text,
            "[[accounts]]\nalias = {alias:?}\nupn = {:?}",
            if alias == "a" {
                configured.clone()
            } else {
                format!("synthetic-{alias}@example.test")
            }
        )
        .unwrap();
    }
    let config = LabConfig::parse(&config_text, root.path()).unwrap();
    let tokens = tokens(UPN).await;
    let store = MemorySecretStore::new();
    let _report = authorize_with(
        &config,
        Alias::A,
        &store,
        &AuthOptions {
            timeout: Duration::from_secs(1),
            skypetoken: false,
            other_audiences: false,
        },
        EntraConfig::teams_native().with_authority(tokens.uri()),
        AuthSteps {
            target: native_target(root.path()),
            redirect: async |authorization: &Authorization| Ok(redirect(authorization)),
            skype: async |_: &Tokens| panic!("skypetoken disabled"),
        },
    )
    .await
    .unwrap();
    assert!(
        store
            .get(&teams_auth::refresh::refresh_token_key(&configured))
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .get(&teams_auth::refresh::refresh_token_key(UPN))
            .await
            .unwrap()
            .is_none()
    );
    let target = NativeTarget::resolve(Alias::A, None, None, Some(root.path().as_os_str().to_owned())).unwrap();
    let file = target.root().join("accounts.toml");
    let saved: toml::Value = toml::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
    assert_eq!(saved["account"][0]["upn"].as_str(), Some(configured.as_str()));
}

#[tokio::test]
async fn a_failed_redirect_or_wrong_identity_stores_no_refresh_and_stops_before_more_grants() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let tokens = tokens("another@example.test").await;
    let store = MemorySecretStore::new();
    let options = AuthOptions {
        timeout: Duration::from_secs(1),
        skypetoken: true,
        other_audiences: true,
    };
    let outcome = authorize_with(
        &config,
        Alias::A,
        &store,
        &options,
        EntraConfig::teams_native().with_authority(tokens.uri()),
        AuthSteps {
            target: native_target(root.path()),
            redirect: async |_: &Authorization| Err(AuthError::InteractionNeeded),
            skype: async |_: &Tokens| panic!("no token after refused redirect"),
        },
    )
    .await;
    assert!(matches!(outcome, Err(AuthError::InteractionNeeded)));
    assert!(tokens.received_requests().await.unwrap().is_empty());
    let outcome = authorize_with(
        &config,
        Alias::A,
        &store,
        &options,
        EntraConfig::teams_native().with_authority(tokens.uri()),
        AuthSteps {
            target: native_target(root.path()),
            redirect: async |authorization: &Authorization| Ok(redirect(authorization)),
            skype: async |_: &Tokens| panic!("no token after identity mismatch"),
        },
    )
    .await;
    assert!(matches!(outcome, Err(AuthError::Token(_))));
    assert!(
        store
            .get(&teams_auth::refresh::refresh_token_key(UPN))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(tokens.received_requests().await.unwrap().len(), 1);
}

async fn page() -> crate::scenario::test_page::Page {
    crate::scenario::test_page::Page::new(|command| match command["method"].as_str().unwrap() {
        "Page.navigate" | "Browser.close" => Ok(json!({})),
        method => Err(format!("unexpected synthetic auth command {method}")),
    })
    .await
}

#[tokio::test]
async fn a_caught_redirect_closes_the_session_and_close_failure_is_not_hidden() {
    let root = tempfile::tempdir().unwrap();
    let authorization = Authorization::new(&EntraConfig::teams_native(), UPN, Scope::spaces()).unwrap();
    for managed in [true, false] {
        let mut page = page().await;
        let session = if managed {
            page.managed_session(root.path(), false).0
        } else {
            page.session(root.path(), false)
        };
        let (send, mut caught) = mpsc::unbounded_channel();
        let expected = redirect(&authorization).0;
        send.send(Secret::new(expected.expose())).unwrap();
        let result = redirect_from_session(session, &authorization, &mut caught, Duration::from_secs(1)).await;
        if managed {
            let (actual, browser) = result.unwrap();
            assert_eq!(actual.expose(), expected.expose());
            assert!(!browser.failed());
        } else {
            assert!(
                matches!(result, Err(AuthError::Session(_))),
                "an unconfirmed close fails even with a caught redirect"
            );
        }
        assert_eq!(
            page.commands()
                .iter()
                .map(|command| command["method"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["Page.navigate", "Browser.close"]
        );
    }
}

#[tokio::test]
async fn waiting_for_a_redirect_stops_on_foreign_hosts_expired_signin_and_lost_connections() {
    let root = tempfile::tempdir().unwrap();
    for (url, expected) in [
        ("https://synthetic.example.test/foreign", "elsewhere"),
        (
            "https://login.microsoftonline.com/common/oauth2/authorize",
            "interaction",
        ),
        (crate::session::TEAMS_URL, "timeout"),
    ] {
        let mut page = page().await;
        let mut session = page.session(root.path(), false);
        page.send("Page.frameNavigated", json!({"frame":{"id":"auth-frame","url":url}}));
        page.send("Page.frameStoppedLoading", json!({"frameId":"auth-frame"}));
        let (_send, mut caught) = mpsc::unbounded_channel();
        let result = within(
            Duration::from_millis(if expected == "interaction" { 6000 } else { 300 }),
            wait_for_redirect(&mut session, &mut caught),
        )
        .await;
        assert!(
            match expected {
                "elsewhere" =>
                    matches!(result, Err(AuthError::Elsewhere(ref host)) if host == "synthetic.example.test"),
                "interaction" => matches!(result, Err(AuthError::InteractionNeeded)),
                _ => matches!(result, Err(AuthError::Timeout(_))),
            },
            "{expected}: {result:?}"
        );
    }
    let mut page = page().await;
    let mut session = page.session(root.path(), false);
    drop(page);
    let (_send, mut caught) = mpsc::unbounded_channel();
    let result = within(Duration::from_secs(2), wait_for_redirect(&mut session, &mut caught)).await;
    assert!(matches!(result, Err(AuthError::ConnectionLost)));
}

#[cfg(unix)]
#[tokio::test]
async fn explicit_native_target_without_browser_never_exchanges_tokens_or_publishes_files() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let target = native_target(root.path());
    let store = MemorySecretStore::new();
    let options = AuthOptions {
        timeout: Duration::from_secs(1),
        skypetoken: true,
        other_audiences: false,
    };
    let error = authorize_target(&config, Alias::A, &store, &options, target.clone())
        .await
        .unwrap_err();
    assert!(matches!(error, AuthError::Session(SessionError::NoChrome(_))));
    assert!(!target.root().exists(), "no native account/region was published");
    assert!(
        !config.paths().profile_dir(Alias::A).exists(),
        "no browser profile was opened"
    );
}
