//! Catching the authorization redirect inside the armed fence (`proto-lab auth`'s browser step): the fence's patterns,
//! and, with Chrome installed, an authorize request on a local stand-in for Entra whose 302 goes to the registered
//! redirect URI (an `https`-style page and the custom scheme), one that gets there by script, and an unrelated redirect
//! that must go on. The redirect never loads, the code comes out with the request's state, and nothing counts as a deny.
//!
//! The browser runs headless on a throwaway profile against a local mock server; see `common` for how Chrome is found.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use proto_lab::arming::{ArmOptions, ArmedBrowser, ArmedTarget};
use proto_lab::browser::{Browser, LaunchOptions};
use proto_lab::classify::Classifier;
use proto_lab::fence_http::{CatchError, HttpFence, RedirectCatch};
use proto_lab::fence_ws::{GuardScript, WsFence};
use serde_json::{Value, json};
use teams_auth::pkce::Pkce;
use teams_auth::redirect::RedirectError;
use teams_auth::secrets::Secret;
use teams_auth::token::{Authorization, EntraConfig, NATIVE_CLIENT_REDIRECT, Scope};
use teams_fence::{FailClosed, Fence, FenceLog, Roster};
use tokio::sync::mpsc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const STEP: Duration = Duration::from_secs(15);
const CODE: &str = "made-up-authorization-code";
const CUSTOM_SCHEME: &str = "msauth.com.microsoft.teams2://auth";
const NATIVE_PATH: &str = "/common/oauth2/nativeclient";

// --- the fence's patterns ---

fn fence(lab: &Lab, catch: Option<RedirectCatch>) -> HttpFence {
    let http = HttpFence::new(Classifier::new(), Arc::clone(&lab.fence), Arc::new(FailClosed));
    match catch {
        Some(catch) => http.with_redirect_catch(catch),
        None => http,
    }
}

#[test]
fn only_a_fence_with_a_catch_pauses_responses_of_the_login_origins() {
    // every fence pauses the call start's answer, which the call registry reads (`tests/call_registry.rs`)
    let call_start_answer = json!({"urlPattern": teams_fence::CALL_START_URL, "requestStage": "Response"});
    let lab = lab().unwrap();
    assert_eq!(
        fence(&lab, None).fetch_patterns(),
        json!({"patterns": [{"urlPattern": "*"}, call_start_answer]})
    );
    let config = EntraConfig::teams_native();
    let (catch, _caught) = RedirectCatch::new(&config.authorize_endpoint(), config.redirect_uri()).unwrap();
    assert_eq!(
        fence(&lab, Some(catch)).fetch_patterns(),
        json!({"patterns": [
            {"urlPattern": "*"},
            call_start_answer,
            {"urlPattern": "https://login.microsoftonline.com/*", "requestStage": "Response"},
        ]})
    );
}

#[test]
fn a_catch_needs_a_bare_redirect_uri_and_a_web_authorize_endpoint() {
    let authorize = EntraConfig::teams_native().authorize_endpoint();
    for redirect in ["https://login.microsoftonline.com/cb?x=1", "not a url"] {
        assert!(
            matches!(
                RedirectCatch::new(&authorize, redirect),
                Err(CatchError::RedirectUri(RedirectError::InvalidRedirectUri))
            ),
            "{redirect}"
        );
    }
    for endpoint in ["msauth.com.microsoft.teams2://auth", "not a url"] {
        let error = RedirectCatch::new(endpoint, NATIVE_CLIENT_REDIRECT).unwrap_err();
        assert!(matches!(error, CatchError::AuthorizeEndpoint), "{endpoint}: {error:?}");
        // the authority is what's wrong, not the redirect URI
        assert!(error.to_string().contains("authorize endpoint"), "{error}");
    }
    assert!(RedirectCatch::new(&authorize, CUSTOM_SCHEME).is_ok());
}

// --- in the browser ---

struct Lab {
    _dir: tempfile::TempDir,
    fence: Arc<Fence>,
}

fn lab() -> TestResult<Lab> {
    let dir = tempfile::tempdir()?;
    let fence = Arc::new(Fence::new(
        Roster::new([], []),
        FenceLog::open(&dir.path().join("fence.log"))?,
    ));
    Ok(Lab { _dir: dir, fence })
}

/// The stand-in for Entra's authorize endpoint: a 302 to `redirect` with the code and the request's own state, as
/// Entra answers a signed-in profile.
struct Authorize {
    redirect: String,
}

