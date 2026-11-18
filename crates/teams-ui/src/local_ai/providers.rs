//! The AI providers the user can pick for each AI work: the command-line tools on this computer (Claude Code, the
//! Codex CLI) and the APIs that the app asks with a key (Anthropic, OpenAI, and any OpenAI-compatible server).
//!
//! - **The list** is the settings' ([`crate::settings::Settings::ai_providers`]), in the user's order, each with a
//!   name and an on/off switch. A command-line tool found on the `PATH` that the list does not hold is added to the
//!   end of it, turned on ([`effective`]): Claude Code before Codex. Thus a settings file of an earlier build, which
//!   has no list, gets the order that its Automatic had.
//! - **Automatic** is the first provider in that list that is on and can work: a tool that is installed, or an API
//!   with its key (an OpenAI-compatible server can take no key).
//! - **The keys** are only in the system keyring ([`AiKeys`]), under the provider's id. The settings file keeps only
//!   that a key is there ([`Provider::has_key`]). The key is read on the thread that asks the API, never in a view.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use gpui::{App, AsyncApp, BorrowAppContext as _, Global};
use teams_auth::secrets::Secret;
use toml::{Table, Value};

use super::catalog::Listed;
use super::{Backend, Failure, Model, Target, ToolChoice, Tools, Work, api};

/// The longest provider id kept, in bytes.
const MAX_ID: usize = 64;
/// The longest provider name kept, in bytes.
pub const MAX_NAME: usize = 100;
/// The settings file's key of the list: an array of tables.
pub const FILE_KEY: &str = "ai_providers";
/// What a provider's `key` says when its key is in the keyring: the only thing the file keeps of a key.
const IN_KEYRING: &str = "keyring";

/// One AI provider of the settings.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Provider {
    /// Its id in the settings: a command-line tool's is its kind's key (`claude`, `codex`), an API's is made when it
    /// is added ([`Provider::new_api`]).
    pub id: String,
    pub kind: Backend,
    /// The name the user gave it.
    pub name: String,
    /// The user turned it on.
    pub enabled: bool,
    /// An API's base URL; `None`: its kind's own ([`default_base_url`]).
    pub base_url: Option<String>,
    /// An API's own default model: what "its default" asks for, and for an OpenAI-compatible server also what
    /// "Recommended" asks for.
    pub model: Option<String>,
    /// A key for it is in the keyring.
    pub has_key: bool,
}

impl Provider {
    /// The command-line tool `backend`, turned on.
    #[must_use]
    pub fn cli(backend: Backend) -> Self {
        Self {
            id: backend.key().to_owned(),
            kind: backend,
            name: backend.name().to_owned(),
            enabled: true,
            base_url: None,
            model: None,
            has_key: false,
        }
    }

    /// A new API provider of `kind` with an id that none in `list` has, turned on.
    #[must_use]
    pub fn new_api(kind: Backend, list: &[Self]) -> Self {
        // one of these is free: the list has fewer ids
        let id = (1..=list.len() + 1)
            .map(|at| format!("api-{at}"))
            .find(|id| !list.iter().any(|provider| provider.id == *id))
            .unwrap_or_default();
        Self {
            id,
            kind,
            name: kind.name().to_owned(),
            enabled: true,
            base_url: None,
            model: None,
            has_key: false,
        }
    }

    /// The base URL its requests go to.
    #[must_use]
    pub fn base_url(&self) -> String {
        self.base_url
            .clone()
            .unwrap_or_else(|| default_base_url(self.kind).to_owned())
    }

    /// Whether it can do a work now, when it is on: a tool installed (in `tools`), an API with a key (an
    /// OpenAI-compatible server with a base URL is enough).
    #[must_use]
    pub fn ready(&self, tools: &Tools) -> bool {
        match self.kind {
            Backend::Claude | Backend::Codex => tools.program(self.kind).is_some(),
            Backend::Anthropic | Backend::OpenAi => self.has_key,
            Backend::Compatible => self.base_url.is_some(),
        }
    }
}

/// The base URL an API kind uses when the user gives none.
#[must_use]
pub fn default_base_url(kind: Backend) -> &'static str {
    match kind {
        Backend::Anthropic => "https://api.anthropic.com",
        Backend::OpenAi => "https://api.openai.com/v1",
        Backend::Claude | Backend::Codex | Backend::Compatible => "",
    }
}

