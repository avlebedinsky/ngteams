//! Which chat a command means, and the lab accounts' names for the MRIs it prints.

use teams_fence::lab::{Alias, LabConfig};
use teams_fence::one_to_one_id;

/// The lab accounts' MRIs, from `lab.toml` (each known after the account's first login), and its contacts' (the user's
/// own accounts the lab may contact), by name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LabNames {
    mris: [Option<String>; 3],
    contacts: Vec<(String, String)>,
}

impl LabNames {
    #[must_use]
    pub fn from_config(config: &LabConfig) -> Self {
        Self {
            mris: Alias::ALL.map(|alias| config.account(alias).mri.clone()),
            contacts: config
                .contacts()
                .iter()
                .map(|contact| (contact.name.clone(), contact.mri.clone()))
                .collect(),
        }
    }

    /// The MRIs of a, b and c, as given.
    #[must_use]
    pub fn new(mris: [Option<&str>; 3]) -> Self {
        Self {
            mris: mris.map(|mri| mri.map(str::to_owned)),
            contacts: Vec::new(),
        }
    }

    /// These names with a contact named `name`.
    #[must_use]
    pub fn with_contact(mut self, name: &str, mri: &str) -> Self {
        self.contacts.push((name.to_owned(), mri.to_owned()));
        self
    }

    /// The MRI of the contact `arg` names: its name, or `contact` when the lab has exactly one.
    #[must_use]
    pub fn contact(&self, arg: &str) -> Option<&str> {
        let named = self.contacts.iter().find(|(name, _)| name == arg);
        let only = match self.contacts.as_slice() {
            [only] if arg == "contact" => Some(only),
            _ => None,
        };
        named.or(only).map(|(_, mri)| mri.as_str())
    }

    #[must_use]
    pub fn mri(&self, alias: Alias) -> Option<&str> {
        self.mris[index(alias)].as_deref()
    }

    /// The lab account an MRI is (MRIs compared without case), if any.
    #[must_use]
    pub fn alias_of(&self, mri: &str) -> Option<Alias> {
        Alias::ALL
            .into_iter()
            .find(|alias| self.mri(*alias).is_some_and(|known| known.eq_ignore_ascii_case(mri)))
    }

    /// An MRI as printed: the lab account's alias, else the MRI itself.
    #[must_use]
    pub fn show(&self, mri: &str) -> String {
        self.alias_of(mri)
            .map_or_else(|| mri.to_owned(), |alias| alias.as_str().to_owned())
    }
}

fn index(alias: Alias) -> usize {
    match alias {
        Alias::A => 0,
        Alias::B => 1,
        Alias::C => 2,
    }
}

/// Why a command's chat argument names no chat.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ChatError {
    #[error("{0} has no 1:1 chat with itself")]
    Own(Alias),
    #[error("lab.toml has no MRI for {0} yet (run `proto-lab login {0}`)")]
    NoMri(Alias),
    #[error("the MRIs of {0} and {1} don't make a 1:1 chat id")]
    NoOneToOne(Alias, Alias),
    #[error("the lab's a–b–c group chat isn't recorded in chats.toml (run `proto-lab run abc-chat`)")]
    NoGroupChat,
    #[error("`{0}` is neither a, b, c, abc, a contact nor a conversation id")]
    NotAChat(String),
    #[error("the MRIs of {0} and the contact don't make a 1:1 chat id")]
    NoContactChat(Alias),
}

/// The conversation id `arg` means for the account `me`: `a`, `b` or `c` is `me`'s 1:1 chat with that account,
/// `abc` the lab's group chat (`abc`, from `chats.toml`), a contact's name (or `contact` when the lab has one) `me`'s
/// 1:1 chat with that contact, and anything with a `:` and no white space a conversation id as it is. The fence decides
/// whether a write to it may go out; this only names it.
///
/// # Errors
/// See [`ChatError`].
pub fn resolve(arg: &str, me: Alias, names: &LabNames, abc: Option<&str>) -> Result<String, ChatError> {
    if arg == "abc" {
        return abc.map(str::to_owned).ok_or(ChatError::NoGroupChat);
    }
    if let Ok(other) = arg.parse::<Alias>() {
        if other == me {
            return Err(ChatError::Own(me));
        }
        let mri = |alias| names.mri(alias).ok_or(ChatError::NoMri(alias));
        return one_to_one_id(mri(me)?, mri(other)?).ok_or(ChatError::NoOneToOne(me, other));
    }
    if let Some(contact) = names.contact(arg) {
        let own = names.mri(me).ok_or(ChatError::NoMri(me))?;
        return one_to_one_id(own, contact).ok_or(ChatError::NoContactChat(me));
    }
    if arg.contains(':') && !arg.contains(char::is_whitespace) {
        return Ok(arg.to_owned());
    }
    Err(ChatError::NotAChat(arg.to_owned()))
}