impl Respond for Authorize {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let state = request
            .url
            .query_pairs()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.into_owned())
            .unwrap_or_default();
        ResponseTemplate::new(302).insert_header("Location", format!("{}?code={CODE}&state={state}", self.redirect))
    }
}

/// A page that sends the browser to the redirect by script instead of a 302.
struct ScriptedAuthorize;

impl Respond for ScriptedAuthorize {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let state = request
            .url
            .query_pairs()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.into_owned())
            .unwrap_or_default();
        let page =
            format!("<!doctype html><script>location.replace('{NATIVE_PATH}?code={CODE}&state={state}')</script>");
        ResponseTemplate::new(200).set_body_raw(page, "text/html")
    }
}

async fn serve(redirect: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/organizations/oauth2/v2.0/authorize"))
        .respond_with(Authorize {
            redirect: redirect.replace("{server}", &server.uri()),
        })
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/scripted/oauth2/v2.0/authorize"))
        .respond_with(ScriptedAuthorize)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/hop"))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", "/landing"))
        .mount(&server)
        .await;
    for page in ["/landing", NATIVE_PATH] {
        Mock::given(method("GET"))
            .and(path(page))
            .respond_with(ResponseTemplate::new(200).set_body_raw("<!doctype html><p>landed", "text/html"))
            .mount(&server)
            .await;
    }
    server
}

/// The requests the mock server received for `path`.
async fn received(server: &MockServer, wanted: &str) -> usize {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|request| request.url.path() == wanted)
        .count()
}

/// An authorization against the local stand-in, with `redirect_uri` registered.
fn authorization(server: &MockServer, tenant: &str, redirect_uri: &str) -> TestResult<Authorization> {
    let config = EntraConfig::teams_native()
        .with_authority(format!("{}/{tenant}/", server.uri()))
        .with_redirect_uri(redirect_uri);
    Ok(Authorization::with_pkce(
        &config,
        "a@lab.example.test",
        Scope::spaces(),
        Pkce::generate()?,
        teams_auth::pkce::random_state()?,
    ))
}

struct Run {
    armed: ArmedBrowser,
    page: ArmedTarget,
    _lab: Lab,
    _profile: tempfile::TempDir,
}

/// An armed browser on a throwaway profile, catching redirects to `redirect_uri` from the server's origin when
/// `catch` is set, with one armed tab.
async fn start(
    chrome: &Path,
    server: &MockServer,
    redirect_uri: &str,
    catch: bool,
) -> TestResult<(Run, mpsc::UnboundedReceiver<Secret>)> {
    let profile = tempfile::tempdir()?;
    let browser = Browser::launch(chrome, &LaunchOptions::new(profile.path().join("p")), STEP).await?;
    let lab = lab()?;
    let authorize = format!("{}/organizations/oauth2/v2.0/authorize", server.uri());
    let (redirect_catch, caught) = RedirectCatch::new(&authorize, redirect_uri)?;
    let http = fence(&lab, catch.then_some(redirect_catch));
    let ws = WsFence::new(Arc::clone(&lab.fence), Arc::new(FailClosed), GuardScript::generate()?);
    let options = ArmOptions {
        service_worker_origins: vec![server.uri()],
    };
    let armed = ArmedBrowser::arm(browser, http, ws, &options).await?;
    let page = armed.open_page(STEP).await?;
    let run = Run {
        armed,
        page,
        _lab: lab,
        _profile: profile,
    };
    Ok((run, caught))
}

impl Run {
    /// Navigates the tab and gives back `Page.navigate`'s answer (its `errorText` when the navigation failed).
    async fn navigate(&self, url: &str) -> TestResult<Value> {
        let params = json!({ "url": url });
        Ok(self
            .armed
            .cdp()
            .call(Some(&self.page.session), "Page.navigate", params)
            .await?)
    }

    async fn location(&self) -> TestResult<String> {
        let params = json!({"expression": "location.href", "returnByValue": true});
        let result = self
            .armed
            .cdp()
            .call(Some(&self.page.session), "Runtime.evaluate", params)
            .await?;
        Ok(result["result"]["value"].as_str().unwrap_or_default().to_owned())
    }

    async fn finish(self) -> TestResult {
        let verdict = self.armed.verdict();
        self.armed.close(Duration::from_secs(10)).await?;
        verdict.map_err(|failed| format!("the run failed: {failed:?}"))?;
        Ok(())
    }
}

async fn next_caught(caught: &mut mpsc::UnboundedReceiver<Secret>) -> TestResult<Secret> {
    Ok(tokio::time::timeout(STEP, caught.recv())
        .await
        .map_err(|_| "nothing caught")?
        .ok_or("the catch is gone")?)
}

