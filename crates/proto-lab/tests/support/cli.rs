//! CLI parsing, offline artifact commands, report outcomes and refusal before opening any browser or credentials.

use std::fmt::Write as _;
use std::io::Write as _;
use std::process::Command as Process;
use std::time::SystemTime;

use proto_lab::session::FenceReport;
use serde_json::{Value, json};
use zip::write::SimpleFileOptions;

use super::*;

fn command(args: &[&str]) -> Cli {
    Cli::try_parse_from(std::iter::once("proto-lab").chain(args.iter().copied())).unwrap()
}

fn paths(root: &Path) -> (LabLocations, LabPaths) {
    let locations = LabLocations {
        config_file: root.join("config/lab.toml"),
        data_dir: root.join("data"),
        personal_data_dir: root.join("personal"),
    };
    let paths = locations.lab_paths().unwrap();
    (locations, paths)
}

#[test]
fn command_defaults_and_overrides_reach_the_typed_options() {
    match command(&["run", "ab-call", "--headless", "--timeout", "7", "--call", "decline"]).command {
        Command::Run {
            scenario,
            headless,
            timeout,
            call,
        } => {
            assert_eq!(scenario, ScenarioName::AbCall);
            assert!(headless);
            assert_eq!(timeout, 7);
            assert_eq!(call, CallStep::Decline);
        }
        _ => panic!("scenario command"),
    }
    match command(&["auth", "b", "--no-skypetoken", "--audiences"]).command {
        Command::Auth {
            alias,
            timeout,
            no_skypetoken,
            audiences,
        } => {
            assert_eq!(alias, Alias::B);
            assert_eq!(timeout, 120);
            assert!(no_skypetoken && audiences);
        }
        _ => panic!("auth command"),
    }
    for args in [
        vec!["probe", "object", "a"],
        vec!["login", "outsider"],
        vec!["run", "ab-call", "--call", "unknown"],
    ] {
        assert!(Cli::try_parse_from(std::iter::once("proto-lab").chain(args)).is_err());
    }
}

#[test]
fn report_statuses_control_success_and_preserve_saved_region() {
    for (signed_in, mri, refused) in [
        (true, MriRecord::Recorded, None),
        (false, MriRecord::AlreadyRecorded, Some(403)),
    ] {
        let report = login::LoginReport {
            signed_in,
            refused,
            mri,
            fence: FenceReport::default(),
            capture: Some("/synthetic/capture".into()),
        };
        assert_eq!(show_login(Alias::A, &report).is_err(), refused.is_some());
    }
    let root = tempfile::tempdir().unwrap();
    let (_, paths) = paths(root.path());
    let region =
        teams_auth::authz::Region::from_json(&json!({"region":"amer","partition":null,"regionGtms":{}})).unwrap();
    for token in [
        None,
        Some(Ok(auth::SkypetokenSummary {
            expires_at: SystemTime::now(),
            region: region.clone(),
        })),
        Some(Err(teams_auth::authz::AuthzError::Refused {
            status: 403,
            code: Some("synthetic refusal".into()),
        })),
    ] {
        let failed = token.as_ref().is_some_and(Result::is_err);
        let report = auth::AuthReport {
            fence: FenceReport::default(),
            signed_in: None,
            refreshed: None,
            audiences: Vec::new(),
            skypetoken: token,
        };
        assert_eq!(show_auth(&paths, Alias::A, &report).is_err(), failed);
    }
    assert!(paths.data_dir().join("auth/a.json").is_file());
    let mut report = scenario::ScenarioReport::new(ScenarioName::AbChat);
    report.notes.push("synthetic progress".into());
    assert!(show_scenario(&report).is_ok());
    let mut report = scenario::ScenarioReport::new(ScenarioName::AbChat);
    report.failures.push("synthetic goal failure".into());
    assert_eq!(show_scenario(&report).unwrap_err().to_string(), "ab-chat failed");
}

#[tokio::test]
async fn artifact_commands_use_disposable_synthetic_files() {
    let root = tempfile::tempdir().unwrap();
    let (_, paths) = paths(root.path());
    run_chrome(ChromeCommand::Path, &paths).await.unwrap();
    let build = ChromeBuild::pinned();
    let binary = build.executable(&paths.chrome_dir());
    std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
    std::fs::write(&binary, b"synthetic placeholder; never executable").unwrap();
    std::fs::write(build.install_dir(&paths.chrome_dir()).join(".verified"), &build.sha256).unwrap();
    run_chrome(ChromeCommand::Install, &paths).await.unwrap();
    let msix = synthetic_msix(root.path());
    for _ in 0..2 {
        run_bundles(BundlesCommand::Import { msix: msix.clone() }, &paths)
            .await
            .unwrap();
    }
    run_bundles(
        BundlesCommand::Search {
            pattern: "synthetic".into(),
            kind: None,
            version: None,
        },
        &paths,
    )
    .await
    .unwrap();
    assert!(
        run_bundles(
            BundlesCommand::Search {
                pattern: "[".into(),
                kind: Some(Kind::Host),
                version: Some("1.2.3.4".into())
            },
            &paths
        )
        .await
        .is_err()
    );
}

