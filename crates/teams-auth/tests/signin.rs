//! The parent's side of the sign-in window: the request line, the answer line, the code taken from a redirect only
//! when it answers this request, and the code redeemed only for the account the ID token names. The window is a thread
//! on a socket pair, or `sh` standing in for the `teams-signin` binary.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::process::Command;
use std::thread;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use teams_auth::redirect::RedirectError;
use teams_auth::refresh::refresh_token_key;
use teams_auth::secrets::{MemorySecretStore, Secret, SecretStore};
use teams_auth::signin::{
    LINE_LIMIT, REASON_LIMIT, WindowAnswer, WindowError, WindowRequest, WindowSignInError, code_from_window, read_line,
    run_window, sign_in_with_window,
};
use teams_auth::token::{
    Authorization, EntraConfig, NATIVE_CLIENT_REDIRECT, Scope, TEAMS_NATIVE_CLIENT_ID, TokenClient, TokenError,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const ACCOUNT: &str = "a@lab.example.test";
const CODE: &str = "made-up-authorization-code-0123456789";
const OID: &str = "00000000-0000-4000-8000-00000000000a";
const TENANT: &str = "00000000-0000-4000-8000-0000000000ff";

fn authorization() -> TestResult<Authorization> {
    Ok(Authorization::new(
        &EntraConfig::teams_native(),
        ACCOUNT,
        Scope::spaces(),
    )?)
}

fn redirect_to(state: &str) -> String {
    format!("{NATIVE_CLIENT_REDIRECT}?code={CODE}&state={state}&session_state=made-up")
}

/// A window on the other end of a socket pair: reads the request, hands it to `check`, answers `answer`.
fn fake_window(answer: String, check: impl FnOnce(WindowRequest) + Send + 'static) -> TestResult<UnixStream> {
    let (ours, theirs) = UnixStream::pair()?;
    thread::spawn(move || -> Result<(), WindowError> {
        let mut reader = BufReader::new(theirs.try_clone().map_err(WindowError::Io)?);
        let mut line = String::new();
        reader.read_line(&mut line).map_err(WindowError::Io)?;
        check(WindowRequest::parse(line.trim_end())?);
        let mut writer = theirs;
        writer.write_all(answer.as_bytes()).map_err(WindowError::Io)
    });
    Ok(ours)
}

fn ask(authorization: &Authorization, answer: String) -> TestResult<Result<Secret, WindowError>> {
    let window = fake_window(answer, |_| {})?;
    Ok(code_from_window(authorization, window.try_clone()?, window))
}

/// Neither the code nor the state nor the URL shows in `error`'s message or `Debug`.
fn assert_hidden(error: &dyn std::fmt::Debug, display: &str, state: &str) {
    let shown = format!("{display} {error:?}");
    for secret in [CODE, state, "nativeclient?"] {
        assert!(!shown.contains(secret), "{shown}");
    }
}

// ---- the lines ----

#[test]
fn the_request_line_carries_the_authorize_url_and_the_redirect_uri() -> TestResult {
    let authorization = authorization()?;
    let request = WindowRequest::for_authorization(&authorization);
    let line = request.to_line();
    assert!(!line.contains('\n'));
    let parsed = WindowRequest::parse(&line)?;
    assert_eq!(parsed.authorize, authorization.url());
    assert_eq!(parsed.redirect_uri, NATIVE_CLIENT_REDIRECT);
    assert!(
        !format!("{parsed:?}").contains(authorization.state()),
        "the state stays out of Debug"
    );
    Ok(())
}

#[test]
fn a_request_line_is_read_strictly() {
    let bad = [
        String::from("not json"),
        String::from("[]"),
        json!({"authorize": "https://example.test/a"}).to_string(),
        json!({"authorize": "https://example.test/a", "redirect_uri": NATIVE_CLIENT_REDIRECT, "more": 1}).to_string(),
        json!({"authorize": 1, "redirect_uri": NATIVE_CLIENT_REDIRECT}).to_string(),
        json!({"authorize": "https://example.test/a", "redirect_uri": "https://x.test/r?q=1"}).to_string(),
    ];
    for line in bad {
        assert!(
            matches!(WindowRequest::parse(&line), Err(WindowError::Malformed)),
            "{line}"
        );
    }
    let long = "x".repeat(LINE_LIMIT + 1);
    assert!(matches!(WindowRequest::parse(&long), Err(WindowError::TooLong)));
}

#[test]
fn answer_lines_round_trip_and_hide_the_redirect() -> TestResult {
    let url = redirect_to("made-up-state");
    let redirect = WindowAnswer::Redirect(Secret::new(url.clone()));
    assert_eq!(format!("{redirect:?}"), "Redirect(<redacted>)");
    let WindowAnswer::Redirect(back) = WindowAnswer::parse(redirect.to_line().expose())? else {
        panic!("a redirect");
    };
    assert_eq!(back.expose(), url);
    assert!(matches!(
        WindowAnswer::parse(WindowAnswer::Closed.to_line().expose())?,
        WindowAnswer::Closed
    ));
    let long = "é".repeat(REASON_LIMIT);
    let WindowAnswer::Failed(reason) = WindowAnswer::parse(WindowAnswer::Failed(long).to_line().expose())? else {
        panic!("a failure");
    };
    assert!(reason.len() <= REASON_LIMIT && reason.chars().all(|c| c == 'é'));
    for line in [
        json!({"closed": false}).to_string(),
        json!({"redirect": url, "closed": true}).to_string(),
        json!({"failed": "x".repeat(REASON_LIMIT + 1)}).to_string(),
        json!({"other": "x"}).to_string(),
        json!({}).to_string(),
    ] {
        assert!(
            matches!(WindowAnswer::parse(&line), Err(WindowError::Malformed)),
            "{line}"
        );
    }
    Ok(())
}

#[test]
fn a_line_longer_than_the_limit_is_refused() -> TestResult {
    let long = format!("{}\n", "x".repeat(LINE_LIMIT + 1));
    assert!(matches!(read_line(long.as_bytes()), Err(WindowError::TooLong)));
    let exact = format!("{}\n", "x".repeat(LINE_LIMIT));
    assert_eq!(read_line(exact.as_bytes())?.map(|line| line.len()), Some(LINE_LIMIT));
    assert!(read_line(&b""[..])?.is_none());
    assert!(matches!(read_line(&b"\xff\n"[..]), Err(WindowError::Malformed)));
    Ok(())
}

// ---- the code from the window ----

#[test]
fn the_window_s_redirect_gives_the_code() -> TestResult {
    let authorization = authorization()?;
    let expected = WindowRequest::for_authorization(&authorization);
    let answer = format!(
        "{}\n",
        WindowAnswer::Redirect(Secret::new(redirect_to(authorization.state())))
            .to_line()
            .expose()
    );
    let window = fake_window(answer, move |request| assert_eq!(request, expected))?;
    let code = code_from_window(&authorization, window.try_clone()?, window)?;
    assert_eq!(code.expose(), CODE);
    Ok(())
}

#[test]
fn a_redirect_for_another_request_gives_no_code() -> TestResult {
    let authorization = authorization()?;
    let answer = format!(
        "{}\n",
        WindowAnswer::Redirect(Secret::new(redirect_to("another-request-s-state")))
            .to_line()
            .expose()
    );
    let error = ask(&authorization, answer)?.expect_err("another state");
    assert!(
        matches!(error, WindowError::Redirect(RedirectError::StateMismatch)),
        "{error:?}"
    );
    assert_hidden(&error, &error.to_string(), authorization.state());

    let elsewhere = format!(
        "{}\n",
        WindowAnswer::Redirect(Secret::new(format!(
            "https://elsewhere.example.test/common/oauth2/nativeclient?code={CODE}&state={}",
            authorization.state()
        )))
        .to_line()
        .expose()
    );
    let error = ask(&authorization, elsewhere)?.expect_err("not the redirect URI");
    assert!(
        matches!(error, WindowError::Redirect(RedirectError::NotTheRedirect)),
        "{error:?}"
    );
    assert_hidden(&error, &error.to_string(), authorization.state());
    Ok(())
}

#[test]
fn a_window_closed_failed_or_gone_gives_no_code() -> TestResult {
    let authorization = authorization()?;
    let closed = ask(&authorization, format!("{}\n", WindowAnswer::Closed.to_line().expose()))?;
    assert!(matches!(closed, Err(WindowError::Closed)));
    let failed = ask(
        &authorization,
        format!(
            "{}\n",
            WindowAnswer::Failed("the guard refused to arm".into())
                .to_line()
                .expose()
        ),
    )?;
    assert!(matches!(failed, Err(WindowError::Failed(reason)) if reason == "the guard refused to arm"));
    assert!(matches!(
        ask(&authorization, String::new())?,
        Err(WindowError::NoAnswer)
    ));
    assert!(matches!(
        ask(&authorization, "{\n".into())?,
        Err(WindowError::Malformed)
    ));
    Ok(())
}

#[test]
fn run_window_starts_the_window_and_reads_its_answer() -> TestResult {
    let authorization = authorization()?;
    let answer = WindowAnswer::Redirect(Secret::new(redirect_to(authorization.state()))).to_line();
    let mut command = Command::new("sh");
    // the stand-in reads the request line, checks it names the redirect URI, and answers
    command
        .arg("-c")
        .arg(r#"read -r line; case "$line" in *nativeclient*) printf '%s\n' "$ANSWER";; *) exit 3;; esac"#)
        .env("ANSWER", answer.expose());
    let code = run_window(command, &authorization)?;
    assert_eq!(code.expose(), CODE);

    let missing = run_window(Command::new("/nonexistent/teams-signin"), &authorization);
    assert!(matches!(missing, Err(WindowError::Spawn(_))));
    Ok(())
}

// ---- the code redeemed for the right account only ----

fn id_token(username: &str) -> TestResult<String> {
    let header = URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"RS256","kid":"test"}"#);
    let claims = json!({
        "aud": TEAMS_NATIVE_CLIENT_ID,
        "oid": OID,
        "tid": TENANT,
        "preferred_username": username,
    });
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims)?);
    Ok(format!("{header}.{payload}.c2lnbmF0dXJlLW5vdC1jaGVja2Vk"))
}

