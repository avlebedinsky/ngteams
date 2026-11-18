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
