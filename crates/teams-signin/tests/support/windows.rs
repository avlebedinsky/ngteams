//! The foreground window service with synthetic requests that fail before any page engine starts.

use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use gpui::TestAppContext;
use teams_auth::signin::{SignInAnswer, SignInRequest};

use super::{Bound, WindowConfig, Windows};

fn invalid_request() -> WindowConfig {
    WindowConfig {
        request: SignInRequest {
            authorize: "http://127.0.0.1/synthetic".to_owned(),
            redirect_uri: "not-a-redirect-uri".to_owned(),
        },
        extension: PathBuf::from("/synthetic/no-extension"),
        bound: Bound::Loopback,
        setup: Box::new(|guard| guard),
    }
}

#[test]
fn a_service_that_has_ended_answers_failure_without_waiting() {
    let (asks, waiting) = async_channel::unbounded();
    drop(waiting);
    let service = Windows { asks };
    assert!(matches!(
        service.ask(invalid_request()),
        SignInAnswer::Failed(message) if message.contains("no window")
    ));
}

#[test]
fn an_abandoned_ask_is_reported_as_failure() {
    let (asks, waiting) = async_channel::unbounded();
    let service = Windows { asks };
    let peer = thread::spawn(move || {
        let (config, reply) = waiting.recv_blocking().unwrap();
        assert_eq!(config.request.authorize, "http://127.0.0.1/synthetic");
        drop(reply);
    });
    assert!(matches!(
        service.ask(invalid_request()),
        SignInAnswer::Failed(message) if message.contains("without an answer")
    ));
    peer.join().unwrap();
}

#[test]
fn the_running_service_returns_setup_failures_to_each_cloned_handle() {
    let cx = TestAppContext::single();
    let service = cx.update(|cx| Windows::serve("synthetic-signin", cx));
    for handle in [service.clone(), service] {
        let answer = thread::spawn(move || handle.ask(invalid_request()));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !answer.is_finished() {
            cx.run_until_parked();
            assert!(
                Instant::now() < deadline,
                "window service did not answer an invalid request"
            );
            thread::sleep(Duration::from_millis(1));
        }
        assert!(matches!(
            answer.join().unwrap(),
            SignInAnswer::Failed(message) if message.contains("redirect URI")
        ));
    }
    cx.run_until_parked();
}

#[test]
fn extension_installation_is_private_selective_and_fail_closed() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = super::private_dir().unwrap();
    assert_eq!(
        std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let source = tempfile::tempdir().unwrap();
    let module = source.path().join("synthetic-extension.so");
    assert!(
        super::install_extension(&module, dir.path())
            .unwrap_err()
            .contains("can't be read")
    );
    std::fs::write(&module, b"_synthetic invalid module").unwrap();
    assert!(
        super::install_extension(&module, dir.path())
            .unwrap_err()
            .contains("isn't the sign-in")
    );
    let bytes = format!("_synthetic not executable: {}", super::ENTRY_POINT);
    std::fs::write(&module, &bytes).unwrap();
    std::fs::write(source.path().join("unrelated.so"), b"_synthetic unrelated").unwrap();
    let installed = super::install_extension(&module, dir.path()).unwrap();
    assert_eq!(
        std::fs::read(installed.join(super::EXTENSION_FILE)).unwrap(),
        bytes.as_bytes()
    );
    assert_eq!(std::fs::read_dir(&installed).unwrap().count(), 1);
    assert!(
        super::install_extension(&module, dir.path())
            .unwrap_err()
            .contains("no extensions directory")
    );
    let obstruction = source.path().join("not-a-directory");
    std::fs::write(&obstruction, b"_synthetic obstruction").unwrap();
    assert!(
        super::install_extension(&module, &obstruction)
            .unwrap_err()
            .contains("no extensions directory")
    );
}
