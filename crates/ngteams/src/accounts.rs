//! Accounts running side by side, with one shared call floor and device set.
//! Each engine owns its account's store and sends writes through its outbox.

use crate::accounts_file::PersonalAccount;
use teams_auth::secrets::{LazyKeyring, SecretStore};
use teams_core::CallFloor;
use teams_signin::window::Windows;

use crate::KEYRING_APPLICATION;
use crate::calling::{CallDevices, PersonalCalls};
use crate::engine::{self, PersonalEngine};
use crate::forget::{self, Forgot};
use crate::setup::Setup;
use crate::signin::{self, SignInClients};

/// An account's engine, started: over the desktop's Secret Service ([`LazyKeyring`]) in the app, over a test's store
/// in its tests.
pub struct Started<S = LazyKeyring> {
    pub account: PersonalAccount,
    pub engine: PersonalEngine<S>,
    /// The window's calls of the account, when the app's devices could be opened.
    pub calls: Option<PersonalCalls>,
}

impl<S> std::fmt::Debug for Started<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Started")
            .field("account", &self.account)
            .field("calls", &self.calls.is_some())
            .finish_non_exhaustive()
    }
}

/// What every account the app runs shares: the setup, the call floor, the calls' devices.
#[derive(Clone, Debug)]
pub struct Runner {
    pub setup: Setup,
    pub floor: CallFloor,
    /// The calls' devices; `None`: the app runs without calls.
    pub devices: Option<CallDevices>,
}

impl Runner {
    /// Starts `account`'s engine on the shared call floor and devices, with its push channel.
    ///
    /// # Errors
    /// Why it couldn't start, said for the user (another `ngteams` runs on the account, its region can't be read).
    pub fn start(&self, account: &PersonalAccount) -> Result<Started, String> {
        self.start_on(account, LazyKeyring::new(KEYRING_APPLICATION))
    }

    fn start_on<S: SecretStore + 'static>(&self, account: &PersonalAccount, secrets: S) -> Result<Started<S>, String> {
        let (engine_calls, calls) = self
            .devices
            .as_ref()
            .map(CallDevices::for_account)
            .map_or((None, None), |(engine, window)| (Some(engine), Some(window)));
        let engine = engine::start(
            &self.setup.paths,
            account,
            self.floor.clone(),
            secrets,
            true,
            engine_calls,
        )
        .map_err(|error| format!("{}: {error}", account.upn))?;
        Ok(Started {
            account: account.clone(),
            engine,
            calls,
        })
    }
}

/// A runtime of its own for a blocking sign-in or forget.
///
/// # Errors
/// When it can't be built.
pub fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("can't start the async runtime: {error}"))
}

/// Whether `account` can start without a sign-in ([`signin::ready`]).
///
/// # Errors
/// When the Secret Service can't be read.
pub fn ready(setup: &Setup, account: &PersonalAccount) -> Result<bool, String> {
    let store = LazyKeyring::new(KEYRING_APPLICATION);
    runtime()?
        .block_on(signin::ready(setup, account, &store))
        .map_err(|error| error.to_string())
}

/// Signs `account` in through the sign-in window `windows` opens (blocking: run it off GPUI's foreground), as
/// [`signin::sign_in`].
///
/// # Errors
/// Why the sign-in stored nothing, as the user is told it ([`signin::failure`]).
pub fn sign_in(setup: &Setup, account: &PersonalAccount, windows: &Windows) -> Result<PersonalAccount, String> {
    let store = LazyKeyring::new(KEYRING_APPLICATION);
    runtime()?.block_on(async {
        let clients = SignInClients::live().map_err(|error| error.to_string())?;
        let signed = match signin::window_for(setup, account, windows) {
            Ok(window) => signin::sign_in(setup, account, &clients, &store, window).await,
            Err(error) => Err(error),
        }
        .map_err(|error| signin::failure(setup, account, &error))?;
        log::info!("an account was signed in (its tenant matched)");
        Ok(signed)
    })
}

/// Adds the account `upn` (or takes up the entry held of it) and signs it in (blocking: run it off GPUI's foreground),
/// as [`signin::add`]: one that doesn't go through is taken out again only while its entry holds nothing but its UPN,
/// and kept when the window refused an identity provider's host, for the user to add to its `idp_hosts`.
///
/// # Errors
/// Why it wasn't added, as the user is told it ([`signin::failure`]).
pub fn add(setup: &Setup, upn: &str, windows: &Windows) -> Result<PersonalAccount, String> {
    let store = LazyKeyring::new(KEYRING_APPLICATION);
    runtime()?.block_on(async {
        let clients = SignInClients::live().map_err(|error| error.to_string())?;
        let window = |account: &PersonalAccount| signin::window_for(setup, account, windows);
        let signed = signin::add(setup, upn, &clients, &store, window).await?;
        log::info!("an account was added (its tenant matched)");
        Ok(signed)
    })
}

/// Forgets the account `upn` (blocking), as `ngteams --forget` does: refused while its engine runs.
///
/// # Errors
/// Why it wasn't forgotten.
pub fn forget(setup: &Setup, upn: &str) -> Result<Forgot, String> {
    let store = LazyKeyring::new(KEYRING_APPLICATION);
    runtime()?
        .block_on(forget::forget(setup, upn, &store))
        .map_err(|error| error.to_string())
}

#[cfg(test)]
#[path = "../tests/support/accounts.rs"]
mod tests;
