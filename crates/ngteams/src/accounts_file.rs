//! Account files, identity metadata, paths and atomic persistence for ngteams.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use teams_proto::Mri;

pub use paths::{PERSONAL_DIR, PersonalLocations, overlapping};
mod paths;

/// The personal build's files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersonalPaths {
    config_dir: PathBuf,
    data_dir: PathBuf,
}

impl PersonalPaths {
    /// The default ones, from the environment ([`PersonalLocations::from_env`]).
    ///
    /// # Errors
    /// [`PersonalError::NoHome`] when they can't be found.
    pub fn from_env() -> Result<Self, PersonalError> {
        let locations = PersonalLocations::from_env().ok_or(PersonalError::NoHome)?;
        Ok(Self::new(locations.config_dir, locations.data_dir))
    }

    /// Paths in these directories (tests use temporary ones).
    #[must_use]
    pub fn new(config_dir: PathBuf, data_dir: PathBuf) -> Self {
        Self { config_dir, data_dir }
    }

    #[must_use]
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// `accounts.toml`: the accounts the user added, in the order they were added ([`PersonalAccount`]).
    #[must_use]
    pub fn accounts_file(&self) -> PathBuf {
        self.config_dir.join("accounts.toml")
    }

    /// `account.toml`: the one account a build before several accounts kept ([`PersonalPaths::migrate_single_account`]
    /// moves it into `accounts.toml`).
    #[must_use]
    pub fn legacy_account_file(&self) -> PathBuf {
        self.config_dir.join("account.toml")
    }

    /// `account`'s store (`teams-store`): `store/<oid>.sqlite`, one per account, so a store kept after
    /// `ngteams --forget` is never another account's; `None` before a sign-in named its object id (or for one that
    /// isn't a GUID).
    #[must_use]
    pub fn store_file(&self, account: &PersonalAccount) -> Option<PathBuf> {
        let oid = account.oid.as_deref().filter(|oid| is_guid(oid))?;
        Some(self.data_dir.join("store").join(format!("{oid}.sqlite")))
    }

    /// `account`'s region as authz gave it (never a token): `regions/<oid>.json`; `None` before a sign-in named its
    /// object id.
    #[must_use]
    pub fn region_file(&self, account: &PersonalAccount) -> Option<PathBuf> {
        let oid = account.oid.as_deref().filter(|oid| is_guid(oid))?;
        Some(self.data_dir.join("regions").join(format!("{oid}.json")))
    }

    /// `region.json`: the one account's region a build before several accounts kept.
    #[must_use]
    pub fn legacy_region_file(&self) -> PathBuf {
        self.data_dir.join("region.json")
    }

    /// Moves a build's single account into the files of several: its `region.json` to `regions/<oid>.json`, then
    /// `account.toml` into `accounts.toml` (as its one account), which marks the move done. Nothing when `accounts.toml` exists already or there is
    /// no `account.toml`; a region of an account no sign-in named is left where it is (the next sign-in writes one).
    ///
    /// # Errors
    /// When a file can't be read, parsed or moved; whatever moved before stays moved, and the next start goes on.
    pub fn migrate_single_account(&self) -> Result<(), PersonalError> {
        let legacy = self.legacy_account_file();
        if self.accounts_file().exists() || !legacy.exists() {
            return Ok(());
        }
        let text = fs::read_to_string(&legacy).map_err(|source| PersonalError::Read {
            path: legacy.clone(),
            source,
        })?;
        let account = PersonalAccount::parse(&text)?;
        // the region first: `accounts.toml` written is what says the move is done
        if let Some(region) = self.region_file(&account) {
            let old = self.legacy_region_file();
            if old.is_file() {
                let moved = region
                    .parent()
                    .map_or(Ok(()), teams_platform::private::create_dir_all)
                    .and_then(|()| fs::rename(&old, &region));
                moved.map_err(|source| PersonalError::Write { path: region, source })?;
            }
        }
        write_accounts(self, std::slice::from_ref(&account))?;
        fs::remove_file(&legacy).map_err(|source| PersonalError::Write { path: legacy, source })
    }

    /// The app's settings (`teams-ui`).
    #[must_use]
    pub fn settings_file(&self) -> PathBuf {
        self.data_dir.join("settings.toml")
    }

