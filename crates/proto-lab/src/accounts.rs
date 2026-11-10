//! The lab accounts' sign-in credentials, from the lab's account file ([`accounts_file`], `accounts.json` beside
//! `lab.toml`), read by these rules:
//! - the file must not be readable or writable by group or others (mode `0600` or stricter);
//! - it is a JSON array of accounts, or an object whose `accounts` is one, and the list isn't empty;
//! - each account has a `profile` (`a`, `b` or `c`, the lab alias), each profile at most once, an `email` of the form
//!   `user@domain.tld`, and a password: the environment variable `passwordEnv` names, or else `password`; an unset or
//!   empty one is an error.
//!
//! Other fields are ignored.
//!
//! The email of a profile has to be that account's UPN in `lab.toml`, so a password is only ever typed for the account
//! it belongs to.
//!
//! A password is never printed, logged or put in an error: [`Password`] has a redacted `Debug` and is wiped from memory
//! when dropped, no error carries a value from the file, and the JSON is read as plain values first, so a type error
//! can't quote what it found.

use std::collections::BTreeSet;
use std::fmt;
use std::io::Read as _;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};
use zeroize::Zeroizing;

use crate::config::{Alias, LabLocations};

/// The lab's account file: `accounts.json` in the directory of `lab.toml`.
#[must_use]
pub fn accounts_file(locations: &LabLocations) -> PathBuf {
    locations.config_file.with_file_name("accounts.json")
}

/// The profile of each lab account, as the account file names it.
const PROFILES: [(&str, Alias); 3] = [("a", Alias::A), ("b", Alias::B), ("c", Alias::C)];

/// The loader's email check.
static EMAIL: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}$").ok());

/// A password. Its `Debug` shows nothing of it, and its memory is wiped when it is dropped.
pub struct Password(Zeroizing<String>);

impl Password {
    /// The password itself, to type into the sign-in page and nowhere else.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password(<redacted>)")
    }
}

/// One lab account's sign-in.
#[derive(Debug)]
pub struct Credentials {
    email: String,
    password: Password,
}

impl Credentials {
    #[must_use]
    pub fn email(&self) -> &str {
        &self.email
    }

    #[must_use]
    pub fn password(&self) -> &Password {
        &self.password
    }
}

/// Why the account file was refused. No variant carries anything read from the file but positions and names.
#[derive(Debug, thiserror::Error)]
pub enum AccountsError {
    #[error("can't read {}: {source}", path.display())]
    Read { path: PathBuf, source: std::io::Error },
    #[error("{} must not be readable by group or others (mode {mode:o}); run: chmod 600 {}", path.display(), path.display())]
    Mode { path: PathBuf, mode: u32 },
    /// Only where the JSON broke, never what was there.
    #[error("the account file isn't valid JSON (line {line}, column {column})")]
    Json { line: usize, column: usize },
    #[error("the account file must contain a non-empty accounts array")]
    NoAccounts,
    #[error("account {index} has an unsupported profile")]
    UnsupportedProfile { index: usize },
    #[error("profile {profile} appears more than once")]
    DuplicateProfile { profile: &'static str },
    #[error("account {index} has an invalid email")]
    InvalidEmail { index: usize },
    /// `from` is `password`, or `environment variable <name>`.
    #[error("account {index} has no {from}")]
    NoPassword { index: usize, from: String },
    #[error("the account file has no account for lab account {0}")]
    NotListed(Alias),
    #[error("the account file's email for lab account {0} isn't its UPN in lab.toml")]
    EmailMismatch(Alias),
}

struct Entry {
    alias: Alias,
    credentials: Credentials,
}

/// The lab accounts found in the account file, validated.
pub struct AccountsFile {
    entries: Vec<Entry>,
}

impl fmt::Debug for AccountsFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.entries.iter().map(|entry| (entry.alias, &entry.credentials)))
            .finish()
    }
}

