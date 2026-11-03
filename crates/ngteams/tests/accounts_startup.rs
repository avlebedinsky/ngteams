//! Concrete account wrappers that must fail before any keyring, tenant request or page starts. They run in a
//! subprocess with temporary HOME/XDG roots and an unavailable fixture bus, never the operator's environment.

#![cfg(test)]

use std::path::{Path, PathBuf};
use std::process::Command;

use gpui::TestAppContext;
use ngteams::accounts::{self, Runner};
use ngteams::accounts_file::{PersonalAccount, PersonalPaths};
use ngteams::forget::Forgot;
use ngteams::setup::Setup;
use ngteams::signin;
use teams_core::CallFloor;
use teams_store::EngineLock;

const CHILD: &str = "NGTEAMS_SAFE_ACCOUNTS_STARTUP_CHILD";
const UPN: &str = "signed@synthetic.example";
const OID: &str = "00000000-0000-4000-8000-0000000000ff";
const TID: &str = "00000000-0000-4000-8000-000000000001";

#[test]
fn concrete_account_wrappers_stop_before_keyring_or_tenant_access() {
    if std::env::var(CHILD).as_deref() == Ok("child") {
        check_wrappers(&PathBuf::from(std::env::var_os("HOME").unwrap()));
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "concrete_account_wrappers_stop_before_keyring_or_tenant_access",
            "--nocapture",
        ])
        .env(CHILD, "child")
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("XDG_DATA_HOME", dir.path().join("share"))
        .env("XDG_RUNTIME_DIR", dir.path().join("run"))
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}/no-session-bus", dir.path().display()),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn check_wrappers(root: &Path) {
    let setup = Setup::new(PersonalPaths::new(
        root.join("config/ngteams"),
        root.join("share/ngteams"),
    ))
    .unwrap();
    let runner = Runner {
        setup: setup.clone(),
        floor: CallFloor::new(),
        devices: None,
    };
    let pending = setup.ask_for("pending@synthetic.example").unwrap();
    assert!(!accounts::ready(&setup, &pending).unwrap());
    let error = runner.start(&pending).unwrap_err();
    assert!(error.contains(&pending.upn));
    assert_eq!(
        accounts::forget(&setup, "missing@synthetic.example").unwrap(),
        Forgot::Nothing
    );

    let signed = PersonalAccount::new(UPN, OID, TID).unwrap();
    signed.save(&setup.paths).unwrap();
    // With no region file ready returns before reading its lazy secret store.
    assert!(!accounts::ready(&setup, &signed).unwrap());
    let error = runner.start(&signed).unwrap_err();
    assert!(error.contains(UPN) && error.contains("region"), "{error}");
    let region = setup.paths.region_file(&signed).unwrap();
    std::fs::create_dir_all(region.parent().unwrap()).unwrap();
    std::fs::write(region, b"not a region").unwrap();
    let error = runner.start(&signed).unwrap_err();
    assert!(error.contains(UPN) && error.contains("region"), "{error}");

    let store = setup.paths.store_file(&signed).unwrap();
    let held = EngineLock::hold(&store).unwrap();
    let error = accounts::forget(&setup, UPN).unwrap_err();
    assert!(error.contains("Teams is running"), "{error}");
    assert!(
        PersonalAccount::find(&setup.paths, UPN).unwrap().is_some(),
        "a refused forget keeps the account"
    );
    drop(held);
    rejected_windows(&setup, &signed);
}

fn rejected_windows(setup: &Setup, account: &PersonalAccount) {
    let cx = TestAppContext::single();
    let windows = cx.update(signin::windows);
    let mut invalid = account.clone();
    invalid.idp_hosts = vec!["NOT-A-DNS-NAME".to_owned()];
    let error = accounts::sign_in(setup, &invalid, &windows).unwrap_err();
    assert!(
        error.contains("NOT-A-DNS-NAME") && error.contains("identity provider host"),
        "{error}"
    );
    assert!(accounts::add(setup, "invalid-upn", &windows).is_err());
    assert!(PersonalAccount::find(&setup.paths, "invalid-upn").unwrap().is_none());
    cx.run_until_parked();
    assert_eq!(
        cx.update(|cx| cx.windows().len()),
        0,
        "no rejected request opens a page"
    );
}