/// Whether `id` has the shape of a provider's id: letters, digits, `-` and `_`, at most [`MAX_ID`] bytes.
#[must_use]
pub fn id_shaped(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

/// Whether `url` can be an API's base URL: `http` or `https` with a host, at most 2048 bytes.
#[must_use]
pub fn base_url_shaped(url: &str) -> bool {
    url.len() <= 2048
        && url::Url::parse(url)
            .is_ok_and(|parsed| matches!(parsed.scheme(), "http" | "https") && parsed.host().is_some())
}

/// The list `configured` (the settings'), with each command-line tool in `tools` that it does not hold added to its
/// end, turned on (see the module docs).
#[must_use]
pub fn effective(configured: &[Provider], tools: &Tools) -> Vec<Provider> {
    let mut list = configured.to_vec();
    for backend in tools.installed() {
        if !list.iter().any(|provider| provider.kind == backend) {
            list.push(Provider::cli(backend));
        }
    }
    list
}

/// The providers of the settings file's `value` (see [`FILE_KEY`]), and whether some could not be read (those are
/// left out, as is a second provider of one id or one command-line tool).
#[must_use]
pub fn from_value(value: &Value) -> (Vec<Provider>, bool) {
    let Some(entries) = value.as_array() else {
        return (Vec::new(), true);
    };
    let mut list: Vec<Provider> = Vec::new();
    let mut bad = false;
    for entry in entries {
        match entry.as_table().and_then(provider_of) {
            Some(provider)
                if !list
                    .iter()
                    .any(|held| held.id == provider.id || (provider.kind.is_cli() && held.kind == provider.kind)) =>
            {
                list.push(provider);
            }
            _ => bad = true,
        }
    }
    (list, bad)
}

fn provider_of(table: &Table) -> Option<Provider> {
    let text = |key: &str| table.get(key).and_then(Value::as_str);
    let id = text("id").filter(|id| id_shaped(id))?.to_owned();
    let kind = Backend::from_key(text("kind")?)?;
    if kind.is_cli() && id != kind.key() {
        return None;
    }
    let name = text("name")
        .map(str::trim)
        .filter(|name| !name.is_empty() && name.len() <= MAX_NAME)?;
    let enabled = table.get("enabled").map_or(Some(true), Value::as_bool)?;
    let base_url = match text("base_url") {
        Some(url) if base_url_shaped(url) && !kind.is_cli() => Some(url.to_owned()),
        Some(_) => return None,
        None => None,
    };
    let model = match text("model") {
        Some(model) => match super::Model::from_key(model) {
            Some(super::Model::Named(model)) if !kind.is_cli() => Some(model),
            _ => return None,
        },
        None => None,
    };
    let has_key = match text("key") {
        Some(IN_KEYRING) => !kind.is_cli(),
        Some(_) => return None,
        None => false,
    };
    Some(Provider {
        id,
        kind,
        name: name.to_owned(),
        enabled,
        base_url,
        model,
        has_key,
    })
}

/// `list` as the settings file keeps it: never a key, only that one is in the keyring.
#[must_use]
pub fn to_value(list: &[Provider]) -> Value {
    Value::Array(
        list.iter()
            .map(|provider| {
                let mut table = Table::new();
                table.insert("id".to_owned(), Value::String(provider.id.clone()));
                table.insert("kind".to_owned(), Value::String(provider.kind.key().to_owned()));
                table.insert("name".to_owned(), Value::String(provider.name.clone()));
                table.insert("enabled".to_owned(), Value::Boolean(provider.enabled));
                if let Some(url) = &provider.base_url {
                    table.insert("base_url".to_owned(), Value::String(url.clone()));
                }
                if let Some(model) = &provider.model {
                    table.insert("model".to_owned(), Value::String(model.clone()));
                }
                if provider.has_key {
                    table.insert("key".to_owned(), Value::String(IN_KEYRING.to_owned()));
                }
                Value::Table(table)
            })
            .collect(),
    )
}

/// The app's list of providers, as the settings last held it ([`crate::settings::SettingsModel`] keeps it current).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Providers(pub Vec<Provider>);

impl Global for Providers {}

impl Providers {
    /// The app's list with the tools found added ([`effective`]).
    pub fn of(cx: &mut App) -> Vec<Provider> {
        let tools = Tools::of(cx);
        let configured = cx.try_global::<Self>().map(|held| held.0.clone()).unwrap_or_default();
        effective(&configured, &tools)
    }

    /// Makes `list` the app's, when it changed.
    pub fn publish(list: &[Provider], cx: &mut App) {
        if cx.try_global::<Self>().is_some_and(|held| held.0 == list) {
            return;
        }
        cx.set_global(Self(list.to_vec()));
    }
}

/// What a pick came to: the provider's kind, its name, and where its work goes.
#[derive(Clone, Debug)]
pub struct Picked {
    pub backend: Backend,
    pub name: String,
    pub target: Target,
}

impl Picked {
    /// What a view says the work runs with, `work` asking `model`.
    #[must_use]
    pub fn label(&self, work: Work, model: &Model) -> String {
        let Target::Api(target) = &self.target else {
            return self.backend.label_of(work, model);
        };
        match api::model_for(self.backend, work, model, target.model.as_deref()) {
            Ok(asked) if self.backend.recommended(work) == Some(asked.as_str()) => {
                format!("{} ({})", self.name, self.backend.label(work))
            }
            Ok(asked) => format!("{} ({asked})", self.name),
            Err(_) => self.name.clone(),
        }
    }
}

/// Where an API's requests go and what reads its key, for the thread that asks it.
#[derive(Clone, Debug)]
pub struct ApiTarget {
    /// The provider's id: its key's name in the keyring.
    pub provider: String,
    /// The name the user gave the provider, for the words of its failures.
    pub name: String,
    pub base_url: String,
    /// The provider's own default model.
    pub model: Option<String>,
    pub has_key: bool,
    pub keys: AiKeys,
}

impl ApiTarget {
    /// The key of the provider, read from the keyring (this blocks: call it off the UI thread).
    ///
    /// # Errors
    /// The kind must have a key and none is stored, or the keyring cannot be read.
    pub fn key(&self, backend: Backend) -> Result<Option<Secret>, Failure> {
        if !self.has_key {
            return if backend == Backend::Compatible {
                Ok(None)
            } else {
                Err(Failure::NoKey(backend))
            };
        }
        match self.keys.get(&self.provider) {
            Ok(Some(key)) => Ok(Some(key)),
            Ok(None) => Err(Failure::NoKey(backend)),
            Err(why) => Err(Failure::Stopped(
                backend,
                Some(format!("the keyring could not be read: {why}")),
            )),
        }
    }
}

/// The provider `choice` comes to, with `list` the providers ([`effective`]).
///
/// # Errors
/// No provider can do the work (Automatic), or the one chosen is gone, turned off, not installed or without a key.
pub fn pick_from(list: &[Provider], tools: &Tools, keys: &AiKeys, choice: &ToolChoice) -> Result<Picked, Failure> {
    let provider = match choice {
        ToolChoice::Automatic => list
            .iter()
            .find(|provider| provider.enabled && provider.ready(tools))
            .ok_or(Failure::NotInstalled(None))?,
        ToolChoice::Only(id) => match list.iter().find(|provider| provider.id == *id) {
            Some(provider) => provider,
            None => {
                return Err(Backend::from_key(id)
                    .filter(|backend| backend.is_cli())
                    .map_or(Failure::NoProvider, |backend| Failure::NotInstalled(Some(backend))));
            }
        },
    };
    let backend = provider.kind;
    if !provider.enabled {
        return Err(Failure::TurnedOff(backend).of(&provider.name));
    }
    let target = if backend.is_cli() {
        Target::Program(tools.program(backend).ok_or(Failure::NotInstalled(Some(backend)))?)
    } else {
        if backend != Backend::Compatible && !provider.has_key {
            return Err(Failure::NoKey(backend).of(&provider.name));
        }
        Target::Api(ApiTarget {
            provider: provider.id.clone(),
            name: provider.name.clone(),
            base_url: provider.base_url(),
            model: provider.model.clone(),
            has_key: provider.has_key,
            keys: keys.clone(),
        })
    };
    Ok(Picked {
        backend,
        name: provider.name.clone(),
        target,
    })
}

/// The provider `choice` comes to in the app (see [`pick_from`]).
///
/// # Errors
/// As [`pick_from`].
pub fn pick(choice: &ToolChoice, cx: &mut App) -> Result<Picked, Failure> {
    let list = Providers::of(cx);
    let tools = Tools::of(cx);
    let keys = AiKeys::of(cx);
    pick_from(&list, &tools, &keys, choice)
}

/// What keeps the providers' keys: the system keyring in the app, the memory in tests and the sandbox.
pub trait KeyVault: Send + Sync {
    /// The key of `provider`.
    ///
    /// # Errors
    /// The keyring cannot be read; the error never holds the key.
    fn get(&self, provider: &str) -> Result<Option<Secret>, String>;
    /// Keeps `key` for `provider`, in place of one there.
    ///
    /// # Errors
    /// The keyring cannot be written.
    fn put(&self, provider: &str, key: &Secret) -> Result<(), String>;
    /// Forgets the key of `provider` (none there is not an error).
    ///
    /// # Errors
    /// The keyring cannot be written.
    fn delete(&self, provider: &str) -> Result<(), String>;
}

/// Keys kept in this process only.
#[derive(Default)]
pub struct MemoryKeys(Mutex<HashMap<String, Secret>>);

impl KeyVault for MemoryKeys {
    fn get(&self, provider: &str) -> Result<Option<Secret>, String> {
        Ok(self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(provider)
            .cloned())
    }

    fn put(&self, provider: &str, key: &Secret) -> Result<(), String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(provider.to_owned(), key.clone());
        Ok(())
    }

    fn delete(&self, provider: &str) -> Result<(), String> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).remove(provider);
        Ok(())
    }
}