    /// The app's diagnostic log directory (`teams-log`).
    #[must_use]
    pub fn log_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    /// Makes the data dir (and the config dir), `0700`.
    ///
    /// # Errors
    /// When they can't be made or made private.
    pub fn make_dirs(&self) -> io::Result<()> {
        for dir in [&self.data_dir, &self.config_dir] {
            teams_platform::private::create_dir_all(dir)?;
            teams_platform::private::restrict_dir(dir)?;
        }
        Ok(())
    }
}

/// One account the personal build signs in (an `[[account]]` of `accounts.toml`): the UPN the user gave (the first
/// sign-in screen, the rail's "+" or `ngteams --account`), and once the
/// first sign-in went through, the object id and tenant its ID token named, which every later sign-in must match, and
/// the display name it gave (`name`, refreshed by every sign-in; what the account's posts carry as their sender's
/// name). Never a token.
///
/// ```toml
/// upn = "someone@example.com"
/// oid = "<the account's object id>"   # written by the first sign-in
/// tid = "<its tenant id>"             # written by the first sign-in
/// name = "Someone Example"            # written by a sign-in whose ID token gave one
/// idp_hosts = ["sso.example.com"]     # optional: the federated identity provider's hosts the sign-in window may reach
/// ```
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonalAccount {
    pub upn: String,
    #[serde(default)]
    pub oid: Option<String>,
    #[serde(default)]
    pub tid: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub idp_hosts: Vec<String>,
}

impl fmt::Debug for PersonalAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PersonalAccount")
            .field("upn", &self.upn)
            .field("idp_hosts", &self.idp_hosts.len())
            .finish_non_exhaustive()
    }
}

