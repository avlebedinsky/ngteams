//! How the lab and the personal build are kept apart (`docs/personal-build.md`): only this crate enables the
//! `personal` features of `teams-fence`, `teams-signin` and `teams-media` (every workspace manifest read; the last
//! picks the desktop's PipeWire over the UI sandbox's), and building a lab binary alone resolves them without those
//! features (`cargo tree`); the workspace's default members leave this crate out; only the install
//! script sets the install marker, and both binaries refuse to run without it and with `test-support`, before anything
//! else; only this crate names the personal Secret Service application and opens the personal fence, and it never opens
//! the lab's; each side refuses the other's data dir, and a lab account.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use teams::setup::Setup;
use teams_auth::signin::{WindowAnswer, WindowRequest};
use teams_auth::token::NATIVE_CLIENT_REDIRECT;
use teams_fence::lab::{ConfigError, LabLocations, LabPaths};
use teams_fence::personal::{PersonalError, PersonalPaths};
use toml::Value;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn manifest(path: &Path) -> Value {
    Value::Table(toml::from_str(&fs::read_to_string(path).unwrap()).unwrap())
}

fn workspace() -> Value {
    manifest(&root().join("Cargo.toml"))
}

fn list(value: &Value, key: &str) -> Vec<String> {
    value["workspace"][key]
        .as_array()
        .unwrap()
        .iter()
        .map(|member| member.as_str().unwrap().to_owned())
        .collect()
}

/// Every dependency table of a manifest, by its name (`dependencies`, `dev-dependencies`, `build-dependencies`, a
/// target's), with its entries.
fn dependency_tables(manifest: &Value) -> Vec<(String, &toml::Table)> {
    let mut tables = Vec::new();
    for kind in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(table) = manifest.get(kind).and_then(Value::as_table) {
            tables.push((kind.to_owned(), table));
        }
    }
    if let Some(targets) = manifest.get("target").and_then(Value::as_table) {
        for (target, value) in targets {
            for kind in ["dependencies", "dev-dependencies", "build-dependencies"] {
                if let Some(table) = value.get(kind).and_then(Value::as_table) {
                    tables.push((format!("target.{target}.{kind}"), table));
                }
            }
        }
    }
    tables
}

#[test]
fn only_this_crate_enables_the_personal_features() {
    let workspace = workspace();
    let mut found = Vec::new();
    for member in list(&workspace, "members") {
        let manifest = manifest(&root().join(&member).join("Cargo.toml"));
        for (kind, table) in dependency_tables(&manifest) {
            for crate_name in ["teams-fence", "teams-signin", "teams-media"] {
                let personal = table
                    .get(crate_name)
                    .and_then(|entry| entry.get("features"))
                    .and_then(Value::as_array)
                    .is_some_and(|features| features.iter().any(|feature| feature.as_str() == Some("personal")));
                if personal {
                    found.push(format!("{member} {kind} {crate_name}"));
                }
            }
        }
        // a feature of another crate forwarding to them
        let features = manifest.get("features").and_then(Value::as_table).into_iter().flatten();
        for (name, enables) in features {
            let forwarded = enables.as_array().into_iter().flatten().filter_map(Value::as_str);
            for enabled in forwarded.filter(|enabled| enabled.ends_with("/personal")) {
                found.push(format!("{member} features.{name} {enabled}"));
            }
        }
    }
    found.sort();
    assert_eq!(
        found,
        [
            // the two crates' own tests of their personal parts, which no binary links
            "crates/teams dependencies teams-fence",
            "crates/teams dependencies teams-media",
            "crates/teams dependencies teams-signin",
            "crates/teams dev-dependencies teams-fence",
            "crates/teams dev-dependencies teams-media",
            "crates/teams-fence dev-dependencies teams-fence",
            // with `test-support` beside it, which wins: the build still opens only the sandbox's PipeWire
            "crates/teams-media dev-dependencies teams-media",
            "crates/teams-signin dev-dependencies teams-signin",
        ]
    );
}