async fn sign_in_as(username: &str) -> TestResult<(Result<(), WindowSignInError>, MemorySecretStore)> {
    let server = MockServer::start().await;
    let body: Value = json!({
        "token_type": "Bearer",
        "scope": "https://api.spaces.skype.com/.default",
        "expires_in": 3600,
        "access_token": "made-up-access-token",
        "refresh_token": "made-up-refresh-token",
        "id_token": id_token(username)?,
    });
    Mock::given(method("POST"))
        .and(path("/organizations/oauth2/v2.0/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    let config = EntraConfig::teams_native().with_authority(format!("{}/organizations/", server.uri()));
    let client = TokenClient::new(config.clone())?;
    let authorization = Authorization::new(&config, ACCOUNT, Scope::spaces())?;
    let answer = WindowAnswer::Redirect(Secret::new(redirect_to(authorization.state()))).to_line();
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(r#"read -r line; printf '%s\n' "$ANSWER""#)
        .env("ANSWER", answer.expose());
    let store = MemorySecretStore::new();
    let result = sign_in_with_window(&client, &store, &authorization, command)
        .await
        .map(|sign_in| assert_eq!(sign_in.account.username, username));
    Ok((result, store))
}

#[tokio::test]
async fn the_code_is_redeemed_for_the_account_the_id_token_names() -> TestResult {
    let (result, store) = sign_in_as(ACCOUNT).await?;
    result?;
    let stored = store.get(&refresh_token_key(ACCOUNT)).await?;
    assert_eq!(stored.as_ref().map(Secret::expose), Some("made-up-refresh-token"));
    Ok(())
}

#[tokio::test]
async fn another_account_signed_in_through_the_window_stores_nothing() -> TestResult {
    let (result, store) = sign_in_as("b@lab.example.test").await?;
    assert!(
        matches!(result, Err(WindowSignInError::Token(TokenError::WrongAccount { .. }))),
        "{result:?}"
    );
    assert!(store.get(&refresh_token_key(ACCOUNT)).await?.is_none());
    assert!(store.get(&refresh_token_key("b@lab.example.test")).await?.is_none());
    Ok(())
}
