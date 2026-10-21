//! What the app crates (`teams-ui` and `teams-app`) can reach: every request goes through `teams-core`, so neither
//! depends directly on an HTTP or WebSocket client (read from `cargo metadata`, resolved for the host); GPUI's HTTP
//! client stays the null one, so nothing in their sources hands GPUI another; and nothing in them reads a token's
//! value out of its `Secret`.
//!
//! The UI kit's audit (M2 Task 7), what can be checked of it: `gpui-component`'s whole tree for the host holds no
//! HTTP or WebSocket client, and every package in it is under a licence an Apache-2.0/MIT project can take; the
//! default features the kit turns on in `gpui-pre` give GPUI's Linux platform no X11 backend; and the app crates never
//! start the kit's theme directory watcher nor install a `log` logger or a `tracing` subscriber, so the kit's log
//! lines go nowhere.

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

/// The package `id` names.
fn package<'a>(metadata: &'a Value, id: &Value) -> &'a Value {
    metadata["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .find(|candidate| &candidate["id"] == id)
        .expect("a package of the resolve")
}

/// The id of the package named `name` (the one version the workspace resolves).
fn package_id(metadata: &Value, name: &str) -> Value {
    metadata["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .find(|candidate| candidate["name"] == name)
        .map_or_else(|| panic!("{name} is resolved"), |found| found["id"].clone())
}

/// The resolve's node of the package `id`.
fn node<'a>(metadata: &'a Value, id: &Value) -> &'a Value {
    metadata["resolve"]["nodes"]
        .as_array()
        .expect("resolve")
        .iter()
        .find(|node| &node["id"] == id)
        .expect("resolved")
}

/// The ids of the package `id`'s direct normal and build dependencies, as resolved.
fn dependency_ids(metadata: &Value, id: &Value) -> Vec<Value> {
    node(metadata, id)["deps"]
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
        .map(|dep| dep["pkg"].clone())
        .collect()
}

/// The names of `package`'s direct normal and build dependencies, as resolved.
fn direct_dependencies(metadata: &Value, package: &str) -> Vec<String> {
    dependency_ids(metadata, &package_id(metadata, package))
        .iter()
        .map(|id| self::package(metadata, id)["name"].as_str().expect("named").to_owned())
        .collect()
}

/// Every package `name` builds on through normal and build dependencies (proc macros and build scripts included),
/// itself among them.
fn closure<'a>(metadata: &'a Value, name: &str) -> Vec<&'a Value> {
    let mut seen: Vec<Value> = Vec::new();
    let mut pending = vec![package_id(metadata, name)];
    while let Some(id) = pending.pop() {
        if seen.contains(&id) {
            continue;
        }
        pending.extend(dependency_ids(metadata, &id));
        seen.push(id);
    }
    seen.iter().map(|id| package(metadata, id)).collect()
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

/// The licences (SPDX ids) a package in the kit's tree may be under: permissive ones, and MPL-2.0, whose copyleft
/// stops at its own files (`option-ext`, used by a build-time macro of the kit's assets crate).
const LICENCES: [&str; 14] = [
    "MIT",
    "Apache-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Zlib",
    "Unicode-3.0",
    "0BSD",
    "CC0-1.0",
    "Unlicense",
    "BSL-1.0",
    "MPL-2.0",
    "Unicode-DFS-2016",
];

/// Whether an SPDX expression (`A OR B`, `A/B`, `(A OR B) AND C`) allows use under one of [`LICENCES`]: some
/// alternative whose every part is allowed.
fn licence_allowed(expression: &str) -> bool {
    let plain = expression.replace(['(', ')'], "");
    let part_allowed = |part: &str| LICENCES.contains(&part.trim());
    // AND binds tighter than OR: `(A OR B) AND C` needs C and one of A or B
    plain.split(" AND ").all(|conjunct| {
        conjunct
            .split(" OR ")
            .flat_map(|alternative| alternative.split('/'))
            .any(part_allowed)
    })
}

#[test]
fn the_licence_check_reads_spdx_expressions() {
    for allowed in [
        "MIT OR Apache-2.0",
        "Apache-2.0/MIT",
        "(MIT OR Apache-2.0) AND Unicode-3.0",
        "Zlib",
        "MPL-2.0",
    ] {
        assert!(licence_allowed(allowed), "{allowed}");
    }
    for refused in [
        "GPL-3.0-only",
        "AGPL-3.0 OR GPL-2.0",
        "MIT AND GPL-3.0-only",
        "LGPL-2.1-or-later",
        "",
    ] {
        assert!(!licence_allowed(refused), "{refused}");
    }
}

