//! Real browser lifecycle in a private subprocess, fresh synthetic lab config and blank pages only.
use super::*;
use std::os::unix::fs::{PermissionsExt as _, symlink};

const CHILD: &str = "PROTO_LAB_SYNTHETIC_SESSION_STARTUP";

fn installed_chrome() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("PROTO_LAB_CHROME") {
        return Some(path.into());
    }
    let paths = crate::config::LabLocations::from_env()
        .and_then(|locations| locations.lab_paths())
        .ok()?;
    let chrome = ChromeBuild::pinned().executable(&paths.chrome_dir());
    chrome.is_file().then_some(chrome)
}

fn private_environment(command: &mut tokio::process::Command, root: &Path) {
    command.env_clear();
    for name in ["PATH", "LLVM_PROFILE_FILE"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    for (name, subdir) in [
        ("HOME", "home"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_RUNTIME_DIR", "runtime"),
    ] {
        let directory = root.join(subdir);
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        command.env(name, directory);
    }
    command.env(CHILD, "1");
    command.env(
        "DBUS_SESSION_BUS_ADDRESS",
        format!("unix:path={}", root.join("no-session-bus").display()),
    );
}

fn prepare(root: &Path, chrome: &Path) {
    let config_file = root.join("config/teams-lab/lab.toml");
    std::fs::create_dir(config_file.parent().unwrap()).unwrap();
    let config=["a","b","c"].map(|alias|format!("[[accounts]]\nalias='{alias}'\nupn='synthetic-{alias}@example.test'\nmri='8:orgid:00000000-0000-4000-8000-00000000000{alias}'\n")).join("\n");
    std::fs::write(config_file, config).unwrap();
    let paths = LabPaths::new(root.join("data/teams-lab"));
    let executable = ChromeBuild::pinned().executable(&paths.chrome_dir());
    std::fs::create_dir_all(executable.parent().unwrap().parent().unwrap()).unwrap();
    symlink(chrome.parent().unwrap(), executable.parent().unwrap()).unwrap();
}

async fn startup_matrix() {
    let locations = crate::config::LabLocations::from_env().unwrap();
    let paths = locations.lab_paths().unwrap();
    assert!(paths.data_dir().starts_with(std::env::var_os("XDG_DATA_HOME").unwrap()));
    let mut session = LabSession::open(&paths, Alias::A).await.unwrap();
    assert_eq!(session.watch().state(), SessionState::Loading);
    assert!(session.capture_dir().is_none());
    queued_capture_messages_stay_local(&mut session).await;
    let (report, _) = close_after_arming(session, &paths).await;
    assert_clean(&report);
    for request_bodies in [false, true] {
        blank_capture(&paths, request_bodies).await;
    }
    let (catch, _caught) = RedirectCatch::new(
        "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
        teams_auth::token::NATIVE_CLIENT_REDIRECT,
    )
    .unwrap();
    let session = LabSession::open_catching(&paths, Alias::C, catch).await.unwrap();
    let (report, _) = close_after_arming(session, &paths).await;
    assert_clean(&report);
    let options = SessionOptions {
        capture: Some("INVALID".into()),
        ..SessionOptions::default()
    };
    assert!(matches!(
        LabSession::open_with(&paths, Alias::C, options).await,
        Err(SessionError::Capture(_))
    ));
}

#[tokio::test]
async fn session_startup_arms_only_fresh_blank_profiles_and_closes_every_owned_browser() {
    if std::env::var_os(CHILD).is_some() {
        tokio::time::timeout(Duration::from_secs(120), startup_matrix())
            .await
            .unwrap();
        return;
    }
    let Some(chrome) = installed_chrome() else {
        assert!(
            std::env::var_os("PROTO_LAB_REQUIRE_CHROME").is_none(),
            "required pinned Chrome is missing"
        );
        eprintln!("skipped: no pinned Chrome for private blank-session startup");
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    private_environment(&mut command, root.path());
    prepare(root.path(), &chrome.canonicalize().unwrap());
    let output = command
        .args([
            "--exact",
            "session::startup_tests::session_startup_arms_only_fresh_blank_profiles_and_closes_every_owned_browser",
            "--nocapture",
        ])
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    if !output.status.success() {
        let retained = root.keep();
        panic!(
            "private startup failed; evidence retained at {}: {}\n{}",
            retained.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
}

async fn blank_capture(paths: &LabPaths, request_bodies: bool) {
    let options = SessionOptions {
        capture: Some("synthetic-startup".into()),
        capture_request_bodies: request_bodies,
        media: Media::Fake { audio: None },
        user_agent: Some("synthetic-test-agent".into()),
        ..SessionOptions::default()
    };
    let mut session = LabSession::open_with(paths, Alias::B, options).await.unwrap();
    let directory = session.capture_dir().unwrap().to_path_buf();
    assert!(directory.starts_with(paths.captures_dir()));
    assert_eq!(session.watch().state(), SessionState::Loading);
    queued_capture_messages_stay_local(&mut session).await;
    let (report, capture) = close_after_arming(session, paths).await;
    assert_eq!(capture, Some(directory.clone()));
    assert_clean(&report);
    assert!(directory.join(crate::capture::EVENTS_FILE).is_file());
}

fn assert_clean(report: &SessionReport) {
    assert!(!report.failed(), "session capture setup: {report:?}");
}

async fn close_after_arming(session: LabSession, paths: &LabPaths) -> (SessionReport, Option<PathBuf>) {
    let recorder = Recorder::create(&paths.captures_dir(), "startup-diagnostic").unwrap();
    let ready = tokio::time::timeout(Duration::from_secs(25), async {
        loop {
            let targets = session
                .cdp()
                .call(None, "Target.getTargets", serde_json::json!({}))
                .await
                .unwrap();
            recorder.record(serde_json::json!({"targets":targets,"undecided":session.status().pending()}));
            let armed = session.armed.targets();
            let all_armed = targets["targetInfos"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|target| target["attached"] == true)
                .all(|target| armed.iter().any(|known| target["targetId"] == known.target_id));
            assert_clean(&session.report());
            if all_armed && session.status().pending() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    recorder.finish().unwrap();
    ready.unwrap();
    session.close_recorded().await.unwrap()
}

async fn queued_capture_messages_stay_local(session: &mut LabSession) {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("synthetic blank page"))
        .mount(&server)
        .await;
    session.navigate(&server.uri()).await.unwrap();
    for marker in [crate::capture_hold::HELD_MARKER, "lab fence probe "] {
        let expression = format!(
            "fetch('/messages', {{method:'POST', body:{}}}).then(() => 'sent', () => 'held')",
            serde_json::to_string(&format!("{marker}earlier-run")).unwrap()
        );
        let answer = session
            .cdp()
            .call(
                Some(session.page()),
                "Runtime.evaluate",
                serde_json::json!({
                    "expression":expression,"returnByValue":true,"awaitPromise":true
                }),
            )
            .await
            .unwrap();
        assert_eq!(answer["result"]["value"], "held", "{answer}");
    }
    assert_eq!(session.status().held_requests().len(), 2);
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| request.method == "GET")
    );
}
