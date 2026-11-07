//! A `proto-lab` binary that cargo built with the test-only `test-support` feature (as `cargo test` builds it) refuses
//! to run any command: its probe origins could reach a mock.

use std::process::Command;

#[test]
fn a_test_support_build_refuses_every_command() -> Result<(), Box<dyn std::error::Error>> {
    assert!(proto_lab::test_support_enabled());
    let home = tempfile::tempdir()?;
    let output = Command::new(env!("CARGO_BIN_EXE_proto-lab"))
        .args(["chrome", "path"])
        .env_clear()
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .envs(
            ["PATH", "LLVM_PROFILE_FILE"]
                .into_iter()
                .filter_map(|key| std::env::var_os(key).map(|value| (key, value))),
        )
        .output()?;
    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "nothing ran: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("built with proto-lab's test-support feature"),
        "{stderr}"
    );
    assert!(!home.path().join("data").exists(), "no lab directory was made");
    Ok(())
}