impl PersonalAccount {
    /// The accounts `paths`' `accounts.toml` holds, in the order they were added; none before the first one was given.
    /// Without `accounts.toml`, a single account's `account.toml` (not migrated yet) is read as the one account.
    ///
    /// # Errors
    /// When it can't be read or isn't valid ([`PersonalAccount::parse`]; an account given twice, by UPN or object id).
    pub fn load_all(paths: &PersonalPaths) -> Result<Vec<Self>, PersonalError> {
        let path = paths.accounts_file();
        match fs::read_to_string(&path) {
            Ok(text) => parse_accounts(&text),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let legacy = paths.legacy_account_file();
                match fs::read_to_string(&legacy) {
                    Ok(text) => Ok(vec![Self::parse(&text)?]),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
                    Err(source) => Err(PersonalError::Read { path: legacy, source }),
                }
            }
            Err(error) => Err(PersonalError::Read { path, source: error }),
        }
    }

    /// The account `upn` (case aside) of `paths`' accounts ([`PersonalAccount::load_all`]); `None` when it isn't one.
    ///
    /// # Errors
    /// As [`PersonalAccount::load_all`].
    pub fn find(paths: &PersonalPaths, upn: &str) -> Result<Option<Self>, PersonalError> {
        Ok(Self::load_all(paths)?
            .into_iter()
            .find(|account| account.upn.eq_ignore_ascii_case(upn)))
    }

    /// Takes the account `upn` (case aside) out of `paths`' `accounts.toml`, the others kept in their order; the account
    /// taken, or `None` when it wasn't one.
    ///
    /// # Errors
    /// When the accounts can't be read or written.
    pub fn remove(paths: &PersonalPaths, upn: &str) -> Result<Option<Self>, PersonalError> {
        let _changing = changing();
        let mut accounts = Self::load_all(paths)?;
        let Some(index) = accounts
            .iter()
            .position(|account| account.upn.eq_ignore_ascii_case(upn))
        else {
            return Ok(None);
        };
        let removed = accounts.remove(index);
        write_accounts(paths, &accounts)?;
        Ok(Some(removed))
    }

    /// Parses one account (a single account's `account.toml`, or one `[[account]]` table of `accounts.toml`): a UPN (`user@domain`), the object id and tenant id as GUIDs (stored lowercase), both or
    /// neither, and the identity provider hosts, each a lowercase DNS name.
    ///
    /// # Errors
    /// [`PersonalError::Account`] saying what is wrong.
    pub fn parse(text: &str) -> Result<Self, PersonalError> {
        let mut account: Self = toml::from_str(text).map_err(|error| PersonalError::Account(error.message().into()))?;
        account.oid = account.oid.map(|oid| oid.to_ascii_lowercase());
        account.tid = account.tid.map(|tid| tid.to_ascii_lowercase());
        account.validate()?;
        Ok(account)
    }

    /// The account `upn` before its first sign-in (no object id or tenant yet).
    ///
    /// # Errors
    /// As [`PersonalAccount::parse`].
    pub fn pending(upn: &str) -> Result<Self, PersonalError> {
        let account = Self {
            upn: upn.to_owned(),
            oid: None,
            tid: None,
            name: None,
            idp_hosts: Vec::new(),
        };
        account.validate()?;
        Ok(account)
    }

    /// The account `upn` as a sign-in named it: its object id and tenant.
    ///
    /// # Errors
    /// As [`PersonalAccount::parse`].
    pub fn new(upn: &str, oid: &str, tid: &str) -> Result<Self, PersonalError> {
        let account = Self {
            upn: upn.to_owned(),
            oid: Some(oid.to_ascii_lowercase()),
            tid: Some(tid.to_ascii_lowercase()),
            name: None,
            idp_hosts: Vec::new(),
        };
        account.validate()?;
        Ok(account)
    }

    /// The same account with the object id and tenant a sign-in named, and the display name its ID token gave (kept as
    /// it was when the token gave none, or one a post can't carry).
    ///
    /// # Errors
    /// As [`PersonalAccount::parse`].
    pub fn signed_in_as(&self, oid: &str, tid: &str, name: Option<&str>) -> Result<Self, PersonalError> {
        let name = name.map(str::trim).filter(|name| is_display_name(name));
        let account = Self {
            oid: Some(oid.to_ascii_lowercase()),
            tid: Some(tid.to_ascii_lowercase()),
            name: name.map(str::to_owned).or_else(|| self.name.clone()),
            ..self.clone()
        };
        account.validate()?;
        Ok(account)
    }

    /// Whether a sign-in has named its object id and tenant.
    #[must_use]
    pub fn is_signed_in(&self) -> bool {
        self.oid.is_some() && self.tid.is_some()
    }

    fn validate(&self) -> Result<(), PersonalError> {
        let bad = |why: &str| Err(PersonalError::Account(why.to_owned()));
        let upn_ok = self
            .upn
            .split_once('@')
            .is_some_and(|(user, domain)| !user.is_empty() && !domain.is_empty() && !domain.contains('@'))
            && !self.upn.chars().any(char::is_whitespace);
        if !upn_ok {
            return bad("upn isn't a UPN (user@domain)");
        }
        match (&self.oid, &self.tid) {
            (Some(oid), Some(tid)) if is_guid(oid) && is_guid(tid) => {}
            (None, None) => {}
            _ => return bad("oid and tid must be GUIDs, both or neither"),
        }
        if !self.name.as_deref().is_none_or(is_display_name) {
            return bad("name is empty, has control characters or is padded with spaces");
        }
        if !self.idp_hosts.iter().all(|host| is_host(host)) {
            return bad("an idp_hosts entry isn't a lowercase DNS name");
        }
        Ok(())
    }

    /// The account's MRI, `8:orgid:<oid>`, once a sign-in named its object id.
    #[must_use]
    pub fn mri(&self) -> Option<Mri> {
        self.oid.as_ref().map(|oid| Mri::new(format!("8:orgid:{oid}")))
    }

    /// Writes the account into `paths`' `accounts.toml` (`0600`, its directory `0700`): in the place of the account of
    /// the same UPN (case aside), else after the others; the file whole, through a new file renamed over it.
    /// A single account's files not moved yet ([`PersonalPaths::migrate_single_account`]) are moved first.
    ///
    /// # Errors
    /// When the accounts can't be read or written, or another account has the same object id.
    pub fn save(&self, paths: &PersonalPaths) -> Result<(), PersonalError> {
        let _changing = changing();
        // a single account's files not moved yet go first, so they are never left behind `accounts.toml`
        paths.migrate_single_account()?;
        let mut accounts = Self::load_all(paths)?;
        match accounts
            .iter_mut()
            .find(|account| account.upn.eq_ignore_ascii_case(&self.upn))
        {
            Some(held) => *held = self.clone(),
            None => accounts.push(self.clone()),
        }
        check_distinct(&accounts)?;
        write_accounts(paths, &accounts)
    }

    /// The account as one `[[account]]` table of `accounts.toml`.
    fn to_toml(&self) -> String {
        let hosts: Vec<toml::Value> = self.idp_hosts.iter().cloned().map(toml::Value::String).collect();
        let ids = match (&self.oid, &self.tid) {
            (Some(oid), Some(tid)) => format!(
                "oid = {}\ntid = {}\n",
                toml::Value::String(oid.clone()),
                toml::Value::String(tid.clone())
            ),
            _ => String::new(),
        };
        let name = self
            .name
            .as_ref()
            .map(|name| format!("name = {}\n", toml::Value::String(name.clone())))
            .unwrap_or_default();
        format!(
            "[[account]]\nupn = {}\n{ids}{name}idp_hosts = {}\n",
            toml::Value::String(self.upn.clone()),
            toml::Value::Array(hosts),
        )
    }
}

