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
fn a_missing_run_directory_is_refused_before_anything_is_created_in_its_place() {
    let dir = tempfile::tempdir().unwrap();
    let absent = dir.path().join("absent");
    // Refused before any lock, stop marker, snapshot or process: the executable is never looked up.
    for (alias, account) in [(Some("a"), None), (None, Some("synthetic@example.test"))] {
        assert!(supervise(&dir.path().join("unused.exe"), &absent, alias, account, Duration::ZERO).is_err());
        assert!(!absent.exists());
    }
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

/// A directory junction that resolves to itself: it exists, but nothing can be resolved through it.
fn junction_loop(path: &Path) {
    let status = std::process::Command::new("cmd")
        .args([
            "/C".as_ref(),
            "mklink".as_ref(),
            "/J".as_ref(),
            path.as_os_str(),
            path.as_os_str(),
        ])
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
}

/// Rust reports a junction as a symlink. The entry is still there, untouched.
fn junction_kept(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|entry| entry.file_type().is_symlink())
}

#[test]
fn unresolvable_snapshot_and_stop_entries_fail_closed_and_stay_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let app = fixture(dir.path());
    // A personal run never deletes through a snapshot entry it cannot resolve, before or after the session.
    let personal = dir.path().join("personal");
    private::create_dir_all(&personal).unwrap();
    junction_loop(&personal.join("snapshot"));
    assert!(
        supervise(
            &app,
            &personal,
            None,
            Some("synthetic@example.test"),
            Duration::from_secs(5)
        )
        .is_err()
    );
    assert!(junction_kept(&personal.join("snapshot")));
    assert!(!personal.join("trace.jsonl").exists());
    // A running session whose stop marker cannot be read ends at once with an error, not at its deadline.
    let lab = dir.path().join("lab");
    private::create_dir_all(&lab).unwrap();
    junction_loop(&lab.join("stop"));
    let start = Instant::now();
    assert!(session(&app, &lab, Some("normal"), None, Duration::from_secs(60)).is_err());
    assert!(start.elapsed() < Duration::from_secs(30));
    assert!(junction_kept(&lab.join("stop")));
}

#[test]
fn an_app_that_cannot_launch_after_its_broker_started_takes_the_broker_down() {
    use std::os::windows::ffi::OsStrExt as _;
    let dir = tempfile::tempdir().unwrap();
    let app = fixture(dir.path());
    let run = dir.path().join("run");
    private::create_dir_all(&run).unwrap();
    // The broker's command line fits Windows' 32,767-unit limit. The app's adds the handle and control-socket
    // arguments and exceeds it, so the app launch fails after the 30-second synthetic broker is already running.
    let alias = "a".repeat(32_700 - app.as_os_str().encode_wide().count());
    let start = Instant::now();
    let error = session(&app, &run, Some(&alias), None, Duration::from_secs(60)).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    // Returning means the broker's job emptied, as the runner waits for that before it releases anything; well
    // before the broker's own 30 seconds means it was terminated, not awaited. A fresh executable's first launch
    // can be slow on a loaded Windows host, so the margin is wide.
    assert!(start.elapsed() < Duration::from_secs(25));
    assert_eq!(std::fs::read_to_string(run.join("trace.jsonl")).unwrap(), "");
}

#[test]
fn dropping_a_tree_terminates_every_process_in_it() {
    let dir = tempfile::tempdir().unwrap();
    let app = fixture(dir.path());
    let tree = supervisor::Tree::new().unwrap();
    let workers: Vec<_> = (0..8)
        .map(|_| {
            Transfer::new(&[])
                .unwrap()
                .spawn_in_job(&app, &["--worker".into()], &tree.0)
                .unwrap()
        })
        .collect();
    assert_eq!(tree.0.active_processes().unwrap(), 8);
    let start = Instant::now();
    // Drop returns once the job is empty. Each 30-second worker ends with the tree's termination code 1.
    drop(tree);
    for worker in &workers {
        assert_eq!(worker.wait(Duration::from_secs(10)).unwrap(), Some(1));
    }
    assert!(start.elapsed() < Duration::from_secs(20));
}

const PROFILE_CASE: &str = "NGTEAMS_RUNNER_TEST_PROFILE_CASE";
const PROFILE_DONE: &str = "NGTEAMS_RUNNER_TEST_PROFILE_DONE";

#[test]
fn the_lab_profile_root_and_proto_labs_live_lock_are_checked_before_the_alias_lock() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("profile-file");
    std::fs::write(&file, "obstruction").unwrap();
    // LOCALAPPDATA is process-wide, so each case runs in its own copy of this test binary with a synthetic profile.
    // Nothing reads or locks the real user's lab profile.
    for (case, profile) in [
        ("relative", PathBuf::from("relative-profile")),
        ("obstructed", file.clone()),
        ("live", dir.path().join("profile")),
    ] {
        let done = dir.path().join(format!("{case}.done"));
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "windows::tests::profile_case", "--test-threads=1"])
            .env(PROFILE_CASE, case)
            .env(PROFILE_DONE, &done)
            .env("LOCALAPPDATA", &profile)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "{case}");
        assert_eq!(std::fs::read_to_string(&done).unwrap(), case);
    }
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "obstruction");
    assert!(!Path::new("relative-profile").exists());
}

/// Runs only as a child of the test above, which sets its case and synthetic LOCALAPPDATA.
#[test]
fn profile_case() {
    let Some(case) = std::env::var_os(PROFILE_CASE) else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    match case.to_str().unwrap() {
        "relative" | "obstructed" => refused_profile(dir.path()),
        "live" => live_lock(dir.path()),
        other => panic!("unknown case {other}"),
    }
    std::fs::write(std::env::var_os(PROFILE_DONE).unwrap(), case.as_encoded_bytes()).unwrap();
}

fn run_directory(dir: &Path, name: &str) -> PathBuf {
    let run = dir.join(name);
    private::create_dir_all(&run).unwrap();
    run
}

fn refused_profile(dir: &Path) {
    let lab = run_directory(dir, "lab");
    // The profile root is refused before the alias lock is taken or any executable is looked up.
    assert!(supervise(&dir.join("missing.exe"), &lab, Some("a"), None, Duration::ZERO).is_err());
    assert!(!lab.join("holder.lock").exists());
}

fn live_lock(dir: &Path) {
    let missing = dir.join("missing.exe");
    let shared = home().unwrap().join("teams-lab");
    private::create_dir_all(&shared).unwrap();
    // proto-lab holds the live lock exclusively. Lab runs wait their turn; read-only personal runs are not lab runs.
    let proto_lab = private::share_lock(&shared.join("live.lock"), false).unwrap();
    let lab = run_directory(dir, "lab");
    assert!(supervise(&missing, &lab, Some("a"), None, Duration::ZERO).is_err());
    assert!(!lab.join("holder.lock").exists());
    let personal = run_directory(dir, "personal");
    assert!(
        supervise(
            &missing,
            &personal,
            None,
            Some("synthetic@example.test"),
            Duration::ZERO
        )
        .is_err()
    );
    assert!(personal.join("holder.lock").exists());
    drop(proto_lab);
    let after = run_directory(dir, "after");
    assert!(supervise(&missing, &after, Some("a"), None, Duration::ZERO).is_err());
    assert!(after.join("holder.lock").exists());
}
