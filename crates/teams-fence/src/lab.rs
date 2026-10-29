//! The lab config, `~/.config/teams-lab/lab.toml`: the 3 lab accounts, the approved service participants and the lab
//! directories. It lives here because the fence's roster (the account and service MRIs) and its log
//! (`<data_dir>/fence.log`) come from it: [`Fence::open_lab`](crate::Fence::open_lab) is the only way a lab binary
//! builds a fence.
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
//! Passwords never go in this file. An account's MRI is written into it by [`record_account_mri`] after the account's
//! first login, and an approved service participant's by [`record_service_mri`] once it is established; nothing else
//! here writes the file.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::Deserialize;

use crate::membership::is_group_chat_id;

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
    /// The `application` attribute of the lab's items in the Secret Service: each lab account's refresh token and
    /// skypetoken, stored by `proto-lab auth` and read by every lab binary.
    pub const KEYRING_APPLICATION: &'static str = "teams-lab";

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

    /// Where `proto-lab auth` keeps an account's region (`regionGtms` and all) and its skypetoken's expiry, never the
    /// token: `<data_dir>/auth/<alias>.json`.
    #[must_use]
    pub fn region_file(&self, alias: Alias) -> PathBuf {
        self.data_dir.join("auth").join(format!("{}.json", alias.as_str()))
    }

    /// A lab account's native store (`teams-store`: its chats, messages and outbox), `<data_dir>/store/<alias>.sqlite`:
    /// lab data, never in the repo.
    #[must_use]
    pub fn store_file(&self, alias: Alias) -> PathBuf {
        self.data_dir.join("store").join(format!("{}.sqlite", alias.as_str()))
    }

    /// A lab account's app settings (`teams-ui`'s settings file: the selected chat, read receipts, typing,
    /// notifications, the idle time, the manual status), `<data_dir>/ui/<alias>.toml`: lab data, never in the repo.
    #[must_use]
    pub fn ui_settings_file(&self, alias: Alias) -> PathBuf {
        self.data_dir.join("ui").join(format!("{}.toml", alias.as_str()))
    }

    /// A lab account's app diagnostic log directory (`teams-log`: `teams-app.log` and its rotated files),
    /// `<data_dir>/logs/<alias>`: lab data like a capture, never in the repo.
    #[must_use]
    pub fn app_log_dir(&self, alias: Alias) -> PathBuf {
        self.data_dir.join("logs").join(alias.as_str())
    }

    /// The lab's own chats whose ids no account's MRIs give (the a–b–c group chat), `<data_dir>/chats.toml`
    /// ([`LabChats`]): lab data, never in the repo.
    #[must_use]
    pub fn chats_file(&self) -> PathBuf {
        self.data_dir.join("chats.toml")
    }
}

/// The lab's chats whose ids can't be derived (`<data_dir>/chats.toml`, [`LabPaths::chats_file`]): today the group
/// chat of a, b and c, which `proto-lab run abc-chat` creates once and records here. Lab data for the scenarios and
/// the clients, which find the chat by it; the fence doesn't read it (a group chat's members are read like any chat's).
///
/// ```toml
/// abc = "19:<id>@thread.v2"
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabChats {
    /// The group chat of a, b and c: a group chat id ([`is_group_chat_id`]).
    #[serde(default)]
    pub abc: Option<String>,
}

impl LabChats {
    /// Reads `paths`' chats file; no file is no chats.
    ///
    /// # Errors
    /// When the file can't be read, isn't valid TOML of this shape, or records an id that isn't a group chat's
    /// ([`ChatsError::NotAGroupChat`]).
    pub fn load(paths: &LabPaths) -> Result<Self, ChatsError> {
        let path = paths.chats_file();
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => return Err(ChatsError::Read { path, source }),
        };
        Self::parse(&text)
    }

    /// Parses a chats file's text (see [`LabChats::load`]).
    ///
    /// # Errors
    /// As [`LabChats::load`], but for reading.
    pub fn parse(text: &str) -> Result<Self, ChatsError> {
        let chats: Self = toml::from_str(text)?;
        match chats.abc.as_deref() {
            Some(id) if !is_group_chat_id(id) => Err(ChatsError::NotAGroupChat(id.to_owned())),
            _ => Ok(chats),
        }
    }
}

