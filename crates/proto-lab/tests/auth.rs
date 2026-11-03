//! `proto-lab auth`'s report and the persisted region: what the operator sees (claims and expiries, the refusals) and
//! what lands on disk (the region, never a token). The browser step is in `tests/auth_redirect.rs`, the live run in
//! `tests/auth_live.rs`.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt as _;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use proto_lab::commands::auth::{
    AudienceOutcome, AuthError, AuthReport, NativeTarget, SkypetokenSummary, region_file, save_region, within,
};
use proto_lab::config::{Alias, LabPaths};
use proto_lab::session::SessionReport;
use serde_json::Value;
use teams_auth::authz::{AuthzError, Region};
use teams_auth::claims::TokenSummary;
use teams_auth::id_token::Account;
use teams_auth::token::{EntraError, TokenError};

const NOW: u64 = 2_000_000_000;

fn at(seconds: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(seconds)
}

fn native_identity() -> Account {
    Account {
        username: "synthetic-a@example.test".into(),
        object_id: "00000000-0000-4000-8000-00000000000a".into(),
        tenant: "00000000-0000-4000-8000-000000000001".into(),
        display_name: Some("Synthetic Account".into()),
    }
}

#[test]
fn native_account_and_region_are_private_atomic_and_keep_manual_same_account_settings() {
    let dir = tempfile::tempdir().unwrap();
    let target = NativeTarget::resolve(Alias::B, None, None, Some(dir.path().as_os_str().to_owned())).unwrap();
    let identity = native_identity();
    let file = target.save(&identity, Some(&region())).unwrap();
    assert_eq!(file, dir.path().join(".local/share/ngteams-lab/b/accounts.toml"));
    let mode = |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(target.root()), 0o700);
    assert_eq!(mode(&file), 0o600);
    let native_region = target.root().join(format!("regions/{}.json", identity.object_id));
    assert_eq!(mode(native_region.parent().unwrap()), 0o700);
    assert_eq!(mode(&native_region), 0o600);
    let mut saved: toml::Value = toml::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(saved["account"][0]["name"].as_str(), Some("Synthetic Account"));
    saved["account"][0]["idp_hosts"] = toml::Value::Array(vec!["signin.example.test".into()]);
    std::fs::write(&file, toml::to_string(&saved).unwrap()).unwrap();
    target
        .save(
            &Account {
                display_name: None,
                ..identity.clone()
            },
            None,
        )
        .unwrap();
    let saved: toml::Value = toml::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(saved["account"][0]["name"].as_str(), Some("Synthetic Account"));
    assert_eq!(
        saved["account"][0]["idp_hosts"][0].as_str(),
        Some("signin.example.test")
    );
    assert_eq!(std::fs::read_dir(target.root()).unwrap().count(), 2);
    assert_eq!(std::fs::read_dir(target.root().join("regions")).unwrap().count(), 1);
    target
        .save(
            &Account {
                object_id: "../../escape".into(),
                ..identity
            },
            Some(&region()),
        )
        .unwrap_err();
    assert!(!target.root().parent().unwrap().join("escape.json").exists());
}

#[test]
fn native_target_uses_xdg_data_and_rejects_personal_overlap_before_writing() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let config = dir.path().join("config");
    let target = || {
        NativeTarget::resolve(
            Alias::C,
            Some(data.as_os_str().to_owned()),
            Some(config.as_os_str().to_owned()),
            None,
        )
    };
    assert_eq!(target().unwrap().root(), data.join("ngteams-lab/c"));
    assert!(NativeTarget::resolve(Alias::A, Some("relative".into()), None, None).is_err());
    std::fs::create_dir_all(data.join("ngteams-lab")).unwrap();
    std::fs::create_dir_all(config.join("ngteams")).unwrap();
    std::os::unix::fs::symlink(config.join("ngteams"), data.join("ngteams-lab/c")).unwrap();
    assert!(target().is_err());
    assert!(std::fs::read_dir(config.join("ngteams")).unwrap().next().is_none());
}

