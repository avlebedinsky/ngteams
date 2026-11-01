//! Only invalid redirect requests reach the mock foreground service; it fails before starting a page engine.

use std::time::{Duration, Instant};

use gpui::TestAppContext;

use super::*;

fn request(authorize: &str) -> WindowRequest {
    WindowRequest {
        authorize: authorize.to_owned(),
        redirect_uri: "invalid-redirect".to_owned(),
    }
}

fn window(setup: &Setup, windows: &Windows) -> Window {
    bound_window(
        setup,
        Arc::new(SignInHosts::entra_with(&[]).unwrap()),
        windows,
        "/synthetic/no-module.so".into(),
    )
}

#[test]
fn bound_windows_refuse_foreign_pages_and_report_data_or_log_failures_before_the_service() {
    let root = tempfile::tempdir().unwrap();
    let (setup, _) = crate::app::tests::setup(root.path(), 1);
    let cx = TestAppContext::single();
    let windows = cx.update(super::windows);
    let again = cx.update(super::windows);
    let refused = window(&setup, &windows)(request("https://example.test/authorize"));
    assert!(matches!(refused, WindowAnswer::Failed(reason) if reason.contains("isn't on the sign-in window's hosts")));
    assert!(!setup.paths.fence_log().exists());
    let mut broken = setup.clone();
    let file = root.path().join("not-a-directory");
    std::fs::write(&file, b"_synthetic obstruction").unwrap();
    broken.paths = teams_fence::personal::PersonalPaths::new(file.clone(), file);
    let refused = window(&broken, &again)(request("https://login.microsoftonline.com/authorize"));
    assert!(matches!(refused, WindowAnswer::Failed(reason) if reason.contains("personal data dir")));
    setup.paths.make_dirs().unwrap();
    std::fs::create_dir(setup.paths.fence_log()).unwrap();
    let refused = window(&setup, &windows)(request("https://login.microsoftonline.com/authorize"));
    assert!(matches!(refused, WindowAnswer::Failed(reason) if reason.contains("fence.log can't be opened")));
    assert_eq!(cx.update(|cx| cx.windows().len()), 0);
}

#[test]
fn a_bound_request_is_answered_by_the_service_without_loading_an_invalid_redirect() {
    let root = tempfile::tempdir().unwrap();
    let (setup, _) = crate::app::tests::setup(root.path(), 2);
    let cx = TestAppContext::single();
    let windows = cx.update(super::windows);
    let ask = window(&setup, &windows);
    let replying = std::thread::spawn(move || ask(request("https://login.microsoftonline.com/authorize")));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !replying.is_finished() {
        cx.run_until_parked();
        assert!(Instant::now() < deadline, "foreground service did not answer");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(matches!(replying.join().unwrap(), WindowAnswer::Failed(reason) if reason.contains("redirect URI")));
    assert!(setup.paths.fence_log().is_file());
    assert_eq!(cx.update(|cx| cx.windows().len()), 0);
}