/// What [`record_abc_chat`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatRecord {
    /// The id was written to the chats file.
    Recorded,
    /// The chats file held that id already; it wasn't touched.
    AlreadyRecorded,
}

/// Why the chats file was refused or not written.
#[derive(Debug, thiserror::Error)]
pub enum ChatsError {
    #[error("can't read {}: {source}", path.display())]
    Read { path: PathBuf, source: std::io::Error },
    #[error("chats.toml: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("not a group chat id (19:<id>@thread.v2, not a meeting's): {0:?}")]
    NotAGroupChat(String),
    #[error("chats.toml already records another a–b–c chat; it isn't replaced")]
    Conflict,
    #[error("can't write {}: {source}", path.display())]
    Write { path: PathBuf, source: std::io::Error },
}

/// Records `id` as the a–b–c group chat in `paths`' chats file (mode `0600`), once `abc-chat` has created it. Only a
/// group chat id, and only when the file records none: another id already there is refused, not replaced. The file
/// is written whole through a new file renamed over it.
///
/// # Errors
/// When the file can't be read or written, isn't valid, records another chat ([`ChatsError::Conflict`]), or `id`
/// isn't a group chat's ([`ChatsError::NotAGroupChat`]).
pub fn record_abc_chat(paths: &LabPaths, id: &str) -> Result<ChatRecord, ChatsError> {
    if !is_group_chat_id(id) {
        return Err(ChatsError::NotAGroupChat(id.to_owned()));
    }
    match LabChats::load(paths)?.abc.as_deref() {
        Some(recorded) if recorded == id => return Ok(ChatRecord::AlreadyRecorded),
        Some(_) => return Err(ChatsError::Conflict),
        None => {}
    }
    let path = paths.chats_file();
    let text = format!(
        "# The lab's chats whose ids no MRI gives (written by `proto-lab run abc-chat`). Lab data: never in the repo.\n\
         abc = {}\n",
        toml::Value::String(id.to_owned())
    );
    write_through_new_file(&path, &text, 0o600).map_err(|source| ChatsError::Write { path, source })?;
    Ok(ChatRecord::Recorded)
}