/// The app's keys of the AI providers: an app global, the memory's until the app gives its keyring ([`AiKeys::new`]).
#[derive(Clone)]
pub struct AiKeys(Arc<dyn KeyVault>);

impl Global for AiKeys {}

impl fmt::Debug for AiKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AiKeys(<vault>)")
    }
}

impl Default for AiKeys {
    fn default() -> Self {
        Self::new(Arc::new(MemoryKeys::default()))
    }
}

impl AiKeys {
    #[must_use]
    pub fn new(vault: Arc<dyn KeyVault>) -> Self {
        Self(vault)
    }

    /// The app's.
    pub fn of(cx: &mut App) -> Self {
        cx.default_global::<Self>().clone()
    }

    /// The key of `provider`.
    ///
    /// # Errors
    /// As [`KeyVault::get`].
    pub fn get(&self, provider: &str) -> Result<Option<Secret>, String> {
        self.0.get(provider)
    }

    /// Keeps `key` for `provider`.
    ///
    /// # Errors
    /// As [`KeyVault::put`].
    pub fn put(&self, provider: &str, key: &Secret) -> Result<(), String> {
        self.0.put(provider, key)
    }

    /// Forgets the key of `provider`.
    ///
    /// # Errors
    /// As [`KeyVault::delete`].
    pub fn delete(&self, provider: &str) -> Result<(), String> {
        self.0.delete(provider)
    }
}

