//! Native worker entry point rejects invocations without a private parent protocol, before opening a browser.
use std::process::{Command, Stdio};

#[test]
fn native_worker_rejects_an_unconnected_invocation_without_opening_a_browser() {
    let output = Command::new(env!("CARGO_BIN_EXE_ngteams-webview"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    #[cfg(unix)]
    {
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("Windows sign-in worker"));
    }
    #[cfg(windows)]
    {
        assert!(output.stderr.is_empty());
        let failure: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(failure["Failed"].as_str().unwrap().contains("started by ngteams"));
    }
}

#[cfg(windows)]
#[test]
fn native_worker_rejects_a_closed_private_pipe_before_opening_a_browser() {
    let output = Command::new(env!("CARGO_BIN_EXE_ngteams-webview"))
        .arg("--guarded-signin")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.stderr.is_empty());
    let failure: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(failure["Failed"].as_str().unwrap().contains("private pipe failed"));
}
