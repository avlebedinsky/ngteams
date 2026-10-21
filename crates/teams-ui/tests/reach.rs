//! What the app crates (`teams-ui` and `teams-app`) can reach: every request goes through `teams-core`, so neither
//! depends directly on an HTTP or WebSocket client (read from `cargo metadata`, resolved for the host); GPUI's HTTP
//! client stays the null one, so nothing in their sources hands GPUI another; and nothing in them reads a token's
//! value out of its `Secret`.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// The crates that make the app.
const APP_CRATES: [&str; 2] = ["teams-ui", "teams-app"];

/// HTTP and WebSocket clients, by crate name.
const NETWORK_CLIENTS: [&str; 16] = [
    "reqwest",
    "hyper",
    "hyper-util",
    "h2",
    "ureq",
    "isahc",
    "curl",
    "surf",
    "attohttpc",
    "minreq",
    "awc",
    "tungstenite",
    "tokio-tungstenite",
    "async-tungstenite",
    "websocket",
    "gpui-pre-reqwest",
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn host_triple() -> String {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(rustc).arg("-vV").output().expect("rustc runs");
    let text = String::from_utf8(output.stdout).expect("UTF-8");
    text.lines()
        .find_map(|line| line.strip_prefix("host: "))
        .expect("rustc names its host")
        .to_owned()
}

/// `cargo metadata` of the workspace, resolved for the host only.
fn metadata() -> Value {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .current_dir(workspace_root())
        .args([
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--offline",
            "--filter-platform",
        ])
        .arg(host_triple())
        .output()
        .expect("cargo metadata runs");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    serde_json::from_slice(&output.stdout).expect("JSON")
}

/// The names of `package`'s direct normal and build dependencies, as resolved.
fn direct_dependencies(metadata: &Value, package: &str) -> Vec<String> {
    let id = metadata["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .find(|candidate| candidate["name"] == package)
        .map_or_else(
            || panic!("{package} is a workspace member"),
            |found| found["id"].clone(),
        );
    let node = metadata["resolve"]["nodes"]
        .as_array()
        .expect("resolve")
        .iter()
        .find(|node| node["id"] == id)
        .expect("resolved");
    node["deps"]
        .as_array()
        .expect("deps")
        .iter()
        .filter(|dep| {
            dep["dep_kinds"].as_array().is_some_and(|kinds| {
                kinds
                    .iter()
                    .any(|kind| kind["kind"].is_null() || kind["kind"] == "build")
            })
        })
        .map(|dep| {
            let id = dep["pkg"].clone();
            metadata["packages"]
                .as_array()
                .expect("packages")
                .iter()
                .find(|candidate| candidate["id"] == id)
                .and_then(|found| found["name"].as_str())
                .expect("named")
                .to_owned()
        })
        .collect()
}

#[test]
fn no_app_crate_depends_directly_on_an_http_or_websocket_client() {
    let metadata = metadata();
    for package in APP_CRATES {
        let dependencies = direct_dependencies(&metadata, package);
        assert!(
            dependencies.iter().any(|name| name == "teams-core"),
            "{package}: {dependencies:?}"
        );
        let clients: Vec<&String> = dependencies
            .iter()
            .filter(|name| NETWORK_CLIENTS.contains(&name.as_str()))
            .collect();
        assert!(clients.is_empty(), "{package} depends directly on {clients:?}");
    }
}

/// Every `.rs` file under `dir`, with its text.
fn sources(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    let mut dirs = vec![dir.to_owned()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir).expect("readable").flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let text = fs::read_to_string(&path).expect("UTF-8");
                found.push((path, text));
            }
        }
    }
    found
}

#[test]
fn gpui_keeps_its_null_http_client_and_no_token_is_read_out() {
    let mut read = 0;
    for package in APP_CRATES {
        for (path, text) in sources(&workspace_root().join("crates").join(package).join("src")) {
            read += 1;
            for forbidden in ["set_http_client", "with_http_client", ".expose()"] {
                assert!(!text.contains(forbidden), "{} uses {forbidden}", path.display());
            }
        }
    }
    assert!(read >= 8, "the app crates' sources were read ({read} files)");
}