/// `cargo tree` of `package` alone, its normal and build dependencies with their features.
fn features_of(package: &str) -> String {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .current_dir(root())
        .args([
            "tree",
            "--offline",
            "--locked",
            "-p",
            package,
            "-e",
            "normal,build,features",
            "--prefix",
            "none",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn a_lab_binary_built_alone_has_neither_personal_feature() {
    for lab in ["teams-app", "teams-headless", "proto-lab", "teams-signin"] {
        let tree = features_of(lab);
        assert!(tree.contains("teams-fence"), "{lab}: {tree}");
        for personal in [
            r#"teams-fence feature "personal""#,
            r#"teams-signin feature "personal""#,
            r#"teams-media feature "personal""#,
        ] {
            assert!(!tree.contains(personal), "{lab} resolves {personal}");
        }
    }
    let tree = features_of("teams");
    assert!(tree.contains(r#"teams-fence feature "personal""#), "{tree}");
    assert!(tree.contains(r#"teams-signin feature "personal""#), "{tree}");
    // its calls' devices: the desktop's PipeWire, and no test reach in a plain build of it
    assert!(tree.contains(r#"teams-media feature "personal""#), "{tree}");
    assert!(!tree.contains(r#"teams-fence feature "test-support""#), "{tree}");
    assert!(!tree.contains(r#"teams-media feature "test-support""#), "{tree}");
    assert!(!tree.contains(r#"teams-core feature "test-support""#), "{tree}");
}

#[test]
fn a_plain_build_at_the_root_leaves_the_personal_build_out() {
    let workspace = workspace();
    let members = list(&workspace, "members");
    let defaults = list(&workspace, "default-members");
    assert!(members.contains(&"crates/teams".to_owned()));
    assert!(!defaults.contains(&"crates/teams".to_owned()));
    let mut rest: Vec<String> = members.into_iter().filter(|member| member != "crates/teams").collect();
    rest.sort();
    let mut sorted = defaults;
    sorted.sort();
    assert_eq!(sorted, rest, "every other member is a default one");
}

/// Every Rust, manifest and script file under `dirs` (relative to the root), with its text; `target` dirs skipped.
fn sources(dirs: &[&str]) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    let mut stack: Vec<PathBuf> = dirs.iter().map(|dir| root().join(dir)).collect();
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name != "target") {
                    stack.push(path);
                }
                continue;
            }
            let wanted = path
                .extension()
                .is_some_and(|extension| ["rs", "toml", "sh"].iter().any(|wanted| extension == *wanted));
            if let (true, Ok(text)) = (wanted, fs::read_to_string(&path)) {
                found.push((path, text));
            }
        }
    }
    found
}

/// The files under `dirs` holding `needle`, relative to the root.
fn holding(dirs: &[&str], needle: &str) -> Vec<String> {
    let base = root().canonicalize().unwrap();
    let mut found: Vec<String> = sources(dirs)
        .into_iter()
        .filter(|(_, text)| text.contains(needle))
        .map(|(path, _)| {
            let path = path.canonicalize().unwrap();
            path.strip_prefix(&base).unwrap().display().to_string()
        })
        .collect();
    found.sort();
    found
}

#[test]
fn only_the_install_script_sets_the_install_marker_and_only_this_crate_reads_it() {
    let marker = ["TEAMS_PERSONAL", "_INSTALL"].concat();
    assert_eq!(
        holding(&["crates", "scripts"], &marker),
        ["crates/teams/src/refusal.rs", "scripts/install-personal.sh"]
    );
    let refusal = fs::read_to_string(root().join("crates/teams/src/refusal.rs")).unwrap();
    assert!(refusal.contains(&format!("option_env!(\"{marker}\")")));
    let script = fs::read_to_string(root().join("scripts/install-personal.sh")).unwrap();
    assert!(script.contains(&format!("{marker}=1")) && script.contains("--release -p teams"));
}

#[test]
fn both_binaries_refuse_first() {
    // from the first function on (the imports above it name what comes later)
    let text = fs::read_to_string(root().join("crates/teams/src/main.rs")).unwrap();
    let main = &text[text.find("fn main()").unwrap()..];
    let refused = main.find("teams::refusal()").unwrap();
    for later in ["Cli::parse", "Setup::from_env", "gpui_platform", "LazyKeyring::new"] {
        assert!(
            main.find(later).is_some_and(|at| at > refused),
            "{later} before the refusal"
        );
    }
    let text = fs::read_to_string(root().join("crates/teams/src/bin/teams-signin-personal.rs")).unwrap();
    let window = &text[text.find("fn prepare()").unwrap()..];
    let refused = window.find("teams::refusal()").unwrap();
    for later in ["Setup::from_env", "read_line", "FenceLog::open", "WindowConfig {"] {
        assert!(
            window.find(later).is_some_and(|at| at > refused),
            "{later} before the refusal"
        );
    }
    let reason = teams::refusal().unwrap();
    assert!(reason.contains("test-support"), "a test build refuses: {reason}");
}

#[test]
fn only_this_crate_names_the_personal_keyring_and_opens_the_personal_fence_and_never_the_lab_s() {
    // in any crate's code (the broker's tests name it to show it refuses it)
    let application = ["\"teams", "-personal\""].concat();
    let code: Vec<String> = fs::read_dir(root().join("crates"))
        .unwrap()
        .flatten()
        .map(|entry| format!("crates/{}/src", entry.file_name().to_string_lossy()))
        .filter(|dir| root().join(dir).is_dir())
        .collect();
    let code: Vec<&str> = code.iter().map(String::as_str).collect();
    for file in holding(&code, &application) {
        assert!(file.starts_with("crates/teams/"), "{file} names the personal keyring");
    }
    assert_eq!(teams::KEYRING_APPLICATION, "teams-personal");
    assert_ne!(teams::KEYRING_APPLICATION, LabPaths::KEYRING_APPLICATION);
    let main = fs::read_to_string(root().join("crates/teams/src/main.rs")).unwrap();
    assert!(main.contains("LazyKeyring::new(KEYRING_APPLICATION)"));
    let opener = ["open", "_personal("].concat();
    for file in holding(&["crates"], &opener) {
        let allowed = file.starts_with("crates/teams/")
            || file.starts_with("crates/teams-fence/")
            || file.starts_with("crates/fence-seal/");
        assert!(allowed, "{file} opens the personal fence");
    }
    for (path, text) in sources(&["crates/teams/src"]) {
        for lab in [
            "open_lab",
            "LabPaths::KEYRING_APPLICATION",
            "teams_core::lab",
            "LabConfig::load",
        ] {
            assert!(!text.contains(lab), "{} uses {lab}", path.display());
        }
    }
}

#[test]
fn every_lab_binary_opening_the_lab_fence_refuses_a_build_with_personal() {
    let opener = ["Fence::open", "_lab"].concat();
    let mut checked = Vec::new();
    for parent in ["crates", "spikes"] {
        for entry in fs::read_dir(root().join(parent)).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let src = entry.path().join("src");
            let binary = src.join("main.rs").is_file() || src.join("bin").is_dir();
            if name == "teams" || !binary {
                continue;
            }
            let dir = format!("{parent}/{name}/src");
            if holding(&[&dir], &opener).is_empty() {
                continue;
            }
            assert!(
                !holding(&[&dir], "teams_fence::personal_enabled()").is_empty(),
                "{dir} opens the lab fence but never refuses a build with `personal`"
            );
            checked.push(dir);
        }
    }
    checked.sort();
    for expected in [
        "crates/proto-lab/src",
        "crates/teams-app/src",
        "crates/teams-headless/src",
    ] {
        assert!(
            checked.iter().any(|dir| dir == expected),
            "{expected} not checked: {checked:?}"
        );
    }
}

#[test]
fn both_binaries_built_with_test_support_refuse_and_touch_nothing() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_teams"))
        .args(["--account", "someone@example.test"])
        .env_clear()
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("test-support"));
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0, "nothing was created");

    let mut child = Command::new(env!("CARGO_BIN_EXE_teams-signin-personal"))
        .env_clear()
        .env("HOME", home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let request = WindowRequest {
        authorize: "https://login.microsoftonline.com/organizations/oauth2/v2.0/authorize".into(),
        redirect_uri: NATIVE_CLIENT_REDIRECT.into(),
    };
    if let Some(mut stdin) = child.stdin.take() {
        // it may have refused and gone before reading
        let _ = stdin.write_all(format!("{}\n", request.to_line()).as_bytes());
    }
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "one answer line: {stdout:?}");
    let WindowAnswer::Failed(reason) = WindowAnswer::parse(lines[0]).unwrap() else {
        panic!("a failure: {stdout}");
    };
    assert!(reason.contains("test-support"), "{reason}");
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0, "nothing was created");
}

#[test]
fn each_side_refuses_the_other_s_data_dir() {
    let dir = tempfile::tempdir().unwrap();
    let lab = LabLocations {
        config_file: dir.path().join("config/teams-lab/lab.toml"),
        data_dir: dir.path().join("share/teams-lab"),
        personal_data_dir: dir.path().join("share/teams"),
    };
    // the personal build refuses the lab's
    let inside = PersonalPaths::new(
        dir.path().join("config/teams"),
        dir.path().join("share/teams-lab/teams"),
    );
    assert!(matches!(
        Setup::new(inside, lab.clone()),
        Err(teams::setup::SetupError::Personal(PersonalError::LabDataDir { .. }))
    ));
    let apart = PersonalPaths::new(dir.path().join("config/teams"), dir.path().join("share/teams"));
    assert!(Setup::new(apart, lab.clone()).is_ok());
    // a lab binary refuses the personal one
    let overlapping = LabLocations {
        data_dir: dir.path().join("share/teams/lab"),
        ..lab
    };
    assert!(matches!(
        overlapping.lab_paths(),
        Err(ConfigError::PersonalDataDir { .. })
    ));
}
