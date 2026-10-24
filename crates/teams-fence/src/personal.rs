//! The personal build's fence (`docs/personal-build.md`, approved by the user on 2026-10-23), only with the `personal`
//! feature, which only `crates/teams` enables.
//!
//! - **Its files** ([`PersonalPaths`]): `account.toml` in `~/.config/teams/` (the account the first sign-in signed in:
//!   its UPN, object id and tenant, and the identity provider hosts the user names; never a token), and under
//!   `~/.local/share/teams/` its `fence.log`, store, region, settings and diagnostic log. The data dir is refused when
//!   it is, contains or lies inside the lab's ([`PersonalPaths::check_apart`]), and an account `lab.toml` names is
//!   refused ([`PersonalAccount::check_not_lab`]).
//! - **Its decision** ([`Fence::open_personal`], the only way to it outside tests): a write is permitted only when it
//!   is a message, an edit, a delete, a reaction, typing or a read receipt on a 1:1 chat whose id names the account or
//!   on a group chat (never a channel, a meeting chat, a stream, a new chat, a call), adding nobody, and the chat's
//!   thread, read fresh when the fence decides by [`PersonalMembershipSource`] (one unpaged read in a shape a capture
//!   showed, every member in a captured MRI form: [`crate::parse_listing`]), lists the account. The one write without
//!   a conversation is the account's own availability, on the account itself. Every decision goes to the personal
//!   `fence.log` first; an allow that can't be logged is denied.
//! - **Its reads by `POST`**: [`Fence::check_post_read_among`] with the people of the account's chats.

use std::fmt;
use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;

use crate::DenyReason;
use crate::apart::{PersonalLocations, overlapping};
use crate::decision::{Fence, Membership, MembershipFuture, MembershipSource, PartialReason};
use crate::lab::{LabConfig, LabLocations};
use crate::log::{FenceLog, Verdict};
use crate::membership::{CredentialSource, LiveMembershipSource, LiveSourceError, is_group_chat_id, one_to_one_pair};
use crate::target::{Mri, Scope, Target, WriteKind};

/// The kinds of write the personal build sends (besides the account's own availability).
pub const PERSONAL_KINDS: [WriteKind; 6] = [
    WriteKind::Message,
    WriteKind::Edit,
    WriteKind::Delete,
    WriteKind::Reaction,
    WriteKind::Typing,
    WriteKind::ReadReceipt,
];

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

    /// `account.toml`: the account the first sign-in signed in ([`PersonalAccount`]).
    #[must_use]
    pub fn account_file(&self) -> PathBuf {
        self.config_dir.join("account.toml")
    }

    /// The personal fence's log.
    #[must_use]
    pub fn fence_log(&self) -> PathBuf {
        self.data_dir.join("fence.log")
    }

    /// The account's store (`teams-store`).
    #[must_use]
    pub fn store_file(&self) -> PathBuf {
        self.data_dir.join("store.sqlite")
    }

    /// The account's region as authz gave it (never a token).
    #[must_use]
    pub fn region_file(&self) -> PathBuf {
        self.data_dir.join("region.json")
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
            DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    /// Refuses a data dir that is, contains or lies inside the lab's: its default data dir (`lab`), and the one
    /// `lab.toml` sets when it exists. Nothing of the lab is written or kept.
    ///
    /// # Errors
    /// [`PersonalError::LabDataDir`] when they overlap; [`PersonalError::Lab`] when `lab.toml` exists but can't be read.
    pub fn check_apart(&self, lab: &LabLocations) -> Result<(), PersonalError> {
        let mut lab_dirs = vec![lab.data_dir.clone()];
        if let Some(config) = lab_config(lab)? {
            lab_dirs.push(config.paths().data_dir().to_path_buf());
        }
        for dir in lab_dirs {
            if overlapping(&self.data_dir, &dir) || overlapping(&self.config_dir, &dir) {
                return Err(PersonalError::LabDataDir {
                    personal: self.data_dir.clone(),
                    lab: dir,
                });
            }
        }
        Ok(())
    }
}

