//! The retained window tests launch the real ngteams app over mock engines and memory-only sessions.

use std::path::Path;
use std::sync::Arc;

use ngteams::accounts::{Runner, Started};
use ngteams::setup::Setup;
use teams_auth::authz::{AuthzClient, Region};
use teams_auth::secrets::MemorySecretStore;
use teams_auth::session::Session;
use teams_auth::token::{EntraConfig, TokenClient};
use teams_core::CallFloor;
use teams_core::EngineHandle;
use teams_core::account::AccountEngine;
use teams_fence::lab::{Alias, LabLocations};
use teams_fence::personal::{PersonalAccount, PersonalPaths};
use ui_sandbox::SandboxResult;

pub fn display_name(upn: &str) -> &str {
    upn.split_once('@').map_or(upn, |(local, _)| local)
}

pub fn started(alias: Alias, handle: EngineHandle) -> SandboxResult<Started<MemorySecretStore>> {
    let mri = match alias {
        Alias::A => teams_mock::LAB_A,
        Alias::B => teams_mock::LAB_B,
        Alias::C => teams_mock::LAB_C,
    };
    let upn = format!("bot-{alias}@lab.example.test");
    let mut account = PersonalAccount::new(
        &upn,
        mri.strip_prefix("8:orgid:").ok_or("mock orgid")?,
        "00000000-0000-4000-8000-000000000001",
    )?;
    account.name = Some(display_name(&upn).to_owned());
    // Only the sign-out watch is used: the engine already holds its mock credentials. Even an accidental
    // renewal here can reach only loopback, with an empty in-memory secret store.
    let session = Session::new(
        upn,
        MemorySecretStore::new(),
        TokenClient::new(EntraConfig::teams_native().with_authority("http://127.0.0.1:9/"))?,
        AuthzClient::with_endpoint("http://127.0.0.1:9/authz")?,
        Region::default(),
    );
    Ok(Started {
        account,
        engine: AccountEngine {
            handle,
            session: Arc::new(session),
        },
        calls: None,
    })
}

pub fn runner(root: &Path) -> SandboxResult<Runner> {
    let paths = PersonalPaths::new(root.join("ngteams-config"), root.join("ngteams-data"));
    let lab = LabLocations {
        config_file: root.join("unused/lab.toml"),
        data_dir: root.join("unused/lab"),
        personal_data_dir: paths.data_dir().to_owned(),
    };
    Ok(Runner {
        setup: Setup::new(paths, lab)?,
        floor: CallFloor::new(),
        devices: None,
    })
}