fn spaces_summary() -> TokenSummary {
    TokenSummary {
        audience: vec!["https://api.spaces.skype.com".to_owned()],
        expires_at: Some(at(NOW + 7_800)),
        scopes: vec!["Authorization.ReadWrite".to_owned(), "user_impersonation".to_owned()],
        tenant: Some("00000000-0000-4000-8000-0000000000ff".to_owned()),
    }
}

fn region() -> Region {
    Region {
        name: Some("emea".to_owned()),
        partition: Some("emea01".to_owned()),
        endpoints: BTreeMap::from([
            (
                "chatServiceAfd".to_owned(),
                "https://teams.example.test/api/chatsvc/emea/".to_owned(),
            ),
            (
                "middleTier".to_owned(),
                "https://teams.example.test/api/mt/emea/".to_owned(),
            ),
        ]),
    }
}

fn blocked() -> TokenError {
    TokenError::ConditionalAccess(Box::new(EntraError {
        error: "invalid_grant".to_owned(),
        codes: vec![53_003],
        description: "Access has been blocked by Conditional Access policies.".to_owned(),
        correlation_id: None,
        suberror: None,
    }))
}

fn report(skypetoken: Option<Result<SkypetokenSummary, AuthzError>>) -> AuthReport {
    AuthReport {
        browser: SessionReport::default(),
        signed_in: Some(spaces_summary()),
        refreshed: Some(spaces_summary()),
        audiences: vec![
            AudienceOutcome {
                resource: "https://graph.microsoft.com".to_owned(),
                token: Ok(Some(TokenSummary {
                    audience: vec!["https://graph.microsoft.com".to_owned()],
                    expires_at: Some(at(NOW - 120)),
                    scopes: vec!["User.Read".to_owned()],
                    tenant: None,
                })),
            },
            AudienceOutcome {
                resource: "https://outlook.office.com".to_owned(),
                token: Err(blocked()),
            },
            AudienceOutcome {
                resource: "https://ic3.teams.office.com".to_owned(),
                token: Ok(None),
            },
        ],
        skypetoken,
    }
}

#[test]
fn the_report_shows_claims_expiries_and_refusals() {
    let skypetoken = SkypetokenSummary {
        expires_at: at(NOW + 86_400),
        region: region(),
    };
    let lines = report(Some(Ok(skypetoken))).lines(at(NOW));
    assert_eq!(
        lines,
        [
            "capture setup complete",
            "signed in: aud https://api.spaces.skype.com, exp Wed, 18 May 2033 05:43:20 GMT (in 130 min), scopes \
             Authorization.ReadWrite user_impersonation, tenant 00000000-0000-4000-8000-0000000000ff",
            "refreshed: aud https://api.spaces.skype.com, exp Wed, 18 May 2033 05:43:20 GMT (in 130 min), scopes \
             Authorization.ReadWrite user_impersonation, tenant 00000000-0000-4000-8000-0000000000ff",
            "https://graph.microsoft.com: aud https://graph.microsoft.com, exp Wed, 18 May 2033 03:31:20 GMT (2 min \
             ago), scopes User.Read",
            "https://outlook.office.com: refused: Conditional Access refused the token: invalid_grant AADSTS53003: \
             Access has been blocked by Conditional Access policies.",
            "https://ic3.teams.office.com: an opaque token (no claims to show)",
            "skypetoken: expires Thu, 19 May 2033 03:33:20 GMT (in 1440 min), region emea, partition emea01, 2 \
             regional endpoint(s)",
        ]
    );
}

#[test]
fn a_refused_skypetoken_is_reported_with_teams_error_code() {
    let refused = AuthzError::Refused {
        status: 403,
        code: Some("UserLicenseNotPresentTrialEligible".to_owned()),
    };
    let lines = report(Some(Err(refused))).lines(at(NOW));
    assert_eq!(
        lines.last().map(String::as_str),
        Some("skypetoken: Teams refused the account (authz answered 403: UserLicenseNotPresentTrialEligible)")
    );
    let entra_only = report(None).lines(at(NOW));
    assert!(
        entra_only.iter().all(|line| !line.starts_with("skypetoken")),
        "{entra_only:?}"
    );
}

