//! `ngteams --prepare`'s run: its steps in order, failures reported without a stop, and its time limit.
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::{LIMIT, Step, run, run_once, steps};

fn step(name: &'static str, log: &Arc<Mutex<Vec<&'static str>>>, result: Result<(), String>) -> Step {
    let log = Arc::clone(log);
    Step {
        name,
        work: Box::new(move || {
            log.lock().unwrap().push(name);
            result
        }),
    }
}

#[test]
fn steps_run_in_order_and_a_failure_does_not_stop_the_next() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let result = run(
        vec![
            step("first", &log, Err("refused".into())),
            step("second", &log, Ok(())),
            step("third", &log, Err("missing".into())),
        ],
        LIMIT,
    );
    assert_eq!(result, Err("first: refused; third: missing".into()));
    assert_eq!(*log.lock().unwrap(), ["first", "second", "third"]);
}

#[test]
fn a_step_that_panics_is_a_failure_and_the_next_step_runs() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let result = run(
        vec![
            Step {
                name: "panics",
                work: Box::new(|| std::panic::resume_unwind(Box::new("synthetic"))),
            },
            step("next", &log, Ok(())),
        ],
        LIMIT,
    );
    assert_eq!(result, Err("panics: it panicked".into()));
    assert_eq!(*log.lock().unwrap(), ["next"]);
}

#[test]
fn steps_that_all_pass_pass() {
    let log = Arc::new(Mutex::new(Vec::new()));
    assert_eq!(run(vec![step("only", &log, Ok(()))], LIMIT), Ok(()));
    assert_eq!(*log.lock().unwrap(), ["only"]);
}

#[test]
fn a_step_that_does_not_end_stops_the_wait_at_the_limit() {
    let started = Instant::now();
    let result = run(
        vec![Step {
            name: "stuck",
            work: Box::new(|| {
                std::thread::sleep(Duration::from_secs(30));
                Ok(())
            }),
        }],
        Duration::from_millis(200),
    );
    assert_eq!(result, Err("not done after 0 s".into()));
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
}

#[cfg(unix)]
#[test]
fn a_program_runs_once_and_its_exit_code_does_not_matter() {
    use std::os::unix::fs::PermissionsExt as _;
    let directory = tempfile::tempdir().unwrap();
    let program = directory.path().join("worker");
    std::fs::write(&program, "#!/bin/sh\nexit 3\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(run_once(&program), Ok(()));
}

#[test]
fn a_missing_program_is_a_failure_with_its_path() {
    let missing = std::env::temp_dir().join("ngteams-prepare-missing").join("worker");
    let error = run_once(&missing).unwrap_err();
    assert!(error.starts_with(&missing.display().to_string()), "{error}");
}

#[test]
fn this_platform_prepares_gstreamer_last() {
    let names: Vec<_> = steps().iter().map(|step| step.name).collect();
    let expected: &[&str] = if cfg!(windows) {
        &["the sign-in worker", "GStreamer"]
    } else {
        &["GStreamer"]
    };
    assert_eq!(names, expected);
}

#[test]
fn gstreamer_starts() {
    let gstreamer = steps().pop().unwrap();
    assert_eq!((gstreamer.work)(), Ok(()));
}

const PREPARE_CHILD: &str = "NGTEAMS_PREPARE_TEST_CHILD";

/// A run of the installer's steps in a process of its own, as setup starts `ngteams --prepare`, with `root` as its HOME
/// and XDG folders.
#[cfg(target_os = "linux")]
fn prepare_in_a_new_process(root: &std::path::Path) -> std::process::Output {
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "prepare::tests::prepare_child",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env_clear()
        .env(PREPARE_CHILD, "1")
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("XDG_STATE_HOME", root.join("state"));
    for key in ["PATH", "LANG", "LD_LIBRARY_PATH", "LLVM_PROFILE_FILE"] {
        if let Some(value) = std::env::var_os(key) {
            child.env(key, value);
        }
    }
    child.output().unwrap()
}

/// The app's files under `root`: any file in the config, data or state folders, or with `ngteams` in its path. The
/// plugin scan also leaves the caches of graphics drivers (Mesa, NVIDIA), which are not the app's.
#[cfg(target_os = "linux")]
fn app_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(&folder).unwrap().map(Result::unwrap) {
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                folders.push(path);
            } else if ["config", "data", "state"]
                .iter()
                .any(|kept| path.starts_with(root.join(kept)))
                || path.to_string_lossy().to_lowercase().contains("ngteams")
            {
                found.push(path);
            }
        }
    }
    found
}

#[test]
fn prepare_child() {
    if std::env::var_os(PREPARE_CHILD).is_none() {
        return;
    }
    assert_eq!(run(steps(), LIMIT), Ok(()));
}

/// The user's view of setup's `--prepare` run: it writes the plugin registry where the next start of the app reads it,
/// so that start finds the registry current and does not write it again (no plugin scan). It makes no file of the
/// app (no account, store or settings).
#[cfg(target_os = "linux")]
#[test]
fn the_installers_run_leaves_a_current_plugin_registry_and_no_app_files() {
    let root = tempfile::tempdir().unwrap();
    let first = prepare_in_a_new_process(root.path());
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    let registries: Vec<_> = std::fs::read_dir(root.path().join("cache/gstreamer-1.0"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert!(
        registries.len() == 1
            && registries[0]
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("registry."),
        "{registries:?}"
    );
    let written = std::fs::metadata(&registries[0]).unwrap();
    assert!(written.len() > 0);
    assert_eq!(app_files(root.path()), Vec::<std::path::PathBuf>::new());

    let next = prepare_in_a_new_process(root.path());
    assert!(next.status.success(), "{}", String::from_utf8_lossy(&next.stderr));
    let after = std::fs::metadata(&registries[0]).unwrap();
    assert_eq!(
        after.modified().unwrap(),
        written.modified().unwrap(),
        "the next start scanned the plugins again"
    );
    assert_eq!(after.len(), written.len());
}
