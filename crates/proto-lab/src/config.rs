//! The lab config, `~/.config/teams-lab/lab.toml`: the 3 lab accounts, the approved service participants and the lab
//! directories.
//!
//! ```toml
//! # data_dir = "/absolute/path"   # optional; defaults to $XDG_DATA_HOME/teams-lab
//!
//! [[accounts]]
//! alias = "a"                     # a, b and c, each exactly once
//! upn = "bot-a@lab.example.com"   # all in one tenant domain
//! # mri = "..."                   # filled in after the first login
//!
//! [[service_participants]]
//! name = "echo"                   # approved by the user
//! # mri = "..."                   # filled in from a capture; without it the participant matches nothing
//! ```
//!
//! Passwords never go in this file.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::Deserialize;

/// The number of lab accounts, fixed by the lab's design.
pub const ACCOUNT_COUNT: usize = 3;

/// A lab account's alias.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Alias {
    A,
    B,
    C,
}

impl Alias {
    pub const ALL: [Self; ACCOUNT_COUNT] = [Self::A, Self::B, Self::C];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::A => "a",
            Self::B => "b",
            Self::C => "c",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::A => 0,
            Self::B => 1,
            Self::C => 2,
        }
    }
}

impl fmt::Display for Alias {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Alias {
    type Err = ConfigError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|alias| alias.as_str() == text)
            .ok_or_else(|| ConfigError::UnknownAlias(text.to_owned()))
    }
}

/// One lab account.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Account {
    pub alias: Alias,
    pub upn: String,
    /// The account's MRI, known after its first login.
    #[serde(default)]
    pub mri: Option<String>,
}

/// A Microsoft service participant the user has approved (such as the echo service).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceParticipant {
    pub name: String,
    /// The participant's MRI. Until it is filled in, the participant is approved in name only and matches nobody.
    #[serde(default)]
    pub mri: Option<String>,
}

/// The lab directories, all under one data dir outside the repo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LabPaths {
    data_dir: PathBuf,
}

impl LabPaths {
    #[must_use]
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }

    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// The pinned Chrome for Testing build.
    #[must_use]
    pub fn chrome_dir(&self) -> PathBuf {
        self.data_dir.join("chrome")
    }

    #[must_use]
    pub fn profiles_dir(&self) -> PathBuf {
        self.data_dir.join("profiles")
    }

    /// The persistent browser profile of one lab account.
    #[must_use]
    pub fn profile_dir(&self, alias: Alias) -> PathBuf {
        self.profiles_dir().join(alias.as_str())
    }

    /// Raw captures. They never enter the repo.
    #[must_use]
    pub fn captures_dir(&self) -> PathBuf {
        self.data_dir.join("captures")
    }

    #[must_use]
    pub fn bundles_dir(&self) -> PathBuf {
        self.data_dir.join("bundles")
    }

    /// The fence's JSONL log of decisions.
    #[must_use]
    pub fn fence_log(&self) -> PathBuf {
        self.data_dir.join("fence.log")
    }
}

/// Where the config file and the default data dir are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LabLocations {
    pub config_file: PathBuf,
    pub data_dir: PathBuf,
}

impl LabLocations {
    /// From `XDG_CONFIG_HOME`, `XDG_DATA_HOME` and `HOME`.
    ///
    /// # Errors
    /// [`ConfigError::NoHome`] when an XDG directory is unset and `HOME` is unset or relative.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::resolve(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("XDG_DATA_HOME"),
            std::env::var_os("HOME"),
        )
    }

    /// The XDG rules on explicit values: an empty or relative XDG directory is ignored in favour of the `HOME`
    /// default.
    ///
    /// # Errors
    /// [`ConfigError::NoHome`] when a fallback to `HOME` is needed and it is unset or relative.
    pub fn resolve(
        config_home: Option<OsString>,
        data_home: Option<OsString>,
        home: Option<OsString>,
    ) -> Result<Self, ConfigError> {
        let home = home.map(PathBuf::from).filter(|home| home.is_absolute());
        let base = |xdg: Option<OsString>, fallback: &str| {
            absolute(xdg)
                .or_else(|| home.as_ref().map(|home| home.join(fallback)))
                .ok_or(ConfigError::NoHome)
        };
        Ok(Self {
            config_file: base(config_home, ".config")?.join("teams-lab/lab.toml"),
            data_dir: base(data_home, ".local/share")?.join("teams-lab"),
        })
    }
}

fn absolute(value: Option<OsString>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|path| path.is_absolute())
}

/// Why `lab.toml` was refused.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("can't read {}: {source}", path.display())]
    Read { path: PathBuf, source: std::io::Error },
    #[error("lab.toml: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("lab.toml must list exactly 3 accounts (a, b, c), found {0}")]
    AccountCount(usize),
    #[error("lab.toml lists account {0} more than once")]
    DuplicateAlias(Alias),
    #[error("lab.toml lists UPN {0} more than once")]
    DuplicateUpn(String),
    #[error("not a UPN (user@domain): {0:?}")]
    InvalidUpn(String),
    #[error("all lab accounts must be in one tenant domain: {expected} vs {found}")]
    MixedDomain { expected: String, found: String },
    #[error("an empty mri for {0}; leave the field out until it is known")]
    EmptyMri(String),
    #[error("a service participant without a name")]
    EmptyServiceName,
    #[error("service participant {0} is listed more than once")]
    DuplicateService(String),
    #[error("data_dir must be absolute: {}", .0.display())]
    RelativeDataDir(PathBuf),
    #[error("not a lab account alias (a, b or c): {0:?}")]
    UnknownAlias(String),
    #[error("HOME is unset or relative, so the lab directories can't be found")]
    NoHome,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    data_dir: Option<PathBuf>,
    #[serde(default)]
    accounts: Vec<Account>,
    #[serde(default)]
    service_participants: Vec<ServiceParticipant>,
}