/// Where the config file and the default data dir are, and where the personal build's data dir is, which no lab data
/// dir may be, contain or lie inside ([`crate::apart`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LabLocations {
    pub config_file: PathBuf,
    pub data_dir: PathBuf,
    /// The personal build's data dir (`$XDG_DATA_HOME/ngteams`, [`crate::apart::PersonalLocations`]).
    pub personal_data_dir: PathBuf,
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
        let data_home = base(data_home, ".local/share")?;
        Ok(Self {
            config_file: base(config_home, ".config")?.join("teams-lab/lab.toml"),
            data_dir: data_home.join("teams-lab"),
            personal_data_dir: data_home.join(crate::apart::PERSONAL_DIR),
        })
    }

    /// `paths` unless their data dir is, contains or lies inside the personal build's data dir
    /// ([`crate::apart::overlapping`]): every lab path is made through this, so no lab binary writes there.
    ///
    /// # Errors
    /// [`ConfigError::PersonalDataDir`] when they overlap.
    fn apart(&self, paths: LabPaths) -> Result<LabPaths, ConfigError> {
        if crate::apart::overlapping(paths.data_dir(), &self.personal_data_dir) {
            return Err(ConfigError::PersonalDataDir {
                lab: paths.data_dir().to_path_buf(),
                personal: self.personal_data_dir.clone(),
            });
        }
        Ok(paths)
    }

    /// The lab directories in effect: those of `lab.toml` (whose `data_dir` overrides the default) when the file exists,
    /// else those under the default data dir. Whatever needs only the directories, such as the pinned Chrome, finds them
    /// this way with or without a lab config.
    ///
    /// # Errors
    /// When `lab.toml` exists but can't be read or is invalid (see [`LabConfig::load`]), or the data dir in effect is,
    /// contains or lies inside the personal build's ([`ConfigError::PersonalDataDir`]).
    pub fn lab_paths(&self) -> Result<LabPaths, ConfigError> {
        match LabConfig::load(self) {
            Ok(config) => Ok(config.paths().clone()),
            Err(ConfigError::Read { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
                self.apart(LabPaths::new(self.data_dir.clone()))
            }
            Err(error) => Err(error),
        }
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
    #[error("lab.toml gives {0}'s mri to another participant too")]
    DuplicateMri(String),
    #[error("lab.toml already records another mri for account {0}; it isn't replaced")]
    MriConflict(Alias),
    #[error("lab.toml already records another mri for service participant {0}; it isn't replaced")]
    ServiceMriConflict(String),
    #[error("lab.toml approves no service participant named {0:?}")]
    UnknownService(String),
    #[error("not a bot's MRI (28:<lowercase guid>): {0:?}")]
    NotABotMri(String),
    #[error("can't edit lab.toml: {0}")]
    Edit(String),
    #[error("can't write {}: {source}", path.display())]
    Write { path: PathBuf, source: std::io::Error },
    #[error("{} changed while an mri was being recorded in it; nothing was written, run it again", .0.display())]
    Changed(PathBuf),
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
    /// The lab's data dir is, contains or lies inside the personal build's: no lab binary writes there.
    #[error(
        "the lab's data dir {} overlaps the personal build's {}: a lab binary never uses the personal build's data",
        lab.display(),
        personal.display()
    )]
    PersonalDataDir { lab: PathBuf, personal: PathBuf },
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
    /// When the file can't be read or is invalid (see [`LabConfig::parse`]), or its data dir is, contains or lies inside
    /// the personal build's ([`ConfigError::PersonalDataDir`]).
    pub fn load(locations: &LabLocations) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(&locations.config_file).map_err(|source| ConfigError::Read {
            path: locations.config_file.clone(),
            source,
        })?;
        let mut config = Self::parse(&text, &locations.data_dir)?;
        config.paths = locations.apart(config.paths)?;
        Ok(config)
    }

    /// Parses and validates `lab.toml` text. `default_data_dir` applies unless the text sets `data_dir`.
    ///
    /// # Errors
    /// On invalid TOML or unknown fields, a count other than 3 accounts, a duplicate alias or UPN, a malformed UPN, UPNs
    /// in more than one domain, an empty MRI or one given to two participants, a duplicate or empty service name, or a
    /// relative `data_dir`.
    pub fn parse(text: &str, default_data_dir: &Path) -> Result<Self, ConfigError> {
        let raw: RawConfig = toml::from_str(text)?;
        let data_dir = match raw.data_dir {
            Some(dir) if dir.is_relative() => return Err(ConfigError::RelativeDataDir(dir)),
            Some(dir) => dir,
            None => default_data_dir.to_path_buf(),
        };
        let (accounts, tenant_domain) = validate_accounts(raw.accounts)?;
        validate_services(&raw.service_participants)?;
        validate_unique_mris(&accounts, &raw.service_participants)?;
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

/// One MRI names one participant: an account and a service, or two accounts, can't share it.
fn validate_unique_mris(accounts: &[Account], services: &[ServiceParticipant]) -> Result<(), ConfigError> {
    let owners = accounts
        .iter()
        .map(|account| (account.mri.as_deref(), account.alias.to_string()))
        .chain(
            services
                .iter()
                .map(|service| (service.mri.as_deref(), service.name.clone())),
        );
    let mut seen = BTreeSet::new();
    for (mri, owner) in owners {
        if let Some(mri) = mri
            && !seen.insert(mri)
        {
            return Err(ConfigError::DuplicateMri(owner));
        }
    }
    Ok(())
}

/// What [`record_account_mri`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MriRecord {
    /// The MRI was written into `lab.toml`.
    Recorded,
    /// `lab.toml` held that MRI for the account already; the file wasn't touched.
    AlreadyRecorded,
}

