//! How the lab and the personal build are kept apart (`docs/personal-build.md`): only this crate enables the
//! `personal` features of `teams-fence` and `teams-media` (every workspace manifest read; the last
//! picks the desktop's PipeWire over the UI sandbox's), and building a lab binary alone resolves them without those
//! features (`cargo tree`); the workspace's default members leave this crate out; only the install
//! script sets the install marker, and the binary refuses to run without it and with `test-support`, before anything
//! else (the sign-in window's page engine included, which it serves as its own worker); only this crate names the personal Secret Service application and opens the personal fence, and it never opens
//! the lab's; each side refuses the other's data dir, and a lab account.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use ngteams::setup::Setup;
use teams_fence::lab::{ConfigError, LabLocations, LabPaths};
use teams_fence::personal::{PersonalAccount, PersonalError, PersonalPaths};
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
            for crate_name in ["teams-fence", "teams-media"] {
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
            "crates/ngteams dependencies teams-fence",
            "crates/ngteams dependencies teams-media",
            "crates/ngteams dev-dependencies teams-fence",
            "crates/ngteams dev-dependencies teams-media",
            "crates/teams-fence dev-dependencies teams-fence",
            // with `test-support` beside it, which wins: the build still opens only the sandbox's PipeWire
            "crates/teams-media dev-dependencies teams-media",
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
    assert!(!features_of("proto-lab").contains("teams-fence"));
    let tree = features_of("teams-signin");
    assert!(!tree.contains("teams-fence"), "teams-signin: {tree}");
    let tree = features_of("ngteams");
    assert!(tree.contains(r#"teams-fence feature "personal""#), "{tree}");
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
    assert!(members.contains(&"crates/ngteams".to_owned()));
    assert!(!defaults.contains(&"crates/ngteams".to_owned()));
    let mut rest: Vec<String> = members
        .into_iter()
        .filter(|member| member != "crates/ngteams")
        .collect();
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
        ["crates/ngteams/src/refusal.rs", "scripts/install-personal.sh"]
    );
    let refusal = fs::read_to_string(root().join("crates/ngteams/src/refusal.rs")).unwrap();
    assert!(refusal.contains(&format!("option_env!(\"{marker}\")")));
    let script = fs::read_to_string(root().join("scripts/install-personal.sh")).unwrap();
    assert!(script.contains(&format!("{marker}=1")) && script.contains("--release -p ngteams"));
}

#[test]
fn the_binary_refuses_first() {
    // from the first function on (the imports above it name what comes later)
    let text = fs::read_to_string(root().join("crates/ngteams/src/main.rs")).unwrap();
    let main = &text[text.find("fn main()").unwrap()..];
    let refused = main.find("ngteams::refusal()").unwrap();
    // the keyring is opened only through `ngteams::accounts`, which `main` reaches after the refusal; the sign-in
    // window's page engine is served after it too
    for later in [
        "serve_if_asked",
        "Cli::parse",
        "Setup::from_env",
        "gpui_platform",
        "accounts::",
    ] {
        assert!(
            main.find(later).is_some_and(|at| at > refused),
            "{later} before the refusal"
        );
    }
    let reason = ngteams::refusal().unwrap();
    assert!(reason.contains("test-support"), "a test build refuses: {reason}");
}

#[test]
fn only_this_crate_names_the_personal_keyring_and_opens_the_personal_fence_and_never_the_lab_s() {
    // as a store's application argument in any crate's code (the broker's tests name it to show it refuses it); the
    // data dir shares the name, never as an argument
    let application = ["\"ngteams", "\")"].concat();
    let code: Vec<String> = fs::read_dir(root().join("crates"))
        .unwrap()
        .flatten()
        .map(|entry| format!("crates/{}/src", entry.file_name().to_string_lossy()))
        .filter(|dir| root().join(dir).is_dir())
        .collect();
    let code: Vec<&str> = code.iter().map(String::as_str).collect();
    for file in holding(&code, &application) {
        assert!(file.starts_with("crates/ngteams/"), "{file} names the personal keyring");
    }
    assert_eq!(ngteams::KEYRING_APPLICATION, "ngteams");
    assert_ne!(ngteams::KEYRING_APPLICATION, LabPaths::KEYRING_APPLICATION);
    let accounts = fs::read_to_string(root().join("crates/ngteams/src/accounts.rs")).unwrap();
    assert!(accounts.contains("LazyKeyring::new(KEYRING_APPLICATION)"));
    let opener = ["open", "_personal("].concat();
    for file in holding(&["crates"], &opener) {
        let allowed = file.starts_with("crates/ngteams/")
            || file.starts_with("crates/teams-fence/")
            || file.starts_with("crates/fence-seal/");
        assert!(allowed, "{file} opens the personal fence");
    }
    for (path, text) in sources(&["crates/ngteams/src"]) {
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
fn the_binary_built_with_test_support_refuses_and_touches_nothing() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ngteams"))
        .args(["--account", "someone@example.test"])
        .env_clear()
        .envs(std::env::var_os("LLVM_PROFILE_FILE").map(|value| ("LLVM_PROFILE_FILE", value)))
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("test-support"));
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0, "nothing was created");
}