/// `lab.toml` as written, without the lab's own checks of where its data dir is; `None` when there is none.
fn lab_config(lab: &LabLocations) -> Result<Option<LabConfig>, PersonalError> {
    match fs::read_to_string(&lab.config_file) {
        Ok(text) => LabConfig::parse(&text, &lab.data_dir)
            .map(Some)
            .map_err(|error| PersonalError::Lab(error.to_string())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(PersonalError::Lab(error.to_string())),
    }
}

/// The account the personal build signs in (`account.toml`): the UPN the user gave (`teams --account`), and once the
/// first sign-in went through, the object id and tenant its ID token named, which every later sign-in must match.
/// Never a token.
///
/// ```toml
/// upn = "someone@example.com"
/// oid = "<the account's object id>"   # written by the first sign-in
/// tid = "<its tenant id>"             # written by the first sign-in
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
    /// Reads `paths`' `account.toml`; `None` when there is none yet (before the first sign-in).
    ///
    /// # Errors
    /// When it can't be read or isn't valid ([`PersonalAccount::parse`]).
    pub fn load(paths: &PersonalPaths) -> Result<Option<Self>, PersonalError> {
        let path = paths.account_file();
        match fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(PersonalError::Read { path, source: error }),
        }
    }

    /// Parses `account.toml`: a UPN (`user@domain`), the object id and tenant id as GUIDs (stored lowercase), both or
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
            idp_hosts: Vec::new(),
        };
        account.validate()?;
        Ok(account)
    }

    /// The same account with the object id and tenant a sign-in named.
    ///
    /// # Errors
    /// As [`PersonalAccount::parse`].
    pub fn signed_in_as(&self, oid: &str, tid: &str) -> Result<Self, PersonalError> {
        let account = Self {
            oid: Some(oid.to_ascii_lowercase()),
            tid: Some(tid.to_ascii_lowercase()),
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

    /// Refuses an account `lab.toml` names, by UPN (case aside) or MRI, when `lab.toml` exists: a lab account is never
    /// driven by the personal fence.
    ///
    /// # Errors
    /// [`PersonalError::LabAccount`]; [`PersonalError::Lab`] when `lab.toml` exists but can't be read.
    pub fn check_not_lab(&self, lab: &LabLocations) -> Result<(), PersonalError> {
        let Some(config) = lab_config(lab)? else {
            return Ok(());
        };
        let mri = self.mri();
        let named = config.accounts().iter().any(|account| {
            account.upn.eq_ignore_ascii_case(&self.upn)
                || account
                    .mri
                    .as_deref()
                    .zip(mri.as_ref())
                    .is_some_and(|(lab, mri)| lab.eq_ignore_ascii_case(mri.as_str()))
        });
        if named {
            return Err(PersonalError::LabAccount);
        }
        Ok(())
    }

    /// Writes the account to `paths`' `account.toml` (`0600`, its directory `0700`), whole, through a new file renamed
    /// over it.
    ///
    /// # Errors
    /// When it can't be written.
    pub fn save(&self, paths: &PersonalPaths) -> Result<(), PersonalError> {
        let path = paths.account_file();
        let write = || -> io::Result<()> {
            let dir = paths.config_dir();
            DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
            let hosts: Vec<toml::Value> = self.idp_hosts.iter().cloned().map(toml::Value::String).collect();
            let ids = match (&self.oid, &self.tid) {
                (Some(oid), Some(tid)) => format!(
                    "oid = {}\ntid = {}\n",
                    toml::Value::String(oid.clone()),
                    toml::Value::String(tid.clone())
                ),
                _ => String::new(),
            };
            let text = format!(
                "# The account the personal build signs in (its first sign-in adds oid and tid). Never a token.\n\
                 upn = {}\n{ids}idp_hosts = {}\n",
                toml::Value::String(self.upn.clone()),
                toml::Value::Array(hosts),
            );
            let mut staging = path.as_os_str().to_owned();
            staging.push(format!(".{}.new", std::process::id()));
            let staging = PathBuf::from(staging);
            let written = (|| {
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&staging)?;
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
}

/// Why the personal build's files or fence couldn't be used.
#[derive(Debug, thiserror::Error)]
pub enum PersonalError {
    #[error("HOME is unset or relative, so the personal build's directories can't be found")]
    NoHome,
    #[error("can't read {}: {source}", path.display())]
    Read { path: PathBuf, source: io::Error },
    #[error("can't write {}: {source}", path.display())]
    Write { path: PathBuf, source: io::Error },
    #[error("account.toml: {0}")]
    Account(String),
    #[error("no account signed in yet (account.toml has no object id): sign in first")]
    NoAccount,
    #[error("the account is one of the lab's (lab.toml names it): the personal build never signs in a lab account")]
    LabAccount,
    #[error("lab.toml can't be read: {0}")]
    Lab(String),
    #[error(
        "the personal build's data dir {} overlaps the lab's {}: the personal build never uses the lab's data",
        personal.display(),
        lab.display()
    )]
    LabDataDir { personal: PathBuf, lab: PathBuf },
    #[error("can't open {}: {source}", path.display())]
    Log { path: PathBuf, source: io::Error },
}

impl Fence {
    /// The personal build's fence (see the module docs): its account from `paths`' `account.toml`, its decisions to
    /// `paths`' `fence.log`. Refused for a data dir that overlaps the lab's or an account `lab.toml` names.
    ///
    /// # Errors
    /// [`PersonalError::NoAccount`] before the first sign-in; otherwise when the account, the lab's files or the log
    /// can't be read, or a keep-apart check refuses.
    pub fn open_personal(paths: &PersonalPaths) -> Result<Self, PersonalError> {
        let lab = LabLocations::from_env().map_err(|_| PersonalError::NoHome)?;
        Self::open_personal_apart(paths, &lab)
    }

    /// [`Fence::open_personal`] against the lab's locations `lab`.
    fn open_personal_apart(paths: &PersonalPaths, lab: &LabLocations) -> Result<Self, PersonalError> {
        paths.check_apart(lab)?;
        let account = PersonalAccount::load(paths)?.ok_or(PersonalError::NoAccount)?;
        account.check_not_lab(lab)?;
        let own = account.mri().ok_or(PersonalError::NoAccount)?;
        paths.make_dirs().map_err(|source| PersonalError::Write {
            path: paths.data_dir().to_path_buf(),
            source,
        })?;
        let path = paths.fence_log();
        let log = FenceLog::open(&path).map_err(|source| PersonalError::Log { path, source })?;
        Ok(Self::of_personal(own, log))
    }

    /// [`Fence::open_personal`] against given lab locations. Tests only (`test-support`).
    ///
    /// # Errors
    /// As [`Fence::open_personal`].
    #[cfg(feature = "test-support")]
    pub fn open_personal_with(paths: &PersonalPaths, lab: &LabLocations) -> Result<Self, PersonalError> {
        Self::open_personal_apart(paths, lab)
    }

    /// The personal fence's verdict on `target` (see the module docs).
    pub(crate) async fn judge_personal(&self, target: &Target, source: &dyn MembershipSource) -> Verdict {
        if target.kind() == WriteKind::OwnAvailability || matches!(target.scope(), Scope::Account { .. }) {
            return self.judge_own_personal(target);
        }
        if !PERSONAL_KINDS.contains(&target.kind()) {
            return Verdict::Deny(DenyReason::NotPersonalKind);
        }
        if !target.added().is_empty() {
            return Verdict::Deny(DenyReason::AddsParticipants);
        }
        let Some(own) = self.own_account() else {
            return Verdict::Deny(DenyReason::NotOwnAccount);
        };
        let id = match target.scope() {
            Scope::New => return Verdict::Deny(DenyReason::NotCreatable),
            Scope::Conversation { id } => id,
            _ => return Verdict::Deny(DenyReason::NotPersonalScope),
        };
        match one_to_one_pair(id) {
            Some(pair) if !pair.iter().any(|member| same(member, own)) => {
                return Verdict::Deny(DenyReason::NotAMember);
            }
            Some(_) => {}
            None if is_group_chat_id(id) => {}
            None => return Verdict::Deny(DenyReason::NotPersonalScope),
        }
        match source.members(target.scope()).await {
            Membership::Listed(members) if members.iter().any(|member| same(member, own)) => Verdict::Allow,
            Membership::Listed(_) => Verdict::Deny(DenyReason::NotAMember),
            Membership::Partial(reason) => Verdict::Deny(DenyReason::Partial(reason)),
            // only the personal source's listing answers the personal fence's question
            Membership::Complete(_) | Membership::OrgWide => {
                Verdict::Deny(DenyReason::Partial(PartialReason::Unsupported))
            }
        }
    }

    /// The account's own availability: only on the account itself, the fence's own, adding nobody.
    fn judge_own_personal(&self, target: &Target) -> Verdict {
        if target.kind() != WriteKind::OwnAvailability {
            return Verdict::Deny(DenyReason::AccountScope);
        }
        let own = match target.scope() {
            Scope::Account { mri } => self.own_account().is_some_and(|own| own.as_str() == mri),
            _ => false,
        };
        if own && target.added().is_empty() {
            Verdict::Allow
        } else {
            Verdict::Deny(DenyReason::NotOwnAccount)
        }
    }
}

/// The same MRI, case aside (an object id is a GUID).
fn same(one: &Mri, other: &Mri) -> bool {
    one.as_str().eq_ignore_ascii_case(other.as_str())
}

/// The personal fence's membership source: a 1:1 or group chat's thread, read fresh from the chat service on every
/// check ([`LiveMembershipSource`]'s client, credential and bounds), judged by [`crate::parse_listing`]. Every other
/// scope is [`PartialReason::Unsupported`], answered without a read.
#[derive(Debug)]
pub struct PersonalMembershipSource {
    live: LiveMembershipSource,
}

impl PersonalMembershipSource {
    /// The source of the personal fence `fence`, with no credential yet.
    ///
    /// # Errors
    /// When the HTTPS client can't be built.
    pub fn for_fence(_fence: &Fence) -> Result<Self, LiveSourceError> {
        Ok(Self {
            live: LiveMembershipSource::new()?,
        })
    }

    /// A source whose reads go to `origin` (a local mock). Tests only (`test-support`).
    ///
    /// # Errors
    /// When the client can't be built.
    #[cfg(feature = "test-support")]
    pub fn with_mock_origin(origin: &str) -> Result<Self, LiveSourceError> {
        Ok(Self {
            live: LiveMembershipSource::with_mock_origin(origin)?,
        })
    }

    /// Asks `source` for the credential on every read (the native transport, over the account's session).
    pub fn set_credential_source(&self, source: Arc<dyn CredentialSource>) {
        self.live.set_credential_source(source);
    }

    /// Reads with `credential` from now on. Tests only (`test-support`): the personal build reads with its session's
    /// token ([`Self::set_credential_source`]).
    #[cfg(feature = "test-support")]
    pub fn set_credential(&self, credential: crate::ChatCredential) {
        self.live.set_credential(credential);
    }
}

impl MembershipSource for PersonalMembershipSource {
    fn members<'a>(&'a self, scope: &'a Scope) -> MembershipFuture<'a> {
        Box::pin(async move {
            match scope {
                Scope::Conversation { id } if one_to_one_pair(id).is_some() || is_group_chat_id(id) => {
                    self.live.listing(id).await
                }
                _ => Membership::Partial(PartialReason::Unsupported),
            }
        })
    }
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