/// Records `mri` as account `alias`'s MRI in the `lab.toml` at `locations.config_file`, once the account's first login
/// has shown it. Everything else in the file (comments included) stays as it is, and so does the file's mode. The new
/// text has to pass [`LabConfig::parse`]; it replaces the file in one rename, so a reader sees the old file or the new
/// one, never half of either, and only if the file, read again right before, still holds the text the edit was made
/// from.
///
/// An MRI is recorded only for an account without one: a different MRI already there is refused, not replaced (the
/// roster of the fence comes from these MRIs).
///
/// # Errors
/// When the file can't be read, isn't valid, records another MRI for the account ([`ConfigError::MriConflict`]), or
/// would become invalid with this one (an empty MRI, one another participant has); when it changed while the MRI was
/// being recorded ([`ConfigError::Changed`]); or when it can't be written.
pub fn record_account_mri(locations: &LabLocations, alias: Alias, mri: &str) -> Result<MriRecord, ConfigError> {
    let path = &locations.config_file;
    let text = fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.clone(),
        source,
    })?;
    let config = LabConfig::parse(&text, &locations.data_dir)?;
    check_mri(Some(mri), || alias.to_string())?;
    match config.account(alias).mri.as_deref() {
        Some(recorded) if recorded == mri => return Ok(MriRecord::AlreadyRecorded),
        Some(_) => return Err(ConfigError::MriConflict(alias)),
        None => {}
    }
    let edited = with_account_mri(&text, alias, mri)?;
    LabConfig::parse(&edited, &locations.data_dir)?;
    replace_unchanged(path, &text, &edited)?;
    Ok(MriRecord::Recorded)
}

/// Records `mri` as the MRI of the approved service participant `name` (its `[[service_participants]]` entry in the
/// `lab.toml` at `locations.config_file`), once it has been established from the sources the M0 plan names. As
/// [`record_account_mri`]: only for a participant without one, everything else in the file kept, the new text
/// validated, one rename, only over the text it was made from. Only a bot's MRI is taken (`28:` and a lowercase GUID):
/// an approved service is never a person.
///
/// # Errors
/// When the file can't be read or isn't valid, approves no service named `name` ([`ConfigError::UnknownService`]),
/// records another MRI for it ([`ConfigError::ServiceMriConflict`]), `mri` isn't a bot's ([`ConfigError::NotABotMri`]),
/// another participant has it ([`ConfigError::DuplicateMri`]); when it changed while the MRI was being recorded
/// ([`ConfigError::Changed`]); or when it can't be written.
pub fn record_service_mri(locations: &LabLocations, name: &str, mri: &str) -> Result<MriRecord, ConfigError> {
    let path = &locations.config_file;
    let text = fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.clone(),
        source,
    })?;
    let config = LabConfig::parse(&text, &locations.data_dir)?;
    if !is_bot_mri(mri) {
        return Err(ConfigError::NotABotMri(mri.to_owned()));
    }
    let service = config
        .service_participants()
        .iter()
        .find(|service| service.name == name)
        .ok_or_else(|| ConfigError::UnknownService(name.to_owned()))?;
    match service.mri.as_deref() {
        Some(recorded) if recorded == mri => return Ok(MriRecord::AlreadyRecorded),
        Some(_) => return Err(ConfigError::ServiceMriConflict(name.to_owned())),
        None => {}
    }
    let edited = with_service_mri(&text, name, mri)?;
    LabConfig::parse(&edited, &locations.data_dir)?;
    replace_unchanged(path, &text, &edited)?;
    Ok(MriRecord::Recorded)
}