/// A validated lab config.
#[derive(Clone, Debug)]
pub struct LabConfig {
    accounts: [Account; ACCOUNT_COUNT],
    tenant_domain: String,
    service_participants: Vec<ServiceParticipant>,
    paths: LabPaths,
}

impl LabConfig {
    /// Reads and validates `locations.config_file`; `locations.data_dir` applies unless the file sets `data_dir`.
    ///
    /// # Errors
    /// When the file can't be read or is invalid (see [`LabConfig::parse`]).
    pub fn load(locations: &LabLocations) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(&locations.config_file).map_err(|source| ConfigError::Read {
            path: locations.config_file.clone(),
            source,
        })?;
        Self::parse(&text, &locations.data_dir)
    }

    /// Parses and validates `lab.toml` text. `default_data_dir` applies unless the text sets `data_dir`.
    ///
    /// # Errors
    /// On invalid TOML or unknown fields, a count other than 3 accounts, a duplicate alias or UPN, a malformed UPN, UPNs
    /// in more than one domain, an empty MRI, a duplicate or empty service name, or a relative `data_dir`.
    pub fn parse(text: &str, default_data_dir: &Path) -> Result<Self, ConfigError> {
        let raw: RawConfig = toml::from_str(text)?;
        let data_dir = match raw.data_dir {
            Some(dir) if dir.is_relative() => return Err(ConfigError::RelativeDataDir(dir)),
            Some(dir) => dir,
            None => default_data_dir.to_path_buf(),
        };
        let (accounts, tenant_domain) = validate_accounts(raw.accounts)?;
        validate_services(&raw.service_participants)?;
        Ok(Self {
            accounts,
            tenant_domain,
            service_participants: raw.service_participants,
            paths: LabPaths::new(data_dir),
        })
    }

    /// The accounts in alias order: a, b, c.
    #[must_use]
    pub fn accounts(&self) -> &[Account; ACCOUNT_COUNT] {
        &self.accounts
    }

    #[must_use]
    pub fn account(&self, alias: Alias) -> &Account {
        &self.accounts[alias.index()]
    }

    /// The tenant domain all lab UPNs share, lowercased.
    #[must_use]
    pub fn tenant_domain(&self) -> &str {
        &self.tenant_domain
    }

    #[must_use]
    pub fn service_participants(&self) -> &[ServiceParticipant] {
        &self.service_participants
    }

    #[must_use]
    pub fn paths(&self) -> &LabPaths {
        &self.paths
    }
}

fn validate_accounts(mut accounts: Vec<Account>) -> Result<([Account; ACCOUNT_COUNT], String), ConfigError> {
    let count = accounts.len();
    if count != ACCOUNT_COUNT {
        return Err(ConfigError::AccountCount(count));
    }
    let mut aliases = BTreeSet::new();
    let mut upns = BTreeSet::new();
    let mut tenant: Option<String> = None;
    for account in &accounts {
        if !aliases.insert(account.alias) {
            return Err(ConfigError::DuplicateAlias(account.alias));
        }
        let domain = upn_domain(&account.upn)?;
        if !upns.insert(account.upn.to_lowercase()) {
            return Err(ConfigError::DuplicateUpn(account.upn.clone()));
        }
        match &tenant {
            Some(expected) if *expected != domain => {
                return Err(ConfigError::MixedDomain {
                    expected: expected.clone(),
                    found: domain,
                });
            }
            Some(_) => {}
            None => tenant = Some(domain),
        }
        check_mri(account.mri.as_deref(), || account.alias.to_string())?;
    }
    accounts.sort_by_key(|account| account.alias);
    let accounts = <[Account; ACCOUNT_COUNT]>::try_from(accounts).map_err(|_| ConfigError::AccountCount(count))?;
    Ok((accounts, tenant.unwrap_or_default()))
}

fn validate_services(services: &[ServiceParticipant]) -> Result<(), ConfigError> {
    let mut names = BTreeSet::new();
    for service in services {
        if service.name.is_empty() {
            return Err(ConfigError::EmptyServiceName);
        }
        if !names.insert(service.name.as_str()) {
            return Err(ConfigError::DuplicateService(service.name.clone()));
        }
        check_mri(service.mri.as_deref(), || service.name.clone())?;
    }
    Ok(())
}

fn check_mri(mri: Option<&str>, owner: impl FnOnce() -> String) -> Result<(), ConfigError> {
    match mri {
        Some(mri) if mri.trim().is_empty() => Err(ConfigError::EmptyMri(owner())),
        _ => Ok(()),
    }
}

/// The lowercased domain of a `user@domain` UPN.
fn upn_domain(upn: &str) -> Result<String, ConfigError> {
    let invalid = || ConfigError::InvalidUpn(upn.to_owned());
    if upn.chars().any(char::is_whitespace) {
        return Err(invalid());
    }
    let (user, domain) = upn.split_once('@').ok_or_else(invalid)?;
    if user.is_empty() || domain.is_empty() || domain.contains('@') {
        return Err(invalid());
    }
    Ok(domain.to_lowercase())
}