#[test]
fn the_region_is_saved_privately_without_the_token() {
    let dir = tempfile::tempdir().unwrap();
    let paths = LabPaths::new(dir.path().join("lab"));
    let skypetoken = SkypetokenSummary {
        expires_at: at(NOW + 86_400),
        region: region(),
    };

    let file = save_region(&paths, Alias::B, &skypetoken).unwrap();

    assert_eq!(file, region_file(&paths, Alias::B));
    assert_eq!(file, dir.path().join("lab/auth/b.json"));
    let mode = |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&file), 0o600);
    assert_eq!(mode(file.parent().unwrap()), 0o700);
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(Region::from_json(&saved), Some(region()));
    assert_eq!(saved["account"], "b");
    assert_eq!(saved["skypetokenExpiresAt"], NOW + 86_400);
    // sorted: a workspace build may unify serde_json's `preserve_order`
    let keys: std::collections::BTreeSet<&str> = saved.as_object().unwrap().keys().map(String::as_str).collect();
    let expected = [
        "account",
        "fetchedAt",
        "partition",
        "region",
        "regionGtms",
        "skypetokenExpiresAt",
    ];
    assert_eq!(keys, expected.into_iter().collect());

    // saved again, it is replaced
    let later = SkypetokenSummary {
        expires_at: at(NOW + 90_000),
        region: Region::default(),
    };
    save_region(&paths, Alias::B, &later).unwrap();
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(saved["skypetokenExpiresAt"], NOW + 90_000);
    assert_eq!(
        std::fs::read_dir(file.parent().unwrap()).unwrap().count(),
        1,
        "no partial file left"
    );
}

#[test]
fn saves_that_run_at_once_each_replace_the_file_whole() {
    let dir = tempfile::tempdir().unwrap();
    let paths = LabPaths::new(dir.path().join("lab"));
    let expiries: Vec<u64> = (1..=16).map(|n| NOW + n * 1_000).collect();

    std::thread::scope(|scope| {
        let saves: Vec<_> = expiries
            .iter()
            .map(|&expiry| {
                let paths = &paths;
                scope.spawn(move || {
                    let skypetoken = SkypetokenSummary {
                        expires_at: at(expiry),
                        region: region(),
                    };
                    save_region(paths, Alias::C, &skypetoken)
                })
            })
            .collect();
        for save in saves {
            save.join().unwrap().unwrap();
        }
    });

    let file = region_file(&paths, Alias::C);
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert!(
        expiries.contains(&saved["skypetokenExpiresAt"].as_u64().unwrap()),
        "{saved}"
    );
    assert_eq!(Region::from_json(&saved), Some(region()));
    assert_eq!(
        std::fs::read_dir(file.parent().unwrap()).unwrap().count(),
        1,
        "no staging file left"
    );
}

#[tokio::test]
async fn the_timeout_bounds_a_step_that_never_answers() {
    // a navigation the browser never answers: the bound has to cover it, not only the wait after it
    let limit = Duration::from_millis(50);
    let started = std::time::Instant::now();
    let outcome = within(limit, std::future::pending::<Result<(), AuthError>>()).await;
    assert!(
        matches!(outcome, Err(AuthError::Timeout(given)) if given == limit),
        "{outcome:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(5));

    // a step that ends in time keeps its own outcome
    let done = within(Duration::from_secs(5), async { Ok::<_, AuthError>(7) }).await;
    assert!(matches!(done, Ok(7)), "{done:?}");
    let refused = within(Duration::from_secs(5), async {
        Err::<(), _>(AuthError::InteractionNeeded)
    })
    .await;
    assert!(matches!(refused, Err(AuthError::InteractionNeeded)), "{refused:?}");
}
