//! Real native subprocesses, synthetic files only: handle whitelisting, exact argument transfer and bounded waits.
#![cfg(all(test, windows))]
use std::ffi::OsString;
use std::fs::File;
use std::io::Write as _;
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::windows::io::{AsHandle as _, AsRawHandle as _, FromRawHandle as _};
use std::path::Path;
use std::time::Duration;
use teams_platform::windows_process::Transfer;
use windows_sys::Win32::Foundation::{GetHandleInformation, HANDLE_FLAG_INHERIT, SetHandleInformation};

const MARKER: &str = "NGTEAMS_PROCESS_SYNTHETIC";
fn arguments(mode: &str) -> Vec<OsString> {
    [
        "--exact",
        "transfer_child",
        "--nocapture",
        "--test-threads=1",
        "--",
        MARKER,
        mode,
    ]
    .map(OsString::from)
    .to_vec()
}

#[test]
fn only_explicit_handles_and_uninterpreted_arguments_reach_the_child() {
    let dir = tempfile::tempdir().unwrap();
    let trace = dir.path().join("trace");
    let file = File::create(&trace).unwrap();
    let unrelated_path = dir.path().join("unrelated");
    let unrelated = File::create(&unrelated_path).unwrap();
    // SAFETY: this live synthetic file intentionally stays inheritable, to test the launcher's whitelist.
    let updated = unsafe { SetHandleInformation(unrelated.as_raw_handle(), HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
    assert_ne!(updated, 0);
    let transfer = Transfer::new(&[file.as_handle()]).unwrap();
    let passed = transfer.value(0).unwrap();
    assert!(transfer.value(1).is_none());
    let mut args = arguments("write");
    args.extend([
        passed.to_string().into(),
        (unrelated.as_raw_handle() as usize).to_string().into(),
        unrelated_path.canonicalize().unwrap().into_os_string(),
    ]);
    let payload: Vec<OsString> = vec![
        "".into(),
        "hello world".into(),
        r#"a\"b\\"#.into(),
        "quotes\"\"".into(),
        r"ends in slash\".into(),
        "日本語 🎉".into(),
        "%PATH% $env:PATH $(bad) `bad` & | >".into(),
    ];
    args.extend(payload.clone());
    let child = transfer.spawn(&std::env::current_exe().unwrap(), &args).unwrap();
    assert_ne!(child.id(), 0);
    drop(transfer);
    assert_eq!(child.wait(Duration::from_secs(20)).unwrap(), Some(0));
    child.kill().unwrap(); // Already exited is also successful.
    let actual = std::fs::read(&trace).unwrap();
    let expected: Vec<u8> = payload
        .iter()
        .flat_map(|part| part.encode_wide().chain([0]).flat_map(u16::to_le_bytes))
        .collect();
    assert_eq!(actual, expected);
    let mut flags = 0;
    // SAFETY: file is still owned in this process, and flags is a live output.
    let valid = unsafe { GetHandleInformation(file.as_raw_handle(), &raw mut flags) };
    assert_ne!(valid, 0);
    assert_eq!(flags & HANDLE_FLAG_INHERIT, 0);
}

#[test]
fn waits_are_bounded_and_kill_targets_the_owned_process() {
    let transfer = Transfer::new(&[]).unwrap();
    let child = transfer
        .spawn(&std::env::current_exe().unwrap(), &arguments("sleep"))
        .unwrap();
    assert_eq!(child.wait(Duration::ZERO).unwrap(), None);
    child.kill().unwrap();
    assert_eq!(child.wait(Duration::from_secs(10)).unwrap(), Some(1));
}

#[test]
fn invalid_launches_fail_before_creating_a_child() {
    let transfer = Transfer::new(&[]).unwrap();
    for program in [
        Path::new("relative.exe"),
        Path::new(r"C:\script.cmd"),
        Path::new(r"C:\script.ps1"),
    ] {
        assert_eq!(
            transfer.spawn(program, &[]).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
    }
    assert!(
        transfer
            .spawn(&std::env::current_exe().unwrap(), &[OsString::from_wide(&[0])])
            .is_err()
    );
    assert!(
        transfer
            .spawn(&std::env::current_exe().unwrap(), &["a".repeat(32768).into()])
            .is_err()
    );
    assert_eq!(
        transfer
            .spawn(Path::new(r"C:\ngteams-synthetic-missing.exe"), &[])
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::NotFound
    );
}

#[test]
fn transfer_child() {
    let args: Vec<OsString> = std::env::args_os().collect();
    let Some(start) = args.iter().position(|value| value == MARKER) else {
        return;
    };
    if args[start + 1] == "sleep" {
        std::thread::sleep(Duration::from_secs(30));
        return;
    }
    let passed = args[start + 2].to_str().unwrap().parse::<usize>().unwrap();
    let unrelated = args[start + 3].to_str().unwrap().parse::<usize>().unwrap();
    let mut name = [0_u16; 4096];
    // SAFETY: a numeric handle is only queried; invalid/reused handles return failure or a different path. The
    // output buffer is valid. No ownership is taken and no potentially reused object is changed or closed.
    let length = unsafe {
        windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW(unrelated as _, name.as_mut_ptr(), 4096, 0)
    };
    assert_ne!(
        OsString::from_wide(&name[..usize::try_from(length).unwrap().min(4096)]),
        args[start + 4]
    );
    // SAFETY: the test runner transferred this fresh file handle once, solely for this helper to own.
    let mut file = unsafe { File::from_raw_handle(passed as _) };
    for part in &args[start + 5..] {
        for unit in part.encode_wide().chain([0]) {
            file.write_all(&unit.to_le_bytes()).unwrap();
        }
    }
}