#[test]
fn the_kit_brings_no_network_client_and_only_licences_we_can_take() {
    let metadata = metadata();
    assert!(
        direct_dependencies(&metadata, "teams-ui")
            .iter()
            .any(|name| name == "gpui-component"),
        "teams-ui takes the kit"
    );
    let tree = closure(&metadata, "gpui-component");
    assert!(tree.len() > 50, "the kit's tree was read ({} packages)", tree.len());
    let clients: Vec<&str> = tree
        .iter()
        .filter_map(|found| found["name"].as_str())
        .filter(|name| NETWORK_CLIENTS.contains(name))
        .collect();
    assert!(clients.is_empty(), "the kit's tree holds {clients:?}");
    let refused: Vec<String> = tree
        .iter()
        .filter(|found| !found["license"].as_str().is_some_and(licence_allowed))
        .map(|found| format!("{} {} ({})", found["name"], found["version"], found["license"]))
        .collect();
    assert!(refused.is_empty(), "under a licence we can't take: {refused:?}");
    // one GPUI: the kit resolves to the vendored, patched copy the workspace patches in
    let gpui: Vec<&&Value> = tree.iter().filter(|found| found["name"] == "gpui-pre").collect();
    assert_eq!(gpui.len(), 1);
    assert!(
        gpui[0]["source"].is_null(),
        "gpui-pre is the vendored path crate: {}",
        gpui[0]["id"]
    );
}

#[test]
fn the_kit_s_default_gpui_features_give_linux_no_x11_backend() {
    let metadata = metadata();
    // the kit takes gpui-pre with its defaults, `x11` among them: in gpui-pre itself that only names a compositor
    // GPUI could guess; the X11 client lives in gpui-pre-linux, whose features nobody turns `x11` on for
    let features = |name: &str| -> Vec<String> {
        node(&metadata, &package_id(&metadata, name))["features"]
            .as_array()
            .expect("features")
            .iter()
            .filter_map(|feature| feature.as_str().map(str::to_owned))
            .collect()
    };
    let linux = features("gpui-pre-linux");
    assert!(linux.iter().any(|feature| feature == "wayland"), "{linux:?}");
    assert!(
        !linux.iter().any(|feature| feature == "x11"),
        "gpui-pre-linux: {linux:?}"
    );
    let platform = features("gpui-pre-platform");
    assert!(
        !platform.iter().any(|feature| feature == "x11"),
        "gpui-pre-platform: {platform:?}"
    );
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

/// What the app crates' sources never name: another HTTP client for GPUI, a token's value, the kit's theme directory
/// watcher, a `log` logger or a `tracing` subscriber (the kit's lines would reach it).
const FORBIDDEN: [&str; 8] = [
    "set_http_client",
    "with_http_client",
    ".expose()",
    "watch_dir(",
    "set_logger",
    "set_boxed_logger",
    "set_global_default",
    "tracing_subscriber",
];

#[test]
fn gpui_keeps_its_null_http_client_and_no_token_is_read_out() {
    let mut read = 0;
    for package in APP_CRATES {
        for (path, text) in sources(&workspace_root().join("crates").join(package).join("src")) {
            read += 1;
            for forbidden in FORBIDDEN {
                assert!(!text.contains(forbidden), "{} uses {forbidden}", path.display());
            }
        }
    }
    assert!(read >= 9, "the app crates' sources were read ({read} files)");
}

/// What the app crates' sources never name: the kit's markdown and HTML view (`TextView`, its `text` module, its
/// `markdown`/`html` constructors) and its chat widgets. A message's text is drawn only by our own renderer
/// (`rich_text`), from `teams-proto`'s sanitized model.
const RENDERERS: [&str; 7] = [
    "TextView",
    "gpui_component::text",
    "::markdown(",
    "::html(",
    "MessageScroller",
    "gpui_component::message",
    "gpui_component::bubble",
];

#[test]
fn no_message_html_reaches_a_markdown_or_html_view() {
    let mut read = 0;
    for package in APP_CRATES {
        for (path, text) in sources(&workspace_root().join("crates").join(package).join("src")) {
            read += 1;
            for renderer in RENDERERS {
                assert!(!text.contains(renderer), "{} names {renderer}", path.display());
            }
        }
    }
    assert!(read >= 11, "the app crates' sources were read ({read} files)");
    // the chat view draws a message's content through our renderer
    let chat_view = fs::read_to_string(workspace_root().join("crates/teams-ui/src/chat_view.rs")).expect("UTF-8");
    assert!(chat_view.contains("rich_text::render("));
}

/// What builds a request for a new chat: the engine's action or the outbox's command, named or brought in by name
/// (`use …::Action::CreateChat`, a glob of either's variants, or the variant's fields after its bare name). A row's
/// kind (`OutboxKind::CreateChat`, shown by the view as it stands) builds nothing.
const CREATES_A_CHAT: [&str; 5] = [
    "Action::CreateChat",
    "Command::CreateChat",
    "Action::*",
    "Command::*",
    "CreateChat {",
];

/// Creating a chat is never offered (M2 Task 13): a group chat the native client creates is denied its writes
/// (CLAUDE.md: it isn't recorded in `chats.toml`), and starting a 1:1 chat needs the people search, whose results list
/// other tenant users. No app crate's source asks the engine for one.
#[test]
fn no_view_offers_creating_a_chat() {
    let mut read = 0;
    for package in APP_CRATES {
        for (path, text) in sources(&workspace_root().join("crates").join(package).join("src")) {
            read += 1;
            for builds in CREATES_A_CHAT {
                assert!(!text.contains(builds), "{} names {builds}", path.display());
            }
        }
    }
    assert!(read >= 13, "the app crates' sources were read ({read} files)");
    // compose writes through the engine's actions: a post, an edit
    let compose = fs::read_to_string(workspace_root().join("crates/teams-ui/src/compose.rs")).expect("UTF-8");
    assert!(compose.contains("self.core.write(action)"));
}
