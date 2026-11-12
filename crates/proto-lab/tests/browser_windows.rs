//! Native browser ownership/refusal contracts in the authorized VM console; blank disposable profiles only.
#![cfg(windows)]

use std::time::Duration;

use proto_lab::browser::{Browser, BrowserError, Display, LaunchOptions, installed_edge};
use proto_lab::cdp::Cdp;
use serde_json::json;

#[tokio::test]
async fn headed_and_unverified_browsers_are_refused_before_creating_a_profile() {
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join("profile");
    let missing = root.path().join("unverified.exe");
    let mut options = LaunchOptions::new(profile.clone());
    for display in [Display::Headed, Display::Headless] {
        options.display = display;
        assert!(matches!(
            Browser::launch(&missing, &options, Duration::from_secs(1)).await,
            Err(BrowserError::Profile { .. })
        ));
        assert!(!profile.exists());
    }
}

#[tokio::test]
async fn a_startup_timeout_removes_the_stale_debugging_endpoint_and_terminates_its_job() {
    let root = tempfile::tempdir().unwrap();
    let stale = root.path().join("DevToolsActivePort");
    std::fs::write(&stale, "40123\n/devtools/browser/stale\n").unwrap();
    let options = LaunchOptions::new(root.path().to_owned());
    assert!(matches!(
        Browser::launch(&installed_edge().unwrap(), &options, Duration::ZERO).await,
        Err(BrowserError::Timeout(_))
    ));
    assert_ne!(
        std::fs::read_to_string(stale).ok().as_deref(),
        Some("40123\n/devtools/browser/stale\n")
    );
}

#[tokio::test]
async fn explicit_termination_and_expired_grace_reap_the_owned_browser() {
    for explicit in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let browser = Browser::launch(
            &installed_edge().unwrap(),
            &LaunchOptions::new(root.path().to_owned()),
            Duration::from_secs(20),
        )
        .await
        .unwrap();
        let status = if explicit {
            browser.start_kill().unwrap();
            tokio::time::timeout(Duration::from_secs(10), browser.reap())
                .await
                .unwrap()
                .unwrap()
        } else {
            tokio::time::timeout(Duration::from_secs(10), browser.wait_or_kill(Duration::ZERO))
                .await
                .unwrap()
                .unwrap()
        };
        assert!(!status.success());
    }
}

#[tokio::test]
async fn a_confirmed_cdp_close_exits_successfully_within_the_grace_period() {
    let root = tempfile::tempdir().unwrap();
    let browser = Browser::launch(
        &installed_edge().unwrap(),
        &LaunchOptions::new(root.path().to_owned()),
        Duration::from_secs(20),
    )
    .await
    .unwrap();
    let (cdp, _) = Cdp::connect(&browser.endpoint().ws_url()).await.unwrap();
    cdp.call(None, "Browser.close", json!({})).await.unwrap();
    assert!(browser.wait_or_kill(Duration::from_secs(10)).await.unwrap().success());
}
