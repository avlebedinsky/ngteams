//! The real broker/app supervisor with a synthetic native executable, never a live client.
use super::*;
use std::time::Instant;

fn fixture(dir: &Path) -> PathBuf {
    let source = dir.join("fixture.rs");
    std::fs::write(&source, include_str!("support/app.rs")).unwrap();
    let app = dir.join("synthetic-app.exe");
    assert!(
        std::process::Command::new("rustc")
            .arg(&source)
            .arg("-o")
            .arg(&app)
            .status()
            .unwrap()
            .success()
    );
    app
}

#[test]
fn deadlines_stop_requests_restarts_and_failures_clean_the_entire_tree() {
    let dir = tempfile::tempdir().unwrap();
    let app = fixture(dir.path());
    for alias in ["normal", "restart", "broker-exits", "app-exits"] {
        let run = dir.path().join(alias);
        private::create_dir_all(&run).unwrap();
        let start = Instant::now();
        let result = session(&app, &run, Some(alias), None, Duration::from_millis(200));
        match alias {
            "broker-exits" => assert!(result.is_err()),
            "app-exits" => assert_eq!(result.unwrap(), 7),
            _ => assert_eq!(result.unwrap(), 124),
        }
        assert!(start.elapsed() < Duration::from_secs(10));
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = supervisor::command(&app, &["--worker".into()], Duration::ZERO);
        assert!(status.is_err());
        assert!(Instant::now() < deadline);
    }
    let stopped = dir.path().join("stopped");
    private::create_dir_all(&stopped).unwrap();
    std::fs::write(stopped.join("stop"), "").unwrap();
    assert_eq!(
        session(&app, &stopped, Some("normal"), None, Duration::from_secs(5)).unwrap(),
        0
    );
    assert!(session(&app, &stopped, None, None, Duration::ZERO).is_err());
    assert!(
        session(
            &dir.path().join("missing.exe"),
            &stopped,
            Some("a"),
            None,
            Duration::ZERO
        )
        .is_err()
    );
}

#[test]
fn cli_separates_build_and_modes_and_rejects_escaping_aliases() {
    for value in ["", "..", "a/b", "a\\b", "a b", "a\n"] {
        assert!(alias(value).is_err());
    }
    assert_eq!(alias("b_2-A").unwrap(), "b_2-A");
    assert!(Cli::try_parse_from(["runner", "run", "--app", "C:/app.exe", "--directory", "C:/run"]).is_err());
    assert!(Cli::try_parse_from(["runner", "build", "--cargo", "C:/cargo.exe", "--timeout", "0"]).is_err());
}

#[test]
fn preparation_and_personal_snapshot_cleanup_use_only_the_synthetic_fixture() {
    let dir = tempfile::tempdir().unwrap();
    let app = fixture(dir.path());
    let run = dir.path().join("private");
    assert_eq!(execute(Action::Prepare { directory: run.clone() }).unwrap(), 0);
    assert!(
        execute(Action::Prepare {
            directory: "relative".into()
        })
        .is_err()
    );
    assert!(execute(Action::Prepare { directory: app.clone() }).is_err());
    // No real personal client or credentials: the fixture just creates a file named "synthetic" in its snapshot.
    for _ in 0..2 {
        private::create_dir_all(&run.join("snapshot")).unwrap();
        std::fs::write(run.join("stop"), "stale stop").unwrap();
        assert_eq!(
            supervise(
                &app,
                &run,
                None,
                Some("synthetic@example.test"),
                Duration::from_millis(200)
            )
            .unwrap(),
            124
        );
        assert!(!run.join("snapshot").exists());
        assert!(!run.join("stop").exists());
    }
    assert_eq!(
        supervise(&app, &run, Some("app-exits"), None, Duration::from_secs(5)).unwrap(),
        7
    );
    let held = private::share_lock(&run.join("holder.lock"), false).unwrap();
    assert!(supervise(&app, &run, Some("a"), None, Duration::ZERO).is_err());
    drop(held);
    assert!(
        supervise(
            &dir.path().join("missing.exe"),
            &run,
            None,
            Some("synthetic@example.test"),
            Duration::ZERO
        )
        .is_err()
    );
    assert!(
        execute(Action::Run {
            app,
            directory: run,
            alias: Some("a".into()),
            account: None,
            timeout: 1
        })
        .is_err()
    );
    assert!(
        execute(Action::Build {
            cargo: "missing.exe".into(),
            timeout: 1
        })
        .is_err()
    );
    assert!(handle(&Transfer::new(&[]).unwrap(), 0).is_err());
}

#[test]
fn console_dispatch_requires_the_guard_directory_and_preserves_app_failures() {
    let dir = tempfile::tempdir().unwrap();
    let app = fixture(dir.path());
    let action = || Action::Run {
        app: app.clone(),
        directory: dir.path().to_owned(),
        alias: Some("app-exits".into()),
        account: None,
        timeout: 5,
    };
    assert!(execute_checked(action(), || Ok(None)).is_err());
    assert!(execute_checked(action(), || Ok(Some("another-directory".into()))).is_err());
    assert!(execute_checked(action(), || Err(io::Error::other("synthetic console lookup failure"))).is_err());
    assert_eq!(
        execute_checked(action(), || Ok(Some(dir.path().as_os_str().to_owned()))).unwrap(),
        7
    );
}

#[test]
fn failed_snapshots_are_removed_and_filesystem_obstructions_are_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let app = fixture(dir.path());
    let failing = dir.path().join("snapshot-fails.exe");
    std::fs::copy(&app, &failing).unwrap();
    let run = dir.path().join("run");
    private::create_dir_all(&run).unwrap();
    assert!(
        supervise(
            &failing,
            &run,
            None,
            Some("synthetic@example.test"),
            Duration::from_secs(5)
        )
        .is_err()
    );
    assert!(!run.join("snapshot").exists());
    std::fs::write(run.join("snapshot"), "existing obstruction").unwrap();
    assert!(supervise(&app, &run, None, Some("synthetic@example.test"), Duration::ZERO).is_err());
    assert_eq!(
        std::fs::read_to_string(run.join("snapshot")).unwrap(),
        "existing obstruction"
    );
    std::fs::create_dir(run.join("stop")).unwrap();
    assert!(supervise(&app, &run, Some("a"), None, Duration::ZERO).is_err());
    assert!(run.join("stop").is_dir());
    std::fs::remove_dir(run.join("stop")).unwrap();
    std::fs::create_dir(run.join("trace.jsonl")).unwrap();
    assert!(session(&app, &run, Some("a"), None, Duration::from_secs(1)).is_err());
    assert!(run.join("trace.jsonl").is_dir());
}

#[test]
fn lab_build_and_profile_policy_refuse_ambiguous_environment_values() {
    use std::ffi::OsStr;
    assert!(lab_build(Some(OsStr::new("1")), None));
    for lab in [
        None,
        Some(OsStr::new("")),
        Some(OsStr::new("0")),
        Some(OsStr::new("true")),
    ] {
        assert!(!lab_build(lab, None));
    }
    for installed in ["", "1"] {
        assert!(!lab_build(Some(OsStr::new("1")), Some(OsStr::new(installed))));
    }
    for path in [None, Some("".into()), Some("relative".into())] {
        assert!(home_at(path).is_err());
    }
    assert_eq!(
        home_at(Some(r"C:\synthetic-profile".into())).unwrap(),
        Path::new(r"C:\synthetic-profile")
    );
}
