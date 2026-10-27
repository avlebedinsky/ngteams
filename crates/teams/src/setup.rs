//! The personal build's files and the accounts it runs for, kept apart from the lab's (`docs/personal-build.md`, "How
//! the lab and personal binaries are kept apart"): its data dir must not be, contain or lie inside the lab's (the
//! default and `lab.toml`'s `data_dir`), and no account may be one `lab.toml` names, by UPN or MRI. Nothing of the lab
//! is read but `lab.toml`, and nothing of it is written.
//!
//! The accounts are `accounts.toml`'s, in the order the user added them: each one given (the first run's screen, the
//! window's "add account", `--account`) is written there at once without an object id ([`Setup::ask_for`]), and its
//! first sign-in adds the object id and tenant (`crate::signin`); one whose sign-in never went through can be taken
//! out again ([`Setup::abandon`]). A build of one account's `account.toml` and `region.json` is moved into these files
//! when the setup is made (`PersonalPaths::migrate_single_account`).

use teams_fence::lab::LabLocations;
use teams_fence::personal::{PersonalAccount, PersonalError, PersonalPaths};

/// Why the personal build can't run for an account here.
#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error(transparent)]
    Personal(#[from] PersonalError),
    #[error("HOME is unset or relative, so the lab's directories can't be found to keep apart from")]
    NoHome,
}

/// The personal build's files and the lab's locations it is kept apart from.
#[derive(Clone, Debug)]
pub struct Setup {
    pub paths: PersonalPaths,
    pub lab: LabLocations,
}

impl Setup {
    /// From the environment, refused when the personal data dir overlaps the lab's ([`Self::new`]).
    ///
    /// # Errors
    /// [`SetupError`] when a directory can't be found, the keep-apart check refuses or a single account's files can't
    /// be moved.
    pub fn from_env() -> Result<Self, SetupError> {
        let paths = PersonalPaths::from_env()?;
        let lab = LabLocations::from_env().map_err(|_| SetupError::NoHome)?;
        Self::new(paths, lab)
    }

    /// `paths`, kept apart from `lab`; a single account's files of an earlier build then moved into the files of
    /// several (see the module docs).
    ///
    /// # Errors
    /// [`SetupError::Personal`] when they overlap, `lab.toml` can't be read, or the move fails.
    pub fn new(paths: PersonalPaths, lab: LabLocations) -> Result<Self, SetupError> {
        paths.check_apart(&lab)?;
        paths.migrate_single_account()?;
        Ok(Self { paths, lab })
    }

    /// Every account, in the order added, signed in or not. A lab account among them is refused.
    ///
    /// # Errors
    /// [`SetupError::Personal`] for a lab account or a file that can't be read.
    pub fn accounts(&self) -> Result<Vec<PersonalAccount>, SetupError> {
        let accounts = PersonalAccount::load_all(&self.paths)?;
        for account in &accounts {
            account.check_not_lab(&self.lab)?;
        }
        Ok(accounts)
    }

    /// The accounts a sign-in named (their object id and tenant known), in the order added: those the app may start an
    /// engine for once their tokens are there (`crate::signin::ready`). A lab account among them is refused.
    ///
    /// # Errors
    /// As [`Self::accounts`].
    pub fn signed_in_accounts(&self) -> Result<Vec<PersonalAccount>, SetupError> {
        let mut accounts = self.accounts()?;
        accounts.retain(PersonalAccount::is_signed_in);
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
            .filter(|account| !account.is_signed_in())
            .map(|account| account.upn)
            .collect())
    }

    /// The account `upn` (case aside), to sign in: the one `accounts.toml` holds of that UPN, signed in or not, else
    /// `upn` added after the others without an object id (`0600`). A lab account is refused, before anything is
    /// written.
    ///
    /// # Errors
    /// [`SetupError::Personal`] for a lab account, a bad UPN or a file that can't be read or written.
    pub fn ask_for(&self, upn: &str) -> Result<PersonalAccount, SetupError> {
        if let Some(held) = PersonalAccount::find(&self.paths, upn)? {
            held.check_not_lab(&self.lab)?;
            return Ok(held);
        }
        let pending = PersonalAccount::pending(upn)?;
        pending.check_not_lab(&self.lab)?;
        pending.save(&self.paths)?;
        Ok(pending)
    }

    /// Takes the account `upn` (case aside) out of `accounts.toml` when no sign-in ever named it, so an account added
    /// whose sign-in stored nothing leaves nothing behind; an account a sign-in named stays (`crate::forget` forgets
    /// one). Whether one was taken out.
    ///
    /// # Errors
    /// [`SetupError::Personal`] when `accounts.toml` can't be read or written.
    pub fn abandon(&self, upn: &str) -> Result<bool, SetupError> {
        match PersonalAccount::find(&self.paths, upn)? {
            Some(held) if !held.is_signed_in() => Ok(PersonalAccount::remove(&self.paths, upn)?.is_some()),
            _ => Ok(false),
        }
    }
}