/// Authorizes through the armed browser with the redirect at `redirect` and checks the code comes out of the catch
/// while the redirect never loads.
async fn caught_from(redirect_uri: &str, location: &str) -> TestResult {
    let Some(chrome) = common::chrome_or_skip() else {
        return Ok(());
    };
    let server = serve(location).await;
    let redirect_uri = redirect_uri.replace("{server}", &server.uri());
    let (run, mut caught) = start(&chrome, &server, &redirect_uri, true).await?;
    let authorization = authorization(&server, "organizations", &redirect_uri)?;
    let mut events = run.armed.watch();
    let outcome = async {
        let navigated = run.navigate(authorization.url()).await?;
        assert!(
            navigated.get("errorText").is_some(),
            "the redirect isn't followed: {navigated}"
        );
        let url = next_caught(&mut caught).await?;
        assert_eq!(authorization.code_from_redirect(url.expose())?.expose(), CODE);
        assert_eq!(received(&server, NATIVE_PATH).await, 0, "the redirect never loads");
        while let Ok(event) = events.try_recv() {
            let paused = event.method == "Fetch.requestPaused";
            let url = event.params["request"]["url"].as_str().unwrap_or_default();
            assert!(
                !(paused && url.contains(NATIVE_PATH)),
                "caught at the response stage, before the browser asked for the redirect"
            );
        }
        assert!(run.armed.denials().is_empty(), "{:?}", run.armed.denials());
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    run.finish().await?;
    outcome
}

#[tokio::test]
async fn the_redirect_to_the_registered_page_is_caught_at_the_response_stage() {
    let redirect = format!("{{server}}{NATIVE_PATH}");
    caught_from(&redirect, &redirect).await.unwrap();
}

#[tokio::test]
async fn the_redirect_to_the_custom_scheme_is_caught_at_the_response_stage() {
    caught_from(CUSTOM_SCHEME, CUSTOM_SCHEME).await.unwrap();
}

#[tokio::test]
async fn a_scripted_navigation_to_the_redirect_is_caught_at_the_request_stage() {
    let Some(chrome) = common::chrome_or_skip() else {
        return;
    };
    let server = serve("unused").await;
    let redirect_uri = format!("{}{NATIVE_PATH}", server.uri());
    let (run, mut caught) = start(&chrome, &server, &redirect_uri, true).await.unwrap();
    let authorization = authorization(&server, "scripted", &redirect_uri).unwrap();
    let outcome = async {
        run.navigate(authorization.url()).await?;
        let url = next_caught(&mut caught).await?;
        assert_eq!(authorization.code_from_redirect(url.expose())?.expose(), CODE);
        assert_eq!(received(&server, NATIVE_PATH).await, 0, "the redirect never loads");
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    run.finish().await.unwrap();
    outcome.unwrap();
}

#[tokio::test]
async fn other_redirects_of_the_login_origin_go_on_and_nothing_is_caught() {
    let Some(chrome) = common::chrome_or_skip() else {
        return;
    };
    let server = serve("unused").await;
    let redirect_uri = format!("{}{NATIVE_PATH}", server.uri());
    let (run, mut caught) = start(&chrome, &server, &redirect_uri, true).await.unwrap();
    let outcome = async {
        let navigated = run.navigate(&format!("{}/hop", server.uri())).await?;
        assert!(navigated.get("errorText").is_none(), "{navigated}");
        assert_eq!(run.location().await?, format!("{}/landing", server.uri()));
        assert!(caught.try_recv().is_err(), "nothing caught");
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    run.finish().await.unwrap();
    outcome.unwrap();
}

#[tokio::test]
async fn without_a_catch_the_redirect_loads() {
    // what the catch prevents: the same authorize request, unfenced for responses, lands on the redirect page
    let Some(chrome) = common::chrome_or_skip() else {
        return;
    };
    let redirect = format!("{{server}}{NATIVE_PATH}");
    let server = serve(&redirect).await;
    let redirect_uri = redirect.replace("{server}", &server.uri());
    let (run, mut caught) = start(&chrome, &server, &redirect_uri, false).await.unwrap();
    let authorization = authorization(&server, "organizations", &redirect_uri).unwrap();
    let outcome = async {
        let navigated = run.navigate(authorization.url()).await?;
        assert!(navigated.get("errorText").is_none(), "{navigated}");
        assert_eq!(received(&server, NATIVE_PATH).await, 1);
        assert!(caught.try_recv().is_err(), "nothing caught");
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    run.finish().await.unwrap();
    outcome.unwrap();
}
