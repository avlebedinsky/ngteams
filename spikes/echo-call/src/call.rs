//! Placing the echo call, step by step, as far as it can go today:
//!
//! 1. the call: lab account A (its MRI from `lab.toml`) places the test call for the approved echo service (the
//!    `echo` service participant's MRI from `lab.toml`, recorded from the Task 18 capture, or an MRI given on the
//!    command line). The call start names no callee; the echo is counted in because the caller adds it next;
//! 2. the fence: [`teams_fence::Fence::check`] on the call's target (`WriteKind::Call`, a new scope, the caller and
//!    the echo), and [`Authorized::new`] pairs the request with its permit. An echo given on the command line that
//!    `lab.toml` doesn't approve is denied here: only `lab.toml` approves;
//! 3. A's skypetoken from the secret store (`proto-lab auth a`);
//! 4. the call start: refused here on purpose ([`CallError::FollowUpsUnfenced`]). The echo joins only when the caller
//!    adds it to the started call ([`echo_invite`], an [`AddToCall`]), and that, the acceptance's acknowledgement and
//!    the `leave` are writes on a call that exists, which the fence can't permit while no source reads a call's
//!    members in full ([`authorize_invite`] is denied with every source a lab binary has;
//!    `docs/backlog/fence-call-follow-ups.md`). A call that could be neither completed nor left isn't placed.
//!
//! The fence comes first: if the write isn't allowed, no token is even read.

use teams_auth::authz::skypetoken_key;
use teams_auth::secrets::{Secret, SecretError, SecretStore};
use teams_fence::lab::{Alias, LabConfig};
use teams_fence::{Denied, FailClosed, Fence, MembershipSource, Mri};
use teams_proto::{AddToCall, Authorized, Mutating as _, StartCall, TargetMismatch};

/// The name of the echo service among `lab.toml`'s service participants.
pub const ECHO_SERVICE: &str = "echo";

/// Why the echo call stopped short. None of these sent anything to Microsoft.
#[derive(Debug, thiserror::Error)]
pub enum CallError {
    #[error("lab account {0} has no MRI in lab.toml yet (run `proto-lab login {0}`)")]
    NoCallerMri(Alias),
    #[error("the echo service has no MRI in lab.toml (record it with `proto-lab service-mri echo <mri>`)")]
    NoEchoMri,
    #[error(transparent)]
    Denied(#[from] Denied),
    #[error(transparent)]
    Mismatch(#[from] TargetMismatch),
    #[error("no skypetoken stored for {0} (run `proto-lab auth <alias>`)")]
    NoSkypetoken(String),
    #[error("the secret store failed: {0}")]
    Secrets(#[from] SecretError),
    #[error(
        "the call start is not sent: the echo joins only through an `add` on the started call, and the fence can't \
         permit writes on a call that exists (add, acknowledgement, leave) until it can read a call's members in \
         full (docs/backlog/fence-call-follow-ups.md)"
    )]
    FollowUpsUnfenced,
}

/// The test call of lab account `alias` for the echo service: `echo_override` when given, else the `echo` service MRI
/// from `config`.
///
/// # Errors
/// [`CallError::NoCallerMri`] or [`CallError::NoEchoMri`] when an MRI is missing.
pub fn echo_call(config: &LabConfig, alias: Alias, echo_override: Option<Mri>) -> Result<StartCall, CallError> {
    let from = config
        .account(alias)
        .mri
        .as_deref()
        .ok_or(CallError::NoCallerMri(alias))?;
    let echo = match echo_override {
        Some(echo) => echo,
        None => config
            .service_participants()
            .iter()
            .find(|service| service.name == ECHO_SERVICE)
            .and_then(|service| service.mri.as_deref())
            .map(Mri::new)
            .ok_or(CallError::NoEchoMri)?,
    };
    Ok(StartCall::test_call(Mri::new(from), echo))
}

/// Asks the fence for the call. A new call has no members to fetch, so the fail-closed source is all it needs.
///
/// # Errors
/// [`CallError::Denied`] when the caller isn't a lab account or the echo isn't an approved service.
pub async fn authorize(fence: &Fence, call: StartCall) -> Result<Authorized<StartCall>, CallError> {
    let permit = fence.check(call.target(), &FailClosed).await?;
    Ok(Authorized::new(call, permit)?)
}

/// The `add` that brings `call`'s echo into the conversation the start opened (`conversation`: its id from the
/// start's answer).
#[must_use]
pub fn echo_invite(call: &StartCall, conversation: &str) -> AddToCall {
    AddToCall::new(conversation, call.echo().clone())
}

/// Asks the fence for the `add`, which judges the call's members as `source` reads them. With the sources a lab binary
/// has, it is denied: [`FailClosed`] reads nothing, and `teams_fence::LiveMembershipSource` reads chats only.
///
/// # Errors
/// [`CallError::Denied`] while the call's members can't be read in full.
pub async fn authorize_invite(
    fence: &Fence,
    invite: AddToCall,
    source: &impl MembershipSource,
) -> Result<Authorized<AddToCall>, CallError> {
    let permit = fence.check(invite.target(), source).await?;
    Ok(Authorized::new(invite, permit)?)
}

/// `upn`'s stored skypetoken.
///
/// # Errors
/// [`CallError::NoSkypetoken`] when none is stored, [`CallError::Secrets`] when the store fails.
pub async fn skypetoken(store: &impl SecretStore, upn: &str) -> Result<Secret, CallError> {
    store
        .get(&skypetoken_key(upn))
        .await?
        .ok_or_else(|| CallError::NoSkypetoken(upn.to_owned()))
}

/// Starts the authorized call. Always refuses for now: see [`CallError::FollowUpsUnfenced`].
///
/// # Errors
/// [`CallError::FollowUpsUnfenced`], always.
pub fn start(_call: &Authorized<StartCall>, _skypetoken: &Secret) -> Result<(), CallError> {
    Err(CallError::FollowUpsUnfenced)
}

/// Steps 1–4 in order.
///
/// # Errors
/// The first step that stops: a missing MRI, the fence's denial of an echo `lab.toml` doesn't approve, the missing
/// skypetoken, then the call start that isn't sent ([`CallError::FollowUpsUnfenced`]).
pub async fn place_echo_call(
    config: &LabConfig,
    fence: &Fence,
    store: &impl SecretStore,
    alias: Alias,
    echo_override: Option<Mri>,
) -> Result<(), CallError> {
    let call = authorize(fence, echo_call(config, alias, echo_override)?).await?;
    let token = skypetoken(store, &config.account(alias).upn).await?;
    start(&call, &token)
}
