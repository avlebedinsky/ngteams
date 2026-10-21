//! The binary's refusals and its answer channel: only a local page, only in the UI sandbox, never a `test-support`
//! build; one answer line on stdout and nothing else there.

use std::ffi::OsStr;
use std::io::Write as _;
use std::process::{Command, Stdio};

use teams_auth::signin::{WindowAnswer, WindowRequest};
use teams_auth::token::NATIVE_CLIENT_REDIRECT;
use teams_signin::cli;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[test]
fn only_a_page_on_this_machine_is_local() {
    for local in [
        "http://127.0.0.1:8080/authorize?x=1",
        "https://localhost/authorize",
        "http://[::1]:9/a",
        "http://127.8.9.10/a",
    ] {
        assert!(cli::is_local_page(local), "{local}");
    }
    for remote in [
        "https://login.microsoftonline.com/organizations/oauth2/v2.0/authorize?client_id=x",
        "https://localhost.evil.test/a",
        "https://127.0.0.1.nip.io/a",
        "file:///etc/passwd",
        "ftp://127.0.0.1/a",
        "about:blank",
        "not a url",
    ] {
        assert!(!cli::is_local_page(remote), "{remote}");
    }
    assert!(cli::NOT_LIVE.contains("webkit-sign-in-guard-redirects.md"));
}

#[test]
fn the_one_mode_is_the_lab_s() {
    assert!(cli::parse_args(["--lab"]).is_ok());
    for args in [&[][..], &["--lab", "a"][..], &["--personal"][..], &["lab"][..]] {
        assert!(cli::parse_args(args.iter().copied()).is_err(), "{args:?}");
    }
}

#[test]
fn a_window_needs_the_sandbox_and_no_x_display() {
    let refusal = cli::window_refusal(Some(OsStr::new(":0")));
    assert!(refusal.is_some_and(|refusal| refusal.contains("DISPLAY")));
    // this test process isn't in the UI sandbox
    if std::env::var_os("UI_SANDBOX_DIR").is_none() {
        assert!(cli::window_refusal(None).is_some());
    }
}

#[test]
fn a_test_support_build_refuses_and_answers_on_stdout_alone() -> TestResult {
    // a test build unifies `test-support` into the binary (the dev-dependency on itself), so it must refuse
    assert!(cli::test_support_refusal().is_some());
    let mut child = Command::new(env!("CARGO_BIN_EXE_teams-signin"))
        .arg("--lab")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DISPLAY")
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let request = WindowRequest {
        authorize: "http://127.0.0.1:9/authorize".into(),
        redirect_uri: NATIVE_CLIENT_REDIRECT.into(),
    };
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(format!("{}\n", request.to_line()).as_bytes())?;
    }
    let output = child.wait_with_output()?;
    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "one answer line and nothing else: {stdout:?}");
    let WindowAnswer::Failed(reason) = WindowAnswer::parse(lines[0])? else {
        panic!("a failure: {stdout}");
    };
    assert!(reason.contains("test-support"), "{reason}");
    assert!(String::from_utf8(output.stderr)?.contains("test-support"));
    Ok(())
}
