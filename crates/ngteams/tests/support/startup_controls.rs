//! Private command entry points, with only disposable files and socket pairs.

use std::io::Write as _;
use std::os::fd::IntoRawFd as _;
use std::os::unix::net::UnixStream;
use std::process::ExitCode;

use ngteams::cli::ReadOnlyMode;
use ngteams::setup::Setup;
use teams_auth::secrets::MemorySecretStore;

use super::super::{Outcome, broker, run_read_only, take_snapshot};

pub(super) fn run(case: &str, setup: &Setup) {
    match case {
        "command-broker" => broker_boundaries(setup),
        "command-invalid-account" => {
            let cli =
                <ngteams::cli::Cli as clap::Parser>::try_parse_from(["ngteams", "--account", "invalid-upn"]).unwrap();
            assert!(super::run(&cli, teams_media::devices::pipewire::Reach::Sandbox).is_err());
        }
        "command-error-files" => corrupt_files(setup),
        "command-no-ready" => {
            super::signed(setup, "unready@example.test", 12, true);
            assert!(matches!(
                super::run(
                    &<ngteams::cli::Cli as clap::Parser>::try_parse_from(["ngteams"]).unwrap(),
                    teams_media::devices::pipewire::Reach::Sandbox
                ),
                Ok(Outcome::Quit)
            ));
        }
        "command-read-engine-error" | "command-read-bad-accounts" | "command-read-window-error" => {
            read_errors(case, setup);
        }
        "command-snapshot" => snapshot_boundaries(setup),
        "command-read-refusal" => read_refusal(setup),
        "command-read-missing" => read_missing(setup),
        "command-read-window" => read_window(setup),
        _ => panic!("unknown synthetic command case {case}"),
    }
}

fn broker_boundaries(setup: &Setup) {
    assert_eq!(broker("missing@example.test", -1), ExitCode::FAILURE);
    let (peer, socket) = UnixStream::pair().unwrap();
    drop(peer);
    assert_eq!(broker("missing@example.test", socket.into_raw_fd()), ExitCode::FAILURE);
    super::signed(setup, "signed@example.test", 8, true);
    let (peer, socket) = UnixStream::pair().unwrap();
    drop(peer);
    // EOF is handled before a lazy keyring lookup; no credential request is sent.
    assert_eq!(broker("SIGNED@example.test", socket.into_raw_fd()), ExitCode::SUCCESS);
    let (mut peer, socket) = UnixStream::pair().unwrap();
    peer.write_all(&u32::MAX.to_be_bytes()).unwrap();
    drop(peer);
    assert_eq!(broker("signed@example.test", socket.into_raw_fd()), ExitCode::FAILURE);
}

fn snapshot_boundaries(setup: &Setup) {
    let account = super::signed(setup, "snapshot@example.test", 9, true);
    let root = tempfile::tempdir().unwrap();
    let snapshot = root.path().join("snapshot");
    assert_eq!(take_snapshot(&snapshot), ExitCode::SUCCESS);
    let paths = ngteams::snapshot::layout(&snapshot);
    assert_eq!(
        std::fs::read(paths.accounts_file()).unwrap(),
        std::fs::read(setup.paths.accounts_file()).unwrap()
    );
    assert!(paths.region_file(&account).unwrap().is_file());
    assert_eq!(
        take_snapshot(&snapshot),
        ExitCode::FAILURE,
        "existing files must survive"
    );
    assert_eq!(take_snapshot(setup.paths.config_dir()), ExitCode::FAILURE);
}

fn mode(_setup: &Setup, socket: UnixStream) -> ReadOnlyMode {
    ReadOnlyMode {
        account: "signed@example.test".to_owned(),
        data: std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join("missing-snapshot"),
        secrets_fd: socket.into_raw_fd(),
        trace_fd: None,
    }
}

fn read_refusal(setup: &Setup) {
    let (peer, socket) = UnixStream::pair().unwrap();
    let mut request = mode(setup, socket);
    request.trace_fd = Some(-1);
    assert!(run_read_only(&request).err().unwrap().contains("descriptor"));
    let (_, socket) = UnixStream::pair().unwrap();
    request = mode(setup, socket);
    assert!(run_read_only(&request).err().unwrap().contains("compositor"));
    drop(peer);
}

fn read_missing(setup: &Setup) {
    let (peer, socket) = UnixStream::pair().unwrap();
    let mut request = mode(setup, socket);
    assert!(run_read_only(&request).err().unwrap().contains("holds no snapshot"));
    let root = tempfile::tempdir().unwrap();
    setup.ask_for("pending@example.test").unwrap();
    ngteams::snapshot::snapshot(&setup.paths, root.path()).unwrap();
    let (_, socket) = UnixStream::pair().unwrap();
    request = mode(setup, socket);
    request.data = root.path().to_owned();
    assert!(
        run_read_only(&request)
            .err()
            .unwrap()
            .contains("isn't a signed-in account")
    );
    drop(peer);
}

