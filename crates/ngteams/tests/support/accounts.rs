//! Starting the actual runner with an in-memory credential store, under an explicitly supplied synthetic fence.

use super::*;
use teams_auth::secrets::MemorySecretStore;

#[test]
fn the_runner_holds_one_engine_per_account_and_releases_it_on_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let (setup, account) = super::super::app::tests::setup(dir.path(), 1);
    let runner = Runner {
        setup,
        floor: CallFloor::new(),
        devices: Some(crate::calling::CallDevices::open().unwrap()),
    };
    let fence = || Fence::open_personal_with(&runner.setup.paths, &account.upn, &runner.setup.lab).unwrap();
    let started = runner.start_on(&account, fence(), MemorySecretStore::new()).unwrap();
    assert!(format!("{started:?}").contains(&account.upn));
    assert!(started.calls.as_ref().unwrap().video.is_some());
    let error = runner
        .start_on(&account, fence(), MemorySecretStore::new())
        .unwrap_err();
    assert!(
        error.contains(&account.upn) && error.contains("another engine runs"),
        "{error}"
    );
    started.engine.handle.shutdown();
    runner
        .start_on(&account, fence(), MemorySecretStore::new())
        .unwrap()
        .engine
        .handle
        .shutdown();
}
