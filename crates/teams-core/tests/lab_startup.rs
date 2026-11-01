//! Concrete lab assembly from disposable synthetic configuration and empty in-memory credentials.
//! No environment lookup, user keyring, tenant request or real device is used.

#![cfg(test)]

use std::sync::Arc;

use teams_auth::authz::{CHAT_BASE_GTM, Region};
use teams_auth::secrets::MemorySecretStore;
use teams_auth::token::TokenError;
use teams_core::EngineError;
use teams_core::account::AccountError;
use teams_core::lab::{self, LabError};
use teams_fence::lab::{Alias, LabConfig};
use teams_fence::{Fence, FenceLog, Mri, Roster};
use teams_mock::{LAB_A, LAB_B, LAB_C};
use teams_proto::IC3_AUDIENCE;

fn config(root: &std::path::Path, identity: bool) -> LabConfig {
    let text = teams_mock::lab_toml(root);
    let text = if identity {
        text
    } else {
        text.replace(&format!("mri = \"{LAB_A}\""), "")
    };
    LabConfig::parse(&text, root).unwrap()
}

fn fence() -> Arc<Fence> {
    Arc::new(
        Fence::new(
            Roster::new([Mri::new(LAB_A), Mri::new(LAB_B), Mri::new(LAB_C)], []),
            FenceLog::from_writer(std::io::sink()),
        )
        .with_own_account(Mri::new(LAB_A)),
    )
}

fn save_region(config: &LabConfig, chat: bool) {
    let region = Region {
        name: Some("amer".into()),
        partition: Some("synthetic-partition".into()),
        endpoints: if chat {
            [(
                CHAT_BASE_GTM.into(),
                "https://teams.microsoft.com/api/chatsvc/amer".into(),
            )]
            .into()
        } else {
            std::collections::BTreeMap::new()
        },
    };
    let file = config.paths().region_file(Alias::A);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, region.to_json().to_string()).unwrap();
}

#[test]
fn lab_identity_and_region_failures_stop_before_the_account_can_start() {
    let root = tempfile::tempdir().unwrap();
    let unsigned = config(root.path(), false);
    let error = lab::start(&unsigned, Alias::A, fence(), MemorySecretStore::new(), false, None).unwrap_err();
    assert!(matches!(error, LabError::NoMri(Alias::A)));
    assert!(error.to_string().contains("proto-lab login a"));
    assert_eq!(lab::known_names(&unsigned).len(), 2);
    let configured = config(root.path(), true);
    let error = lab::start(&configured, Alias::A, fence(), MemorySecretStore::new(), false, None).unwrap_err();
    assert!(matches!(error, LabError::Region { alias: Alias::A, .. }));
    assert!(error.to_string().contains("proto-lab auth a"));
    let file = configured.paths().region_file(Alias::A);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "{\"_synthetic\":true,\"not-a-region\":true}").unwrap();
    assert!(matches!(
        lab::start(&configured, Alias::A, fence(), MemorySecretStore::new(), false, None),
        Err(LabError::Region { .. })
    ));
    save_region(&configured, false);
    assert!(matches!(
        lab::start(&configured, Alias::A, fence(), MemorySecretStore::new(), false, None),
        Err(LabError::Account(AccountError::NoChatBase))
    ));
}

#[tokio::test]
async fn lab_assembly_binds_the_alias_store_names_credentials_and_single_engine_lock() {
    for push in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let configured = config(root.path(), true);
        save_region(&configured, true);
        let engine = lab::start(&configured, Alias::A, fence(), MemorySecretStore::new(), push, None).unwrap();
        assert!(!engine.handle.requests().read_only());
        for (mri, name) in lab::known_names(&configured) {
            assert_eq!(
                engine.handle.person(&mri).await.unwrap().name.as_deref(),
                Some(name.as_str())
            );
        }
        assert!(matches!(
            engine.session.access_token(IC3_AUDIENCE).await,
            Err(TokenError::NotSignedIn { .. })
        ));
        assert!(matches!(
            lab::start(&configured, Alias::A, fence(), MemorySecretStore::new(), push, None),
            Err(LabError::Account(AccountError::Engine(
                EngineError::AlreadyRunning { .. }
            )))
        ));
        engine.handle.shutdown();
        assert!(configured.paths().store_file(Alias::A).is_file());
        assert!(!configured.paths().store_file(Alias::B).exists());
    }
    assert_eq!(lab::display_name("synthetic@example.test"), "synthetic");
    assert_eq!(
        lab::display_name("synthetic-without-domain"),
        "synthetic-without-domain"
    );
    assert_eq!(lab::display_name("@example.test"), "");
}
