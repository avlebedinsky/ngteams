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
    #[error("no account yet: start `teams` and enter it in its window to sign in")]
    NoAccount,
    /// Another account than the one `account.toml` holds.
    #[error("Teams is set up for {held}: run `teams --forget` first to change accounts")]
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

    /// The account to run for: with `asked` (a `--account` UPN), what [`Self::ask_for`] makes of it, as the first
    /// sign-in screen would; without, `account.toml`'s. A lab account is refused either way.
    ///
    /// # Errors
    /// [`SetupError::NoAccount`] with neither; as [`Self::ask_for`] with `asked`; [`SetupError::Personal`] for a lab
    /// account or a file that can't be read.
    pub fn account(&self, asked: Option<&str>) -> Result<PersonalAccount, SetupError> {
        if let Some(asked) = asked {
            return self.ask_for(asked);
        }
        let held = PersonalAccount::load(&self.paths)?.ok_or(SetupError::NoAccount)?;
        held.check_not_lab(&self.lab)?;
        Ok(held)
    }

    /// The account `account.toml` holds once a sign-in named it (its object id known), else `None`: the first sign-in
    /// screen asks for it then ([`Self::ask_for`]). A lab account is refused.
    ///
    /// # Errors
    /// [`SetupError::Personal`] for a lab account or a file that can't be read.
    pub fn signed_in_account(&self) -> Result<Option<PersonalAccount>, SetupError> {
        match PersonalAccount::load(&self.paths)? {
            Some(held) if held.is_signed_in() => {
                held.check_not_lab(&self.lab)?;
                Ok(Some(held))
            }
            _ => Ok(None),
        }
    }

    /// The account an earlier attempt named, for the first sign-in screen to show again: `account.toml`'s UPN.
    ///
    /// # Errors
    /// [`SetupError::Personal`] when `account.toml` can't be read.
    pub fn held_upn(&self) -> Result<Option<String>, SetupError> {
        Ok(PersonalAccount::load(&self.paths)?.map(|held| held.upn))
    }

    /// The account the first sign-in screen (or `--account`) was given (`upn`), to sign in: `account.toml`'s when a
    /// sign-in named it (`upn` may only repeat it) or `upn` repeats it, else `upn` written to `account.toml` (`0600`)
    /// without an object id, in place of an account no sign-in named yet (whose `idp_hosts` it keeps). A lab account is
    /// refused.
    ///
    /// # Errors
    /// [`SetupError::OtherAccount`] when a signed-in account is held and `upn` differs; [`SetupError::Personal`] for a
    /// lab account, a bad UPN or a file that can't be read or written.
    pub fn ask_for(&self, upn: &str) -> Result<PersonalAccount, SetupError> {
        let held = PersonalAccount::load(&self.paths)?;
        if let Some(held) = held
            .as_ref()
            .filter(|held| held.is_signed_in() || held.upn.eq_ignore_ascii_case(upn))
        {
            if !held.upn.eq_ignore_ascii_case(upn) {
                return Err(SetupError::OtherAccount { held: held.upn.clone() });
            }
            held.check_not_lab(&self.lab)?;
            return Ok(held.clone());
        }
        let mut pending = PersonalAccount::pending(upn)?;
        pending.idp_hosts = held.map(|held| held.idp_hosts).unwrap_or_default();
        pending.check_not_lab(&self.lab)?;
        pending.save(&self.paths)?;
        Ok(pending)
    }
}