/// `28:` and a lowercase GUID (`8-4-4-4-12` hex digits), nothing else.
fn is_bot_mri(mri: &str) -> bool {
    let Some(guid) = mri.strip_prefix("28:") else {
        return false;
    };
    let groups: Vec<&str> = guid.split('-').collect();
    groups.len() == 5
        && groups.iter().zip([8, 4, 4, 4, 12]).all(|(group, len)| {
            group.len() == len
                && group
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

/// `text` with `mri = "<mri>"` set in the `[[service_participants]]` table named `name`.
fn with_service_mri(text: &str, name: &str, mri: &str) -> Result<String, ConfigError> {
    let mut document: toml_edit::DocumentMut = text
        .parse()
        .map_err(|error: toml_edit::TomlError| ConfigError::Edit(error.to_string()))?;
    let service = document
        .get_mut("service_participants")
        .and_then(toml_edit::Item::as_array_of_tables_mut)
        .and_then(|services| {
            services
                .iter_mut()
                .find(|table| table.get("name").and_then(toml_edit::Item::as_str) == Some(name))
        })
        .ok_or_else(|| ConfigError::Edit(format!("no [[service_participants]] table named {name}")))?;
    service.insert("mri", toml_edit::value(mri));
    Ok(document.to_string())
}

/// `text` with `mri = "<mri>"` set in the `[[accounts]]` table of `alias`.
fn with_account_mri(text: &str, alias: Alias, mri: &str) -> Result<String, ConfigError> {
    let mut document: toml_edit::DocumentMut = text
        .parse()
        .map_err(|error: toml_edit::TomlError| ConfigError::Edit(error.to_string()))?;
    let account = document
        .get_mut("accounts")
        .and_then(toml_edit::Item::as_array_of_tables_mut)
        .and_then(|accounts| {
            accounts
                .iter_mut()
                .find(|table| table.get("alias").and_then(toml_edit::Item::as_str) == Some(alias.as_str()))
        })
        .ok_or_else(|| ConfigError::Edit(format!("no [[accounts]] table with alias {alias}")))?;
    account.insert("mri", toml_edit::value(mri));
    Ok(document.to_string())
}

/// Replaces `path` with `edited`, which was made from `read`, the text read from it before: only when the file still
/// holds `read` when it is read again just before the replacement, so an edit made in between (by hand or by another
/// writer, such as one taking an approval away) isn't undone by writing back the older text. Otherwise nothing is
/// written ([`ConfigError::Changed`]).
fn replace_unchanged(path: &Path, read: &str, edited: &str) -> Result<(), ConfigError> {
    let now = fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    if now != read {
        return Err(ConfigError::Changed(path.to_path_buf()));
    }
    replace_file(path, edited)
}

/// Replaces `path` with `text` through a new file beside it, renamed over it, with the old file's mode.
fn replace_file(path: &Path, text: &str) -> Result<(), ConfigError> {
    let write_error = |source| ConfigError::Write {
        path: path.to_path_buf(),
        source,
    };
    let mode = fs::metadata(path).map_err(write_error)?.permissions().mode() & 0o7777;
    write_through_new_file(path, text, mode).map_err(write_error)
}

/// Writes `text` to `path` (made or replaced) through a new file beside it, `<path>.<pid>.new` with `mode`, renamed
/// over it; when that fails, the new file is removed and `path` is untouched.
fn write_through_new_file(path: &Path, text: &str, mode: u32) -> std::io::Result<()> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".{}.new", std::process::id()));
    let temporary = PathBuf::from(temporary);
    let written = (|| {
        // owner-only until it has `mode`, which may be stricter still
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if written.is_err() {
        // whatever was left of the new file; the old one is untouched
        let _ = fs::remove_file(&temporary);
    }
    written
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

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{ConfigError, replace_unchanged};

    #[test]
    fn a_file_changed_since_it_was_read_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lab.toml");
        fs::write(&path, "read\n").unwrap();
        // edited by hand after the writer read it: the writer's text, made from the older one, isn't written back
        fs::write(&path, "edited in between\n").unwrap();
        let error = replace_unchanged(&path, "read\n", "read\nmri\n").unwrap_err();
        assert!(
            matches!(error, ConfigError::Changed(ref changed) if *changed == path),
            "{error}"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "edited in between\n");
        // as it was read, it is replaced
        replace_unchanged(&path, "edited in between\n", "edited in between\nmri\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "edited in between\nmri\n");
    }
}
