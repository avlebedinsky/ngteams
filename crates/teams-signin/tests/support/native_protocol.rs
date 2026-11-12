use std::io::{BufRead as _, Write as _};
use std::path::PathBuf;
use std::sync::Arc;
use teams_auth::signin::{SignInAnswer, SignInRequest};
use teams_auth::token::NATIVE_CLIENT_REDIRECT;
use teams_signin::window::{Bound, EngineSource, WindowConfig, Windows};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

pub fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("synthetic native protocol failed: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> TestResult {
    if std::env::args_os().nth(1).is_some_and(|a| a == "--guarded-signin") {
        peer()?;
        // Stay alive until Job termination, like the real GUI worker. Returning now races LLVM's exit writer
        // against the parent's kill and can truncate a raw coverage profile.
        let mut line = String::new();
        let _ = std::io::stdin().lock().read_line(&mut line);
        return Ok(());
    }
    let service = Windows::new("com.nickalie.ngteams.signin.protocol.synthetic");
    assert!(teams_signin::test_support_enabled());
    assert!(service.engine_source(std::path::Path::new("unused")).is_ok());
    for (scenario, expected) in [
        ("closed", "closed"),
        ("refused", "refused"),
        ("load", "load"),
        ("failed", "failed"),
        ("redirect", "redirect"),
        ("request-redirect", "redirect"),
        ("malformed", "generic"),
    ] {
        let answer = service.clone().ask(config(scenario)?);
        verify(expected, answer)?;
    }

    for broken in ["redirect", "host", "worker"] {
        let mut config = config("closed")?;
        match broken {
            "redirect" => config.request.redirect_uri = "invalid".into(),
            "host" => config.request.authorize = "https://example.test/".into(),
            _ => config.extension = PathBuf::from("C:/synthetic-missing/ngteams-webview.exe"),
        }
        assert!(matches!(service.ask(config), SignInAnswer::Failed(_)));
    }
    pin_worker()?;
    println!("synthetic native parent decisions, worker errors and pinned identity passed");
    Ok(())
}

fn verify(expected: &str, answer: SignInAnswer) -> TestResult {
    match (expected, answer) {
        ("closed", SignInAnswer::Closed) => {}
        ("refused", SignInAnswer::Refused(host)) => assert_eq!(host, "sso.example.test"),
        ("load", SignInAnswer::Failed(reason)) => assert!(reason.contains("error 42")),
        ("failed", SignInAnswer::Failed(reason)) => assert_eq!(reason, "synthetic failure"),
        ("redirect", SignInAnswer::Redirect(uri)) => assert!(uri.expose().contains("code=synthetic")),
        ("generic", SignInAnswer::Failed(reason)) => assert!(reason.contains("could not finish")),
        (_, _) => return Err("unexpected synthetic worker answer".into()),
    }
    Ok(())
}

fn pin_worker() -> TestResult {
    let directory =
        tempfile::tempdir_in(std::env::var_os("UI_SANDBOX_DIR").ok_or("private fixture directory missing")?)?;
    let source = directory.path().join("absent-parent.exe");
    assert!(teams_signin::window::extension_beside(&source).is_none());
    let worker = directory.path().join(teams_signin::window::EXTENSION_FILE);
    std::fs::copy(std::env::current_exe()?, &worker)?;
    assert_eq!(teams_signin::window::extension_beside(&source), Some(worker.clone()));
    assert_eq!(teams_signin::window::extension_beside(&source), Some(worker.clone()));
    assert!(teams_signin::window::pin_worker().is_ok());
    assert!(std::fs::OpenOptions::new().write(true).open(worker).is_err());
    Ok(())
}

fn config(scenario: &str) -> TestResult<WindowConfig> {
    Ok(WindowConfig {
        engine: EngineSource,
        bound: Bound::Hosts(Arc::new(teams_signin::hosts::SignInHosts::entra_with(&[])?)),
        request: SignInRequest {
            authorize: format!("https://login.microsoftonline.com/{scenario}"),
            redirect_uri: NATIVE_CLIENT_REDIRECT.into(),
        },
        extension: std::env::current_exe()?,
        setup: Box::new(|guard| guard),
    })
}

fn peer() -> TestResult {
    let mut input = std::io::stdin().lock();
    let mut line = String::new();
    input.read_line(&mut line)?;
    let start: serde_json::Value = serde_json::from_str(&line)?;
    let scenario = start["authorize"]
        .as_str()
        .ok_or("missing synthetic authorize URI")?
        .rsplit('/')
        .next()
        .ok_or("missing scenario")?;
    let redirect = format!("{NATIVE_CLIENT_REDIRECT}?code=synthetic&state=synthetic");
    let navigation = |url: &str| serde_json::json!({"Navigation":url});
    match scenario {
        "closed" => {
            assert!(exchange(
                &mut input,
                &navigation("https://login.microsoftonline.com/next")
            )?);
            for (method, expected) in [("GET", true), ("POST", false)] {
                assert_eq!(
                    exchange(
                        &mut input,
                        &serde_json::json!({"Request":{"method":method,"url":"https://login.microsoftonline.com/unknown"}})
                    )?,
                    expected
                );
            }
            send(&serde_json::json!({"NetworkFailure":"net::ERR_SYNTHETIC"}))?;
            send(&serde_json::json!("Closed"))?;
        }
        "refused" => {
            assert!(!exchange(
                &mut input,
                &navigation("https://sso.example.test/private?SAMLRequest=synthetic")
            )?);
            send(&serde_json::json!("Closed"))?;
        }
        "load" => send(&serde_json::json!({"LoadFailed":42}))?,
        "failed" => send(&serde_json::json!({"Failed":"synthetic failure"}))?,
        "redirect" => {
            exchange(&mut input, &navigation(&redirect))?;
        }
        "request-redirect" => {
            exchange(
                &mut input,
                &serde_json::json!({"Request":{"method":"GET","url":redirect}}),
            )?;
        }
        _ => println!("invalid protocol"),
    }
    Ok(())
}

fn send(value: &serde_json::Value) -> std::io::Result<()> {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{value}")?;
    out.flush()
}

fn exchange(input: &mut impl std::io::BufRead, value: &serde_json::Value) -> TestResult<bool> {
    send(value)?;
    let mut line = String::new();
    input.read_line(&mut line)?;
    Ok(serde_json::from_str(&line)?)
}
