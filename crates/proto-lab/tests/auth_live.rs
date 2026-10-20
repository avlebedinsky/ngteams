//! Live, against the company tenant with the 3 lab accounts: the native sign-in of `proto-lab auth` through each
//! account's persistent profile under the armed fence (`Fence::open_lab`, fail-closed), with the tokens kept in memory
//! (never the Secret Service, never a file).
//!
//! Built only with `--features live`, and even then each test skips unless `PROTO_LAB_LIVE=1`, so
//! `cargo test --all-features` stays offline. The tests share the lab profiles, so they take turns.

use std::time::{Duration, SystemTime};

use proto_lab::commands::auth::{AuthOptions, AuthReport, authorize};
use proto_lab::config::{Alias, LabConfig, LabLocations};
use teams_auth::authz::skypetoken_key;
use teams_auth::refresh::refresh_token_key;
use teams_auth::secrets::{MemorySecretStore, SecretStore};
use teams_auth::token::SPACES_RESOURCE;

/// One browser per lab profile at a time.
static PROFILES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// The lab config, or `None` (after a note) when live tests aren't asked for.
fn live() -> TestResult<Option<LabConfig>> {
    if std::env::var("PROTO_LAB_LIVE").as_deref() != Ok("1") {
        eprintln!("skipped: a live lab test; set PROTO_LAB_LIVE=1 to run it");
        return Ok(None);
    }
    Ok(Some(LabConfig::load(&LabLocations::from_env()?)?))
}

async fn sign_in(
    config: &LabConfig,
    alias: Alias,
    store: &MemorySecretStore,
    skypetoken: bool,
) -> TestResult<AuthReport> {
    let options = AuthOptions {
        timeout: Duration::from_secs(120),
        skypetoken,
        other_audiences: false,
    };
    let report = authorize(config, alias, store, &options)
        .await
        .map_err(|error| format!("{alias}: {error}"))?;
    for line in report.lines(SystemTime::now()) {
        eprintln!("{alias}: {line}");
    }
    Ok(report)
}

#[tokio::test]
async fn every_lab_account_signs_in_natively_and_refreshes() {
    let Some(config) = live().unwrap() else {
        return;
    };
    let _turn = PROFILES.lock().await;
    for alias in Alias::ALL {
        let store = MemorySecretStore::new();
        let report = sign_in(&config, alias, &store, false).await.unwrap();
        for summary in [&report.signed_in, &report.refreshed] {
            let summary = summary
                .as_ref()
                .unwrap_or_else(|| panic!("{alias}: an opaque access token"));
            assert_eq!(summary.audience, [SPACES_RESOURCE], "{alias}");
            assert!(summary.expires_at.is_some_and(|exp| exp > SystemTime::now()), "{alias}");
        }
        let upn = &config.account(alias).upn;
        assert!(
            store.get(&refresh_token_key(upn)).await.unwrap().is_some(),
            "{alias}: the refresh token is kept"
        );
        assert!(!report.fence.breached(), "{alias}: {}", report.fence.summary());
    }
}

#[tokio::test]
async fn every_lab_account_gets_a_skypetoken() {
    let Some(config) = live().unwrap() else {
        return;
    };
    let _turn = PROFILES.lock().await;
    for alias in Alias::ALL {
        let store = MemorySecretStore::new();
        let report = sign_in(&config, alias, &store, true).await.unwrap();
        let skypetoken = match report.skypetoken {
            Some(Ok(skypetoken)) => skypetoken,
            Some(Err(error)) => panic!("{alias}: {error}"),
            None => panic!("{alias}: no exchange"),
        };
        assert!(skypetoken.expires_at > SystemTime::now(), "{alias}");
        // authz never lets a skypetoken outlive the access token it was given: its `exp` is that token's `exp`
        let spaces_expiry = report.refreshed.as_ref().and_then(|summary| summary.expires_at);
        assert!(
            spaces_expiry.is_some_and(|exp| skypetoken.expires_at <= exp),
            "{alias}: the skypetoken outlives the api.spaces.skype.com token"
        );
        assert!(
            !skypetoken.region.endpoints.is_empty(),
            "{alias}: no regional endpoints"
        );
        let upn = &config.account(alias).upn;
        assert!(store.get(&skypetoken_key(upn)).await.unwrap().is_some(), "{alias}");
    }
}