/// What an API listed of its models, or why it could not.
pub type Listing = Result<Vec<Listed>, Failure>;

/// The models each API provider lists (`/v1/models`), by the provider's id: an app global, asked in the background on
/// first use ([`ApiModels::of`]) and again on [`ApiModels::relist`] (its key, base URL or kind changed).
#[derive(Clone, Debug, Default)]
pub struct ApiModels {
    listed: HashMap<String, Listing>,
    /// The providers asked now, with what was asked: a listing of a provider changed since is stale.
    asking: HashMap<String, Provider>,
}

impl Global for ApiModels {}

impl ApiModels {
    /// What `provider` listed; `None` while it is asked, which starts on first use.
    pub fn of(provider: &Provider, cx: &mut App) -> Option<Listing> {
        // only read: a mutable access tells the observers, and a view that draws this observes it
        if let Some(held) = cx.try_global::<Self>() {
            if let Some(listing) = held.listed.get(&provider.id) {
                return Some(listing.clone());
            }
            if held.asking.contains_key(&provider.id) {
                return None;
            }
        }
        Self::relist(provider, cx);
        None
    }

    /// Asks `provider` for its models again, in the background.
    pub fn relist(provider: &Provider, cx: &mut App) {
        let held = cx.default_global::<Self>();
        held.listed.remove(&provider.id);
        held.asking.insert(provider.id.clone(), provider.clone());
        let asked = provider.clone();
        let keys = AiKeys::of(cx);
        let listing = cx
            .background_executor()
            .spawn(async move { api::list_models(&asked, &keys) });
        let asked = provider.clone();
        cx.spawn(async move |cx: &mut AsyncApp| {
            let listing = listing.await;
            cx.update(|cx| Self::listed(asked, listing, cx));
        })
        .detach();
    }

    /// Keeps what `asked` listed, unless it was asked again since.
    fn listed(asked: Provider, listing: Listing, cx: &mut App) {
        cx.default_global::<Self>();
        cx.update_global::<Self, _>(|held, _| {
            if held.asking.get(&asked.id) == Some(&asked) {
                held.asking.remove(&asked.id);
                held.listed.insert(asked.id, listing);
            }
        });
    }

    /// Forgets what `provider` listed (it was removed).
    pub fn forget(provider: &str, cx: &mut App) {
        let held = cx.default_global::<Self>();
        held.listed.remove(provider);
        held.asking.remove(provider);
    }
}

#[cfg(test)]
#[path = "../../tests/support/local_ai_providers_units.rs"]
mod tests;