impl AccountsFile {
    /// Reads and validates the account file at `path`. `env` looks up the variables `passwordEnv` names
    /// (`|name| std::env::var(name).ok()` for the real environment).
    ///
    /// # Errors
    /// When the file can't be read, is readable by group or others, or breaks a rule (see the module docs).
    pub fn load(path: &Path, env: impl Fn(&str) -> Option<String>) -> Result<Self, AccountsError> {
        let read_error = |source| AccountsError::Read {
            path: path.to_path_buf(),
            source,
        };
        let mut file = std::fs::File::open(path).map_err(read_error)?;
        // the mode of the file opened, not of whatever the path names a moment later
        #[cfg(unix)]
        {
            let mode = file.metadata().map_err(read_error)?.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                return Err(AccountsError::Mode {
                    path: path.to_path_buf(),
                    mode,
                });
            }
        }
        #[cfg(windows)]
        teams_platform::private::check_file(&file).map_err(read_error)?;
        let mut text = Zeroizing::new(String::new());
        file.read_to_string(&mut text).map_err(read_error)?;
        Self::parse(&text, env)
    }

    /// Validates the account file's text.
    ///
    /// # Errors
    /// When it breaks a rule (see the module docs).
    pub fn parse(text: &str, env: impl Fn(&str) -> Option<String>) -> Result<Self, AccountsError> {
        let parsed: Value = serde_json::from_str(text).map_err(|error| AccountsError::Json {
            line: error.line(),
            column: error.column(),
        })?;
        let accounts = match parsed {
            Value::Array(accounts) => accounts,
            Value::Object(mut object) => match object.remove("accounts") {
                Some(Value::Array(accounts)) => accounts,
                _ => return Err(AccountsError::NoAccounts),
            },
            _ => return Err(AccountsError::NoAccounts),
        };
        if accounts.is_empty() {
            return Err(AccountsError::NoAccounts);
        }
        let mut seen = BTreeSet::new();
        let mut entries = Vec::with_capacity(accounts.len());
        for (index, account) in (1..).zip(accounts) {
            let entry = read_account(index, account, &env)?;
            if !seen.insert(entry.alias) {
                return Err(AccountsError::DuplicateProfile {
                    profile: profile_name(entry.alias),
                });
            }
            entries.push(entry);
        }
        Ok(Self { entries })
    }

    /// The credentials of lab account `alias`, whose UPN in `lab.toml` is `upn`.
    ///
    /// # Errors
    /// [`AccountsError::NotListed`] when the file has no account for it, [`AccountsError::EmailMismatch`] when its email
    /// isn't `upn` (case aside).
    pub fn for_alias(&self, alias: Alias, upn: &str) -> Result<&Credentials, AccountsError> {
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.alias == alias)
            .ok_or(AccountsError::NotListed(alias))?;
        if !entry.credentials.email.eq_ignore_ascii_case(upn) {
            return Err(AccountsError::EmailMismatch(alias));
        }
        Ok(&entry.credentials)
    }
}

fn profile_name(alias: Alias) -> &'static str {
    PROFILES
        .iter()
        .find(|(_, of)| *of == alias)
        .map_or("?", |(name, _)| name)
}

/// One account of the file, by the rules above. `index` counts from 1, as the loader's messages do.
fn read_account(index: usize, account: Value, env: &impl Fn(&str) -> Option<String>) -> Result<Entry, AccountsError> {
    let Value::Object(mut account) = account else {
        return Err(AccountsError::UnsupportedProfile { index });
    };
    let profile = text_of(account.get("profile"));
    let profile = profile.trim();
    let alias = PROFILES
        .iter()
        .find(|(name, _)| *name == profile)
        .map(|(_, alias)| *alias)
        .ok_or(AccountsError::UnsupportedProfile { index })?;
    let email = text_of(account.get("email")).trim().to_owned();
    if !EMAIL.as_ref().is_some_and(|pattern| pattern.is_match(&email)) {
        return Err(AccountsError::InvalidEmail { index });
    }
    let password = password_of(index, &mut account, env)?;
    Ok(Entry {
        alias,
        credentials: Credentials { email, password },
    })
}

/// The loader's `String(value ?? "")` for the fields it reads as text: a string as it is, a number or a bool as
/// written, anything else (missing, null, a list, an object) as nothing.
fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(flag)) => flag.to_string(),
        _ => String::new(),
    }
}

/// `passwordEnv`'s variable when the field is set, else `password`. The string is moved out of the parsed file, so
/// the only copy left is the one [`Password`] wipes.
fn password_of(
    index: usize,
    account: &mut Map<String, Value>,
    env: &impl Fn(&str) -> Option<String>,
) -> Result<Password, AccountsError> {
    // taken out whether or not it is used, so it is wiped either way
    let literal = match account.remove("password") {
        Some(Value::String(password)) => Some(Zeroizing::new(password)),
        _ => None,
    };
    // the loader's truthiness test on `passwordEnv`
    let variable = account
        .get("passwordEnv")
        .filter(|name| !matches!(name, Value::Null | Value::Bool(false)) && *name != "" && *name != 0)
        .map(|name| text_of(Some(name)));
    let (password, source) = match variable {
        Some(name) => (env(&name).map(Zeroizing::new), format!("environment variable {name}")),
        None => (literal, "password".to_owned()),
    };
    let password = password.unwrap_or_default();
    if password.is_empty() {
        return Err(AccountsError::NoPassword { index, from: source });
    }
    Ok(Password(password))
}