/// Held while `accounts.toml` is read, changed and written again: an account saved by a sign-in and one removed at the
/// same moment (each on a task of its own) never write over each other's change.
static CHANGING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The next staging file's number: two writes in one process never share a staging file.
static STAGING: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn changing() -> std::sync::MutexGuard<'static, ()> {
    CHANGING.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `accounts.toml` as written: its `[[account]]` tables.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountsFile {
    #[serde(default)]
    account: Vec<toml::Table>,
}

/// Parses `accounts.toml`: each `[[account]]` as [`PersonalAccount::parse`] takes `account.toml`, no account twice.
fn parse_accounts(text: &str) -> Result<Vec<PersonalAccount>, PersonalError> {
    let file: AccountsFile = toml::from_str(text).map_err(|error| PersonalError::Account(error.message().into()))?;
    let accounts = file
        .account
        .into_iter()
        .map(|table| PersonalAccount::parse(&table.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    check_distinct(&accounts)?;
    Ok(accounts)
}

/// Refuses an account given twice, by UPN (case aside) or object id.
fn check_distinct(accounts: &[PersonalAccount]) -> Result<(), PersonalError> {
    for (index, account) in accounts.iter().enumerate() {
        let twice = accounts[..index].iter().any(|earlier| {
            earlier.upn.eq_ignore_ascii_case(&account.upn) || (earlier.oid.is_some() && earlier.oid == account.oid)
        });
        if twice {
            return Err(PersonalError::Account(format!("{} is given twice", account.upn)));
        }
    }
    Ok(())
}

/// Writes `accounts` to `paths`' `accounts.toml` (`0600`, its directory `0700`), whole, through a new file renamed over
/// it.
fn write_accounts(paths: &PersonalPaths, accounts: &[PersonalAccount]) -> Result<(), PersonalError> {
    let path = paths.accounts_file();
    let write = || -> io::Result<()> {
        teams_platform::private::create_dir_all(paths.config_dir())?;
        let mut text = String::from(
            "# The accounts the personal build signs in, in the order they were added (a first sign-in adds oid and\n\
             # tid). Never a token.\n",
        );
        for account in accounts {
            text.push('\n');
            text.push_str(&account.to_toml());
        }
        let mut staging = path.as_os_str().to_owned();
        let serial = STAGING.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        staging.push(format!(".{}.{serial}.new", std::process::id()));
        let staging = PathBuf::from(staging);
        let written = (|| {
            let mut file = teams_platform::private::open(&staging, teams_platform::private::Open::NewReadWrite)?;
            io::Write::write_all(&mut file, text.as_bytes())?;
            file.sync_all()?;
            fs::rename(&staging, &path)
        })();
        if written.is_err() {
            let _ = fs::remove_file(&staging);
        }
        written
    };
    write().map_err(|source| PersonalError::Write {
        path: path.clone(),
        source,
    })
}

/// Why the account files could not be used.
#[derive(Debug, thiserror::Error)]
pub enum PersonalError {
    #[error("the personal build's directories cannot be resolved from the native home/data environment")]
    NoHome,
    #[error("can't read {}: {source}", path.display())]
    Read { path: PathBuf, source: io::Error },
    #[error("can't write {}: {source}", path.display())]
    Write { path: PathBuf, source: io::Error },
    #[error("accounts.toml: {0}")]
    Account(String),
    #[error("the account isn't one signed in yet (accounts.toml names no object id for it): sign in first")]
    NoAccount,
}

/// A GUID as `8-4-4-4-12` lowercase hex digits.
fn is_guid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 5
        && groups.iter().zip([8, 4, 4, 4, 12]).all(|(group, len)| {
            group.len() == len
                && group
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

/// A display name as a post may carry it: not empty, no control character, no leading or trailing space, at most 256
/// characters.
fn is_display_name(text: &str) -> bool {
    !text.is_empty() && text.trim() == text && text.chars().count() <= 256 && !text.chars().any(char::is_control)
}

/// A lowercase DNS name: labels of letters, digits and `-`, at least two of them, none empty.
fn is_host(text: &str) -> bool {
    let labels: Vec<&str> = text.split('.').collect();
    labels.len() >= 2
        && text.len() <= 253
        && labels.iter().all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}