fn read_window(setup: &Setup) {
    super::signed(setup, "signed@example.test", 10, true);
    let root = tempfile::tempdir().unwrap();
    let paths = ngteams::snapshot::snapshot(&setup.paths, root.path()).unwrap();
    let (peer, socket) = UnixStream::pair().unwrap();
    let serving = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime
            .block_on(ngteams::read_only::serve_broker(
                peer,
                &ngteams::read_only::broker_scope("signed@example.test"),
                &MemorySecretStore::new(),
                &mut Vec::new(),
            ))
            .unwrap();
    });
    let mut request = mode(setup, socket);
    request.account = "SIGNED@example.test".to_owned();
    request.data = root.path().to_owned();
    let trace = std::fs::File::create(root.path().join("trace.jsonl")).unwrap();
    request.trace_fd = Some(trace.into_raw_fd());
    assert!(matches!(run_read_only(&request), Ok(Outcome::Quit)));
    serving.join().unwrap();
    let account = ngteams::accounts_file::PersonalAccount::find(&paths, "signed@example.test")
        .unwrap()
        .unwrap();
    assert!(paths.store_file(&account).unwrap().is_file());
    assert_eq!(
        std::fs::read(setup.paths.accounts_file()).unwrap(),
        std::fs::read(paths.accounts_file()).unwrap()
    );
    // Starting again proves the window quit joined and released its engine.

    ngteams::engine::start_read_only(&paths, &account, teams_core::CallFloor::new(), MemorySecretStore::new())
        .unwrap()
        .handle
        .shutdown();
}

fn corrupt_files(setup: &Setup) {
    let runner = super::accounts::Runner {
        setup: setup.clone(),
        floor: teams_core::CallFloor::new(),
        devices: None,
    };
    assert!(
        super::super::start_accounts(&runner, &[])
            .unwrap_err()
            .contains("no account is signed in")
    );
    setup.ask_for("pending@example.test").unwrap();
    std::fs::write(setup.paths.accounts_file(), b"invalid accounts").unwrap();
    assert!(super::super::ready_accounts(setup).is_err());
    assert!(super::super::ask_account(setup.clone(), None).is_err());
    let cli = <ngteams::cli::Cli as clap::Parser>::try_parse_from(["ngteams"]).unwrap();
    assert!(super::run(&cli, teams_media::devices::pipewire::Reach::Sandbox).is_err());
    let (peer, socket) = UnixStream::pair().unwrap();
    assert_eq!(broker("signed@example.test", socket.into_raw_fd()), ExitCode::FAILURE);
    drop(peer);
}

fn read_errors(case: &str, setup: &Setup) {
    let account = super::signed(setup, "signed@example.test", 13, true);
    let root = tempfile::tempdir().unwrap();
    let dest = root.path().to_owned();
    let paths = ngteams::snapshot::snapshot(&setup.paths, &dest).unwrap();
    if case == "command-read-bad-accounts" {
        std::fs::write(paths.accounts_file(), b"invalid accounts").unwrap();
    } else if case == "command-read-engine-error" {
        std::fs::write(paths.region_file(&account).unwrap(), b"invalid region").unwrap();
    }
    let (peer, socket) = UnixStream::pair().unwrap();
    // An empty credential source: even the window-failure branch has no user credential or reachable token.
    drop(peer);
    let mut request = mode(setup, socket);
    request.data = dest;
    let error = run_read_only(&request).err().unwrap();
    let expected = if case.ends_with("window-error") {
        "window couldn't be opened"
    } else if case.ends_with("engine-error") {
        "region"
    } else {
        "accounts"
    };
    assert!(error.contains(expected), "{case}: {error}");
}

pub(super) fn invalid_environment() {
    let cli = <ngteams::cli::Cli as clap::Parser>::try_parse_from(["ngteams"]).unwrap();
    assert!(super::run(&cli, teams_media::devices::pipewire::Reach::Sandbox).is_err());
    assert_eq!(
        take_snapshot(std::path::Path::new("/synthetic/not-created")),
        ExitCode::FAILURE
    );
    let (peer, socket) = UnixStream::pair().unwrap();
    assert_eq!(broker("signed@example.test", socket.into_raw_fd()), ExitCode::FAILURE);
    drop(peer);
    let (peer, socket) = UnixStream::pair().unwrap();
    let request = ReadOnlyMode {
        account: "signed@example.test".to_owned(),
        data: "/synthetic/not-created".into(),
        secrets_fd: socket.into_raw_fd(),
        trace_fd: None,
    };
    let error = run_read_only(&request).err().unwrap();
    assert!(error.contains("HOME"), "{error}");
    drop(peer);
}
