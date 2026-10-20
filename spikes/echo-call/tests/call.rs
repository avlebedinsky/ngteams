//! The echo call's flow up to the skypetoken, on a lab config in a temp dir: the call goes through the fence and is
//! denied until `lab.toml` approves the echo service's MRI; a token is read only after the fence allowed the call; and
//! even then the unconfirmed signaling sends nothing.

use std::path::Path;

use echo_call::call::{CallError, authorize, echo_call, place_echo_call};
use teams_auth::authz::skypetoken_key;
use teams_auth::secrets::{MemorySecretStore, Secret, SecretStore as _};
use teams_fence::lab::{Alias, LabConfig};
use teams_fence::{DenyReason, Fence, Mri};
use teams_proto::Mutating as _;

const LAB_A: &str = "8:orgid:lab-a";
const ECHO: &str = "28:echo-service";
const UPN_A: &str = "bot-a@lab.example.com";

fn lab_toml(data_dir: &Path, echo_mri: Option<&str>, a_mri: Option<&str>) -> String {
    let mri = |value: Option<&str>| value.map_or_else(String::new, |mri| format!("mri = \"{mri}\"\n"));
    format!(
        "data_dir = \"{}\"\n\
         [[accounts]]\nalias = \"a\"\nupn = \"{UPN_A}\"\n{}\
         [[accounts]]\nalias = \"b\"\nupn = \"bot-b@lab.example.com\"\n\
         [[accounts]]\nalias = \"c\"\nupn = \"bot-c@lab.example.com\"\n\
         [[service_participants]]\nname = \"echo\"\n{}",
        data_dir.display(),
        mri(a_mri),
        mri(echo_mri),
    )
}

struct Lab {
    dir: tempfile::TempDir,
    config: LabConfig,
    fence: Fence,
}

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

fn lab(echo_mri: Option<&str>, a_mri: Option<&str>) -> TestResult<Lab> {
    let dir = tempfile::tempdir()?;
    let config = LabConfig::parse(&lab_toml(dir.path(), echo_mri, a_mri), dir.path())?;
    let fence = Fence::open(&config)?;
    Ok(Lab { dir, config, fence })
}

impl Lab {
    fn fence_log(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("fence.log")).unwrap_or_default()
    }
}

async fn store_with_token() -> TestResult<MemorySecretStore> {
    let store = MemorySecretStore::new();
    store
        .put(&skypetoken_key(UPN_A), &Secret::new("not-a-real-skypetoken"))
        .await?;
    Ok(store)
}

#[test]
fn the_call_is_from_account_a_to_the_echo_service_of_lab_toml() {
    let lab = lab(Some(ECHO), Some(LAB_A)).unwrap();
    let call = echo_call(&lab.config, Alias::A, None).unwrap();
    assert_eq!(call.caller(), &Mri::new(LAB_A));
    assert_eq!(call.callees().iter().collect::<Vec<_>>(), [&Mri::new(ECHO)]);
    assert_eq!(call.target().kind(), teams_fence::WriteKind::Call);
}

#[test]
fn without_an_echo_mri_in_lab_toml_there_is_no_call() {
    let lab = lab(None, Some(LAB_A)).unwrap();
    assert!(matches!(
        echo_call(&lab.config, Alias::A, None),
        Err(CallError::NoEchoMri)
    ));
    let no_caller = self::lab(Some(ECHO), None).unwrap();
    assert!(matches!(
        echo_call(&no_caller.config, Alias::A, None),
        Err(CallError::NoCallerMri(Alias::A))
    ));
}

#[tokio::test]
async fn a_callee_lab_toml_does_not_approve_is_denied_by_the_fence() {
    // today's lab.toml: the echo service is approved by name, its MRI not filled in yet
    let lab = lab(None, Some(LAB_A)).unwrap();
    let store = store_with_token().await.unwrap();
    let error = place_echo_call(&lab.config, &lab.fence, &store, Alias::A, Some(Mri::new(ECHO)))
        .await
        .unwrap_err();
    let CallError::Denied(denied) = &error else {
        panic!("expected the fence to deny, got {error}");
    };
    assert_eq!(denied.reason(), &DenyReason::Outsiders { count: 1 });
    let log = lab.fence_log();
    assert!(
        log.contains("\"call\"") && log.contains("deny"),
        "the denial is logged: {log}"
    );
}

#[tokio::test]
async fn an_approved_call_still_needs_a_skypetoken() {
    let lab = lab(Some(ECHO), Some(LAB_A)).unwrap();
    let error = place_echo_call(&lab.config, &lab.fence, &MemorySecretStore::new(), Alias::A, None)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, CallError::NoSkypetoken(upn) if upn == UPN_A),
        "{error}"
    );
    assert!(lab.fence_log().contains("allow"), "the fence allowed the call first");
}

#[tokio::test]
async fn with_a_skypetoken_the_unconfirmed_signaling_sends_nothing() {
    let lab = lab(Some(ECHO), Some(LAB_A)).unwrap();
    let store = store_with_token().await.unwrap();
    let error = place_echo_call(&lab.config, &lab.fence, &store, Alias::A, None)
        .await
        .unwrap_err();
    assert!(matches!(error, CallError::SignalingUnconfirmed), "{error}");
    assert!(error.to_string().contains("Task 18"));
}

#[tokio::test]
async fn the_permit_covers_exactly_the_call_it_was_issued_for() {
    let lab = lab(Some(ECHO), Some(LAB_A)).unwrap();
    let call = echo_call(&lab.config, Alias::A, None).unwrap();
    let authorized = authorize(&lab.fence, call.clone()).await.unwrap();
    assert_eq!(authorized.permit().target(), &call.target());
}
