//! Placing the echo call, step by step, as far as it can go today:
//!
//! 1. the participants: lab account A (its MRI from `lab.toml`) calls the approved echo service (the `echo` service
//!    participant's MRI from `lab.toml`, or an MRI given on the command line);
//! 2. the fence: [`teams_fence::Fence::check`] on the call's target (`WriteKind::Call`, a new scope, the caller and
//!    the callee), and [`Authorized::new`] pairs the request with its permit. The echo service's MRI isn't in
//!    `lab.toml` until Task 18's capture shows it, so the fence denies the call until then (a callee given on the
//!    command line is denied the same way: only `lab.toml` approves);
//! 3. A's skypetoken from the secret store (Task 14). None exists: authz refuses the unlicensed lab accounts;
//! 4. the call start through signaling: refused here on purpose, because the signaling is unconfirmed (Task 18), and
//!    nothing unconfirmed is sent to Microsoft.
//!
//! The fence comes first: if the write isn't allowed, no token is even read.

use teams_auth::authz::skypetoken_key;
use teams_auth::secrets::{Secret, SecretError, SecretStore};
use teams_fence::lab::{Alias, LabConfig};
use teams_fence::{Denied, FailClosed, Fence, Mri};
use teams_proto::{Authorized, Mutating as _, StartCall, TargetMismatch};

/// The name of the echo service among `lab.toml`'s service participants.
pub const ECHO_SERVICE: &str = "echo";

/// Why the echo call stopped short. None of these sent anything to Microsoft.
#[derive(Debug, thiserror::Error)]
pub enum CallError {
    #[error("lab account {0} has no MRI in lab.toml yet (run `proto-lab login {0}`)")]
    NoCallerMri(Alias),
    #[error(
        "the echo service has no MRI in lab.toml yet: Task 18's capture fills it in (blocked: the lab accounts have no \
         Teams license)"
    )]
    NoEchoMri,
    #[error(transparent)]
    Denied(#[from] Denied),
    #[error(transparent)]
    Mismatch(#[from] TargetMismatch),
    #[error("no skypetoken stored for {0} (blocked: authz refuses the unlicensed lab accounts)")]
    NoSkypetoken(String),
    #[error("the secret store failed: {0}")]
    Secrets(#[from] SecretError),
    #[error(
        "the call start is not sent: the signaling is unconfirmed until Task 18 captures a real echo call \
         (docs/spikes/echo-call.md)"
    )]
    SignalingUnconfirmed,
}

/// The call from lab account `alias` to the echo service: `echo_override` when given, else the `echo` service MRI from `config`.
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
    Ok(StartCall::new(Mri::new(from), [echo]))
}

/// Asks the fence for the call. A new call has no members to fetch, so the fail-closed source is all it needs.
///
/// # Errors
/// [`CallError::Denied`] when a participant isn't a lab account or an approved service.
pub async fn authorize(fence: &Fence, call: StartCall) -> Result<Authorized<StartCall>, CallError> {
    let permit = fence.check(call.target(), &FailClosed).await?;
    Ok(Authorized::new(call, permit)?)
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

/// Starts the authorized call. Always refuses for now: see [`CallError::SignalingUnconfirmed`].
///
/// # Errors
/// [`CallError::SignalingUnconfirmed`], always.
pub fn start(_call: &Authorized<StartCall>, _skypetoken: &Secret) -> Result<(), CallError> {
    Err(CallError::SignalingUnconfirmed)
}

/// Steps 1–4 in order.
///
/// # Errors
/// The first step that stops: today the fence's denial (no echo MRI approved), then the missing skypetoken, then the
/// unconfirmed signaling.
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
