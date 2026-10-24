//! The personal build's files and the account it runs for, kept apart from the lab's (`docs/personal-build.md`, "How
//! the lab and personal binaries are kept apart"): its data dir must not be, contain or lie inside the lab's (the
//! default and `lab.toml`'s `data_dir`), and the account must not be one `lab.toml` names, by UPN or MRI. Nothing of
//! the lab is read but `lab.toml`, and nothing of it is written.

use teams_fence::lab::LabLocations;
use teams_fence::personal::{PersonalAccount, PersonalError, PersonalPaths};

/// Why the personal build can't run for an account here.
#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error(transparent)]
    Personal(#[from] PersonalError),
    #[error("HOME is unset or relative, so the lab's directories can't be found to keep apart from")]
    NoHome,
    /// No account yet, and none given.
    #[error("no account yet: run `teams --account <your UPN>` once to sign in")]
    NoAccount,
    /// Another account than the one `account.toml` holds.
    #[error("account.toml holds another account ({held}): run `teams --forget` first to change accounts")]
    OtherAccount { held: String },
}

/// The personal build's files and the lab's locations it is kept apart from.
#[derive(Clone, Debug)]
pub struct Setup {
    pub paths: PersonalPaths,
    pub lab: LabLocations,
}

impl Setup {
    /// From the environment, refused when the personal data dir overlaps the lab's.
    ///
    /// # Errors
    /// [`SetupError`] when a directory can't be found or the keep-apart check refuses.
    pub fn from_env() -> Result<Self, SetupError> {
        let paths = PersonalPaths::from_env()?;
        let lab = LabLocations::from_env().map_err(|_| SetupError::NoHome)?;
        Self::new(paths, lab)
    }

    /// `paths`, kept apart from `lab`.
    ///
    /// # Errors
    /// [`SetupError::Personal`] when they overlap or `lab.toml` can't be read.
    pub fn new(paths: PersonalPaths, lab: LabLocations) -> Result<Self, SetupError> {
        paths.check_apart(&lab)?;
        Ok(Self { paths, lab })
    }

    /// The account to run for: `account.toml`'s, which `asked` (a `--account` UPN) may only repeat, or, before the first
    /// sign-in, the one `asked` names, written to `account.toml` (`0600`) without an object id yet. A lab account is
    /// refused either way.
    ///
    /// # Errors
    /// [`SetupError::NoAccount`] with neither; [`SetupError::OtherAccount`] when `asked` differs from the account held;
    /// [`SetupError::Personal`] for a lab account, a bad UPN or a file that can't be read or written.
    pub fn account(&self, asked: Option<&str>) -> Result<PersonalAccount, SetupError> {
        let account = match (PersonalAccount::load(&self.paths)?, asked) {
            (Some(held), Some(asked)) if !held.upn.eq_ignore_ascii_case(asked) => {
                return Err(SetupError::OtherAccount { held: held.upn });
            }
            (Some(held), _) => held,
            (None, Some(asked)) => {
                let pending = PersonalAccount::pending(asked)?;
                pending.check_not_lab(&self.lab)?;
                pending.save(&self.paths)?;
                pending
            }
            (None, None) => return Err(SetupError::NoAccount),
        };
        account.check_not_lab(&self.lab)?;
        Ok(account)
    }
}
