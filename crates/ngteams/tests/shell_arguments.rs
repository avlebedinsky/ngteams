//! Offline shell startup parsing; no GUI, account, credential broker or transport.
use clap::Parser as _;
use ngteams::cli::{Cli, shell_arguments};

#[test]
fn com_startup_marker_keeps_application_and_runner_arguments() {
    let args = ["ngteams", "--lab", "synthetic", "-Embedding"]
        .into_iter()
        .map(std::ffi::OsString::from);
    let filtered: Vec<_> = shell_arguments(args).collect();
    if cfg!(windows) {
        let cli = Cli::try_parse_from(filtered).unwrap();
        assert_eq!(cli.lab.as_deref(), Some("synthetic"));
        assert!(
            cli.lab_mode().is_err(),
            "COM cannot supply a runner's broker descriptors"
        );
    } else {
        assert_eq!(
            filtered.last().unwrap(),
            "-Embedding",
            "other platforms keep unknown arguments for Clap to reject"
        );
    }
}
