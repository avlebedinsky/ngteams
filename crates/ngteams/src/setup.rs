//! The app's account files and setup, including legacy single-account migration.
//!
//! The accounts are `accounts.toml`'s, in the order the user added them: each one given (the first run's screen, the
//! window's "add account", `--account`) is written there at once without an object id ([`Setup::ask_for`]), and its
//! first sign-in adds the object id and tenant (`crate::signin`); one whose sign-in never went through can be taken
//! out again while it holds nothing but its UPN ([`Setup::abandon`]): one the user wrote more into (its `idp_hosts`)
//! stays, a pending entry the next sign-in of that UPN takes up, never an account the app starts. A build of one account's `account.toml` and `region.json` is moved into these files
//! when the setup is made (`PersonalPaths::migrate_single_account`).

use crate::accounts_file::{OwnServer, PersonalAccount, PersonalError, PersonalPaths};
use crate::secrets::AppSecrets;

/// Why the personal build can't run for an account here.
#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error(transparent)]
    Personal(#[from] PersonalError),
}

/// The app's account files.
#[derive(Clone, Debug)]
pub struct Setup {
    pub paths: PersonalPaths,
    secrets: AppSecrets,
    own_servers: bool,
}

/// The variable that turns own-server accounts on (`NGTEAMS_OWN_SERVER=1`) while they are being built.
pub const OWN_SERVER_ENV: &str = "NGTEAMS_OWN_SERVER";

/// Whether this process was started with own-server accounts on ([`OWN_SERVER_ENV`]).
#[must_use]
pub fn own_servers_from_env() -> bool {
    std::env::var_os(OWN_SERVER_ENV).is_some_and(|value| value == "1")
}

impl Setup {
    /// From the environment, migrating any earlier single-account files.
    ///
    /// # Errors
    /// When the default paths cannot be resolved or the account files cannot be moved.
    pub fn from_env() -> Result<Self, SetupError> {
        Self::new(PersonalPaths::from_env()?)
    }

    /// Uses `paths`, migrating earlier single-account files (see the module docs).
    ///
    /// # Errors
    /// When the files cannot be read or moved.
    pub fn new(paths: PersonalPaths) -> Result<Self, SetupError> {
        paths.migrate_single_account()?;
        Ok(Self {
            paths,
            secrets: AppSecrets::personal(),
            own_servers: false,
        })
    }

    /// Whether own-server accounts are offered, signed in and started (off unless asked for).
    #[must_use]
    pub fn with_own_servers(mut self, own_servers: bool) -> Self {
        self.own_servers = own_servers;
        self
    }

    /// Whether own-server accounts are on ([`Self::with_own_servers`]).
    #[must_use]
    pub fn own_servers(&self) -> bool {
        self.own_servers
    }

    /// Uses the startup-selected credential backend for every account operation.
    #[must_use]
    pub fn with_secrets(mut self, secrets: AppSecrets) -> Self {
        self.secrets = secrets;
        self
    }

    /// A handle to the selected store, including token renewal and account removal.
    #[must_use]
    pub fn secrets(&self) -> AppSecrets {
        self.secrets.clone()
    }

    /// Every account, in the order added, signed in or not.
    ///
    /// # Errors
    /// When the account file cannot be read or parsed.
    pub fn accounts(&self) -> Result<Vec<PersonalAccount>, SetupError> {
        Ok(PersonalAccount::load_all(&self.paths)?)
    }

    /// The accounts a sign-in named (their object id and tenant known), in the order added: those the app may start an
    /// engine for once their tokens are there (`crate::signin::ready`).
    ///
    /// # Errors
    /// As [`Self::accounts`].
    pub fn signed_in_accounts(&self) -> Result<Vec<PersonalAccount>, SetupError> {
        let mut accounts = self.accounts()?;
        // an own server's account stays in the file, unstarted, while own servers are off
        accounts.retain(|account| account.is_signed_in() && (self.own_servers || account.server.is_none()));
        Ok(accounts)
    }

    /// The UPNs of the accounts no sign-in named yet, in the order given: the first run's screen shows the last one
    /// again.
    ///
    /// # Errors
    /// [`SetupError::Personal`] when `accounts.toml` can't be read.
    pub fn pending_upns(&self) -> Result<Vec<String>, SetupError> {
        Ok(PersonalAccount::load_all(&self.paths)?
            .into_iter()
            .filter(|account| !account.is_signed_in() && account.server.is_none())
            .map(|account| account.upn)
            .collect())
    }

    /// The account `upn` (case aside), to sign in: the one `accounts.toml` holds of that UPN, signed in or not, else
    /// `upn` added after the others without an object id (`0600`).
    ///
    /// # Errors
    /// [`SetupError::Personal`] for a bad UPN or a file that can't be read or written.
    pub fn ask_for(&self, upn: &str) -> Result<PersonalAccount, SetupError> {
        if let Some(held) = PersonalAccount::find(&self.paths, upn)? {
            return Ok(held);
        }
        let pending = PersonalAccount::pending(upn)?;
        pending.save(&self.paths)?;
        Ok(pending)
    }

    /// The own server `server`'s account `upn`, to sign in: the one `accounts.toml` holds of that server and UPN (with
    /// whatever the user wrote into it, its `ca_file` say; a CA file given now replaces the entry's), else added after
    /// the others without an object id.
    ///
    /// # Errors
    /// [`SetupError::Personal`] while own servers are off, for a bad UPN or server, or a file that can't be read or
    /// written.
    pub fn ask_for_on(&self, upn: &str, server: &OwnServer) -> Result<PersonalAccount, SetupError> {
        if !self.own_servers {
            return Err(PersonalError::Account(format!("own servers are off (set {OWN_SERVER_ENV}=1)")).into());
        }
        let pending = PersonalAccount::pending_on(upn, server)?;
        match PersonalAccount::find(&self.paths, &pending.id())? {
            Some(held) if server.ca_file.is_none() || held.ca_file == server.ca_file => Ok(held),
            Some(held) => {
                let held = PersonalAccount {
                    ca_file: server.ca_file.clone(),
                    ..held
                };
                held.save(&self.paths)?;
                Ok(held)
            }
            None => {
                pending.save(&self.paths)?;
                Ok(pending)
            }
        }
    }

    /// Takes the account `upn` (case aside) out of `accounts.toml` when no sign-in ever named it and it holds nothing
    /// but its UPN (as [`Self::ask_for`] wrote it), so an account added whose sign-in stored nothing leaves nothing
    /// behind. An entry the user wrote more into (its `idp_hosts`, by hand) stays pending for the next sign-in of that
    /// UPN, and an account a sign-in named stays (`crate::forget` forgets either). Whether one was taken out.
    ///
    /// # Errors
    /// [`SetupError::Personal`] when `accounts.toml` can't be read or written.
    pub fn abandon(&self, upn: &str) -> Result<bool, SetupError> {
        match PersonalAccount::find(&self.paths, upn)? {
            Some(held) if is_bare(&held) => Ok(PersonalAccount::remove(&self.paths, upn)?.is_some()),
            _ => Ok(false),
        }
    }
}

/// Whether `account` holds nothing but its UPN: no sign-in named it and nothing was written into it by hand.
fn is_bare(account: &PersonalAccount) -> bool {
    !account.is_signed_in() && account.name.is_none() && account.idp_hosts.is_empty() && account.ca_file.is_none()
}
