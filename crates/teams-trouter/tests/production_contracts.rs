//! Production-only construction contracts. The nested project uses normal dependencies, so this crate's
//! test-support dev-dependencies cannot make mock origins or a fake registrar available to these callers.

#![cfg(test)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn project(root: &Path, dir: &Path) {
    let proto = root.join("crates/teams-proto");
    let trouter = root.join("crates/teams-trouter");
    let manifest = format!(
        "[package]\nname = \"production-contracts\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\
         [workspace]\n\
         [dependencies]\nteams-proto = {{ path = {proto:?} }}\nteams-trouter = {{ path = {trouter:?} }}\n"
    );
    fs::write(dir.join("Cargo.toml"), manifest).unwrap();
    // Reuse the reviewed workspace versions, including versions that may since have been yanked.
    fs::copy(root.join("Cargo.lock"), dir.join("Cargo.lock")).unwrap();
    fs::create_dir_all(dir.join("src/bin")).unwrap();
    // A passing baseline distinguishes a broken dependency build from the intended compile failures and
    // proves that dev-dependency feature unification has not enabled the transport's test support.
    fs::write(
        dir.join("src/lib.rs"),
        "const _: () = assert!(!teams_proto::test_support_enabled());\n",
    )
    .unwrap();
    for name in [
        "proto_mock_origin",
        "proto_register_fake_transport",
        "trouter_mock_origin",
        "trouter_fake_transport",
    ] {
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/production_contracts/{name}.rs")),
            dir.join(format!("src/bin/{name}.rs")),
        )
        .unwrap();
    }
}

fn check(dir: &Path, cache: &Path, target: &str) -> Output {
    let mut command = Command::new("timeout");
    command
        .args(["300", env!("CARGO"), "check", "--offline", "-j", "4", "--manifest-path"])
        .arg(dir.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(cache)
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("LLVM_PROFILE_FILE");
    if target == "lib" {
        command.arg("--lib");
    } else {
        command.args(["--bin", target]);
    }
    command.output().unwrap()
}

#[test]
fn normal_dependencies_keep_mock_origins_and_fake_registrars_unavailable() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let builds = root.join("target/production-contracts");
    fs::create_dir_all(&builds).unwrap();
    let dir = tempfile::tempdir_in(&builds).unwrap();
    project(root, dir.path());
    // Never share the outer Cargo process's locked target directory.
    let cache: PathBuf = builds.join("cache");
    let baseline = check(dir.path(), &cache, "lib");
    assert!(
        baseline.status.success(),
        "{}",
        String::from_utf8_lossy(&baseline.stderr)
    );
    for (name, contract) in [
        ("proto_mock_origin", "with_mock_origin"),
        ("trouter_mock_origin", "with_mock_origin"),
        ("proto_register_fake_transport", "EndpointRegistrar"),
        ("trouter_fake_transport", "EndpointRegistrar"),
    ] {
        let output = check(dir.path(), &cache, name);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{name} unexpectedly compiled");
        assert!(
            stderr.contains("error[E") && stderr.contains(contract),
            "{name}: {stderr}"
        );
    }
}