#[test]
fn each_side_refuses_the_other_s_data_dir() {
    let dir = tempfile::tempdir().unwrap();
    let lab = LabLocations {
        config_file: dir.path().join("config/teams-lab/lab.toml"),
        data_dir: dir.path().join("share/teams-lab"),
        personal_data_dir: dir.path().join("share/ngteams"),
    };
    // the personal build refuses the lab's
    let inside = PersonalPaths::new(
        dir.path().join("config/ngteams"),
        dir.path().join("share/teams-lab/teams"),
    );
    assert!(matches!(
        Setup::new(inside, lab.clone()),
        Err(ngteams::setup::SetupError::Personal(PersonalError::LabDataDir { .. }))
    ));
    let apart = PersonalPaths::new(dir.path().join("config/ngteams"), dir.path().join("share/ngteams"));
    assert!(Setup::new(apart, lab.clone()).is_ok());
    // a lab binary refuses the personal one
    let overlapping = LabLocations {
        data_dir: dir.path().join("share/ngteams/lab"),
        ..lab
    };
    assert!(matches!(
        overlapping.lab_paths(),
        Err(ConfigError::PersonalDataDir { .. })
    ));
}

#[test]
fn a_lab_account_among_the_personal_accounts_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("config/teams-lab/lab.toml");
    fs::create_dir_all(config_file.parent().unwrap()).unwrap();
    let lab_toml = ["a", "b", "c"].map(|alias| {
        let end = alias.repeat(2);
        format!(
            "[[accounts]]\nalias = \"{alias}\"\nupn = \"bot-{alias}@lab.example.test\"\n\
             mri = \"8:orgid:00000000-0000-4000-8000-0000000000{end}\"\n"
        )
    });
    fs::write(&config_file, lab_toml.concat()).unwrap();
    let lab = LabLocations {
        config_file,
        data_dir: dir.path().join("share/teams-lab"),
        personal_data_dir: dir.path().join("share/ngteams"),
    };
    let paths = PersonalPaths::new(dir.path().join("config/ngteams"), dir.path().join("share/ngteams"));
    let setup = Setup::new(paths, lab).unwrap();
    setup.ask_for("someone@example.test").unwrap();
    // written beside it by hand: every listing of the accounts refuses it, and asking for it does too
    PersonalAccount::pending("BOT-A@lab.example.test")
        .unwrap()
        .save(&setup.paths)
        .unwrap();
    for refused in [setup.accounts(), setup.signed_in_accounts()] {
        assert!(
            matches!(
                refused,
                Err(ngteams::setup::SetupError::Personal(PersonalError::LabAccount))
            ),
            "{refused:?}"
        );
    }
    assert!(matches!(
        setup.ask_for("bot-a@lab.example.test"),
        Err(ngteams::setup::SetupError::Personal(PersonalError::LabAccount))
    ));
}