fn synthetic_msix(root: &Path) -> PathBuf {
    let path = root.join("synthetic.msix");
    let mut archive = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    for (name, data) in [
        (
            "AppxManifest.xml",
            r#"<?xml version="1.0"?><Package xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10"><Identity Name="Synthetic.Teams" Publisher="CN=Synthetic" Version="1.2.3.4"/></Package>"#,
        ),
        (
            "desktop-assets/serve-configs/work.json",
            r#"{"_synthetic":true,"version":"1.2.3.4"}"#,
        ),
        (
            "desktop-assets/unhashed-assets/synthetic.js",
            "// _synthetic\nconst host = 'https://synthetic.example.test/a';",
        ),
        ("client-bindings/bundle.js", "// _synthetic\nconst route='/synthetic';"),
    ] {
        archive.start_file(name, SimpleFileOptions::default()).unwrap();
        archive.write_all(data.as_bytes()).unwrap();
    }
    archive.finish().unwrap();
    path
}

#[test]
fn commands_stop_at_missing_config_before_credentials_or_browser_start() {
    let dir = tempfile::tempdir().unwrap();
    let output = Process::new(std::env::current_exe().unwrap())
        .args(["--exact", "tests::missing_config_child", "--nocapture"])
        .env_clear()
        .env("PROTO_LAB_MAIN_TEST_CHILD", "1")
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}/no-bus", dir.path().display()),
        )
        .envs(
            ["PATH", "LLVM_PROFILE_FILE"]
                .into_iter()
                .filter_map(|key| std::env::var_os(key).map(|value| (key, value))),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn missing_config_child() {
    if std::env::var_os("PROTO_LAB_MAIN_TEST_CHILD").is_none() {
        return;
    }
    for args in [
        vec!["login", "a"],
        vec!["auth", "b"],
        vec!["status"],
        vec!["status", "c"],
        vec!["probe", "chat", "a", "--with", "b", "--save", "../refused"],
        vec!["probe", "thread", "b"],
        vec!["probe", "profile", "a"],
        vec!["probe", "teams", "c"],
        vec!["probe", "object", "a", "synthetic"],
        vec!["run", "ab-chat", "--headless"],
        vec!["service-mri", "echo", "28:00000000-0000-4000-8000-000000000001"],
    ] {
        let error = run(command(&args)).await.unwrap_err().to_string();
        assert!(error.contains("lab.toml"), "{args:?}: {error}");
    }
    run(command(&["chrome", "path"])).await.unwrap();
    valid_config_child().await;
}

#[test]
fn exports_resolve_relative_captures_and_refuse_corrupt_lab_configuration() {
    let root = tempfile::tempdir().unwrap();
    let (locations, paths) = paths(root.path());
    let capture = paths.captures_dir().join("synthetic/1");
    std::fs::create_dir_all(&capture).unwrap();
    std::fs::write(
        capture.join("events.jsonl"),
        "{\"_synthetic\":true,\"method\":\"GET\",\"url\":\"https://example.test/\"}\n",
    )
    .unwrap();
    run_export(
        &locations,
        &paths,
        Path::new("synthetic/1"),
        "synthetic/without-lab",
        export::Checks::Warn,
    )
    .unwrap();
    let copy = paths.data_dir().join("reference/synthetic/without-lab.jsonl");
    let value: serde_json::Value = serde_json::from_str(std::fs::read_to_string(copy).unwrap().trim()).unwrap();
    assert_eq!(value["_synthetic"], true);
    std::fs::create_dir_all(locations.config_file.parent().unwrap()).unwrap();
    std::fs::write(&locations.config_file, "not a lab config").unwrap();
    assert!(
        run_export(
            &locations,
            &paths,
            &capture,
            "synthetic/refused",
            export::Checks::Strict
        )
        .is_err()
    );
    assert!(!paths.data_dir().join("reference/synthetic/refused.jsonl").exists());
}

fn report_config(root: &Path) -> LabConfig {
    let mut text = format!("data_dir = {:?}\n", root.join("data").to_string_lossy());
    for alias in ["a", "b", "c"] {
        writeln!(
            text,
            "[[accounts]]\nalias = {alias:?}\nupn = \"synthetic-{alias}@example.test\""
        )
        .unwrap();
    }
    LabConfig::parse(&text, root).unwrap()
}

#[test]
fn report_references_are_lazy_private_and_refuse_repository_destinations() {
    let root = tempfile::tempdir().unwrap();
    let config = report_config(root.path());
    let called = std::cell::Cell::new(false);
    show_reads(
        &config,
        Alias::A,
        vec!["a: synthetic summary only".to_owned()],
        None,
        || {
            called.set(true);
            Vec::new()
        },
    )
    .unwrap();
    assert!(!called.get(), "reading a summary must not materialize captured bodies");
    let events = || {
        vec![
            json!({"_synthetic":true,"type":"response","method":"GET","url":"https://example.test/read","body":{"syntheticMarker":"MadeUpReportBody"}}),
        ]
    };
    show_reads(
        &config,
        Alias::A,
        vec!["a: HTTP 200".to_owned()],
        Some("synthetic/report"),
        events,
    )
    .unwrap();
    let saved = config.paths().data_dir().join("reference/synthetic/report.jsonl");
    let saved: Value = serde_json::from_str(std::fs::read_to_string(saved).unwrap().trim()).unwrap();
    assert_eq!(saved["_synthetic"], true);
    std::fs::create_dir(root.path().join(".git")).unwrap();
    let error = show_reads(&config, Alias::B, Vec::new(), Some("synthetic/refused"), events).unwrap_err();
    assert!(error.to_string().contains("git work tree"));
    assert!(
        !config
            .paths()
            .data_dir()
            .join("reference/synthetic/refused.jsonl")
            .exists()
    );
}

#[tokio::test]
async fn status_checks_default_accounts_and_propagates_refusals_breaches_and_errors() {
    use proto_lab::session::{FenceReport, SessionError};
    let root = tempfile::tempdir().unwrap();
    let config = report_config(root.path());
    let mut seen = Vec::new();
    check_status(&config, Vec::new(), 7, async |paths: &LabPaths, alias, timeout| {
        assert_eq!(paths.data_dir(), config.paths().data_dir());
        assert_eq!(timeout, Duration::from_secs(7));
        seen.push(alias);
        Ok((AccountStatus::Ok, FenceReport::default()))
    })
    .await
    .unwrap();
    assert_eq!(seen, Alias::ALL);
    let error = check_status(&config, vec![Alias::B], 1, async |_: &LabPaths, alias, _| {
        assert_eq!(alias, Alias::B);
        Ok((AccountStatus::LoginNeeded, FenceReport::default()))
    })
    .await
    .unwrap_err();
    assert_eq!(error.to_string(), "not every account is ok");
    let mut seen = Vec::new();
    let error = check_status(&config, Alias::ALL.to_vec(), 1, async |_: &LabPaths, alias, _| {
        seen.push(alias);
        Ok((
            AccountStatus::Ok,
            FenceReport {
                unsanctioned_frames: 1,
                ..FenceReport::default()
            },
        ))
    })
    .await
    .unwrap_err();
    assert!(error.to_string().contains("fence was breached"));
    assert_eq!(seen, [Alias::A], "a breach stops before another account is opened");
    let error = check_status(&config, vec![Alias::C], 1, async |_: &LabPaths, _, _| {
        Err(SessionError::NoChrome("/synthetic/no-chrome".into()))
    })
    .await
    .unwrap_err();
    assert!(error.to_string().contains("no Chrome"));
}

async fn valid_config_child() {
    let locations = LabLocations::from_env().unwrap();
    std::fs::create_dir_all(locations.config_file.parent().unwrap()).unwrap();
    let mut text = format!("data_dir = {:?}\n", locations.data_dir.to_string_lossy());
    for alias in ["a", "b", "c"] {
        writeln!(
            text,
            "[[accounts]]\nalias = {alias:?}\nupn = \"synthetic-{alias}@example.test\""
        )
        .unwrap();
    }
    text.push_str("[[service_participants]]\nname = \"echo\"\n");
    std::fs::write(&locations.config_file, text).unwrap();
    valid_config_refusals().await;
    for _ in 0..2 {
        run(command(&[
            "service-mri",
            "echo",
            "28:00000000-0000-4000-8000-000000000001",
        ]))
        .await
        .unwrap();
    }
    let capture = locations.data_dir.join("captures/synthetic/1");
    std::fs::create_dir_all(&capture).unwrap();
    std::fs::write(capture.join("events.jsonl"), "{\"_synthetic\":true}\n").unwrap();
    for args in [
        vec!["export", "synthetic/1", "synthetic/warn"],
        vec!["export", "synthetic/1", "synthetic/strict", "--strict"],
    ] {
        run(command(&args)).await.unwrap();
    }
    assert!(locations.data_dir.join("reference/synthetic/strict.jsonl").is_file());
}

async fn valid_config_refusals() {
    for args in [
        vec!["probe", "chat", "a", "--save", "../refused"],
        vec!["probe", "chat", "b", "--save", "../refused"],
        vec!["probe", "thread", "b", "--save", "../refused"],
        vec!["probe", "object", "c", "synthetic", "--save", "../refused"],
    ] {
        let error = run(command(&args)).await.unwrap_err();
        assert!(
            error.downcast_ref::<export::ExportError>().is_some(),
            "{args:?}: {error}"
        );
    }
    for args in [
        vec!["auth", "a"],
        vec!["probe", "profile", "b"],
        vec!["probe", "teams", "c"],
    ] {
        let error = run(command(&args)).await.unwrap_err().to_string();
        assert!(
            error.contains("Secret Service"),
            "the disposable nonexistent bus must fail before any credential read: {args:?}: {error}"
        );
    }
    for args in [
        vec!["status"],
        vec!["status", "c"],
        vec!["run", "load", "--headless"],
        vec!["run", "load"],
    ] {
        let error = run(command(&args)).await.unwrap_err().to_string();
        assert!(
            error.contains("no Chrome for Testing"),
            "no executable exists in the disposable lab: {args:?}: {error}"
        );
    }
}
