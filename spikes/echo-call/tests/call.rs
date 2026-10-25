//! The echo call's flow up to the skypetoken, on a lab config in a temp dir: the call goes through the fence and is
//! denied until `lab.toml` approves the echo service's MRI; a token is read only after the fence allowed the call; and
//! even then nothing is sent, because the `add` that brings the echo in is a write on the started call that the fence
//! can't permit yet.

use std::path::Path;

use echo_call::call::{CallError, authorize, authorize_invite, echo_call, echo_invite, place_echo_call};
use teams_auth::authz::skypetoken_key;
use teams_auth::secrets::{MemorySecretStore, Secret, SecretStore as _};
use teams_fence::lab::{Alias, LabConfig};
use teams_fence::{DenyReason, FailClosed, Fence, LiveMembershipSource, Mri, PartialReason, Scope, WriteKind};
use teams_proto::{ConversationUrl, Mutating as _, StartCall};

const LAB_A: &str = "8:orgid:lab-a";
const ECHO: &str = "28:echo-service";
const UPN_A: &str = "bot-a@lab.example.com";
/// A placeholder conversation controller.
const CALL: &str = "https://proxy.example.com/api/v2/ep/conv-1.example.com/conv/Conversation1?i=1&e=1";

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
    let call = echo_call(&lab.config, Alias::A).unwrap();
    assert_eq!(call.caller(), &Mri::new(LAB_A));
    assert_eq!(call.echo(), Some(&Mri::new(ECHO)));
    assert_eq!(call.target().kind(), WriteKind::Call);
}

#[test]
fn without_an_echo_mri_in_lab_toml_there_is_no_call() {
    let lab = lab(None, Some(LAB_A)).unwrap();
    assert!(matches!(echo_call(&lab.config, Alias::A), Err(CallError::NoEchoMri)));
    let no_caller = self::lab(Some(ECHO), None).unwrap();
    assert!(matches!(
        echo_call(&no_caller.config, Alias::A),
        Err(CallError::NoCallerMri(Alias::A))
    ));
}

#[tokio::test]
async fn without_lab_tomls_echo_nothing_reaches_the_fence_and_another_echo_is_denied() {
    // today's lab.toml: the echo service is approved by name, its MRI not filled in yet
    let lab = lab(None, Some(LAB_A)).unwrap();
    let store = store_with_token().await.unwrap();
    let error = place_echo_call(&lab.config, &lab.fence, &store, Alias::A)
        .await
        .unwrap_err();
    assert!(matches!(error, CallError::NoEchoMri), "{error}");
    assert!(!lab.fence_log().contains("\"call\""), "no call was judged");

    // a test call for an echo the roster doesn't approve, made by hand: the fence denies it
    let call = StartCall::test_call(Mri::new(LAB_A), Mri::new(ECHO));
    let error = authorize(&lab.fence, call).await.unwrap_err();
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
    let error = place_echo_call(&lab.config, &lab.fence, &MemorySecretStore::new(), Alias::A)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, CallError::NoSkypetoken(upn) if upn == UPN_A),
        "{error}"
    );
    assert!(lab.fence_log().contains("allow"), "the fence allowed the call first");
}

#[tokio::test]
async fn with_a_skypetoken_the_call_start_is_still_not_sent() {
    let lab = lab(Some(ECHO), Some(LAB_A)).unwrap();
    let store = store_with_token().await.unwrap();
    let error = place_echo_call(&lab.config, &lab.fence, &store, Alias::A)
        .await
        .unwrap_err();
    assert!(matches!(error, CallError::FollowUpsUnfenced), "{error}");
    assert!(error.to_string().contains("fence-call-follow-ups"));
}

#[tokio::test]
async fn the_permit_covers_exactly_the_call_it_was_issued_for() {
    let lab = lab(Some(ECHO), Some(LAB_A)).unwrap();
    let call = echo_call(&lab.config, Alias::A).unwrap();
    let authorized = authorize(&lab.fence, call.clone()).await.unwrap();
    assert_eq!(authorized.permit().target(), &call.target());
}

#[tokio::test]
async fn adding_the_echo_to_the_started_call_is_denied_with_every_source_a_lab_binary_has() {
    let lab = lab(Some(ECHO), Some(LAB_A)).unwrap();
    let call = authorize(&lab.fence, echo_call(&lab.config, Alias::A).unwrap())
        .await
        .unwrap();
    let invite = echo_invite(&call, ConversationUrl::parse(CALL).unwrap()).unwrap();
    assert_eq!(invite.target().scope(), &Scope::Call { id: CALL.into() });
    assert_eq!(invite.target().kind(), WriteKind::Invite);
    assert_eq!(
        Some(invite.invitee()),
        call.request().echo(),
        "the permitted call's echo"
    );

    let error = authorize_invite(&lab.fence, invite.clone(), &FailClosed)
        .await
        .unwrap_err();
    let CallError::Denied(denied) = &error else {
        panic!("expected the fence to deny, got {error}");
    };
    assert_eq!(denied.reason(), &DenyReason::Partial(PartialReason::NoSource));

    // the live source reads chats only: a call's members aren't read (and nothing goes out for it)
    let live = LiveMembershipSource::new().unwrap();
    let error = authorize_invite(&lab.fence, invite, &live).await.unwrap_err();
    let CallError::Denied(denied) = &error else {
        panic!("expected the fence to deny, got {error}");
    };
    assert_eq!(denied.reason(), &DenyReason::Partial(PartialReason::Unsupported));
}
