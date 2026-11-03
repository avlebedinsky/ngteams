//! The accounts of an app that runs several at once (the personal build with the user's accounts, the lab app with
//! several lab accounts): each its own engine, models, notifications and own presence, all live at once, one of them
//! shown in the window.
//!
//! [`Accounts`] is the GPUI entity every account's shell shares. Its rail shows each account's avatar above Settings
//! (its picture, else its initials), the shown one marked and carrying the account's own presence dot, another one
//! with its unread total as a badge (from its [`Tray`] count) and a mark when it is signed out; a click on another
//! account switches to it at once ([`AccountsEvent::Switched`], which the window acts on: its shell is kept, so it
//! comes back as it was left), a click on the shown one opens its account menu (the shell's). The "+" under the
//! avatars asks the app to add an account ([`AccountsEvent::AddAsked`], only in an app that can:
//! [`Accounts::can_add`]), and the account menu's "Sign out and remove" asks to remove one
//! ([`AccountsEvent::RemoveAsked`]). The app does what is asked (a sign-in, an engine started or stopped) and tells
//! the model ([`Accounts::add`], [`Accounts::remove`]).
//!
//! Calls are one at a time across the accounts (`teams_core::CallFloor` decides it for the writes): an account's
//! shell offers no call while another account has one up or ringing ([`Accounts::busy_elsewhere`]).
//!
//! Traced: `account added`, `account removed` and `account switched`, each with the account's MRI, and `account
//! add-asked`.

use std::sync::Arc;

use gpui::{App, AsyncApp, Context, Entity, EventEmitter, SharedString, Subscription, WeakEntity};

use crate::bridge::Core;
use crate::models::{Account, Models};
use crate::own_presence::OwnPresence;
use crate::people::{Picture, Shown};
use crate::trace::{TraceLine, ViewTrace};
use crate::tray::Tray;

/// What the app knows of an account when it adds it.
#[derive(Clone)]
pub struct AccountParts {
    /// The account's engine's request side; its MRI ([`Core::me`]) is the account's key.
    pub core: Arc<dyn Core>,
    pub models: Models,
    /// The name it is shown by (its display name, else what the app has).
    pub name: SharedString,
    /// What tells it apart beside the name: its UPN, or the lab's alias.
    pub detail: SharedString,
}

/// One account as the rail shows it.
pub struct AccountTile {
    pub key: String,
    pub name: SharedString,
    pub detail: SharedString,
    pub core: Arc<dyn Core>,
    pub models: Models,
    /// Its unread total, counted as the tray counts it.
    pub tray: Entity<Tray>,
    /// Its own presence, once the desktop made it.
    pub own_presence: Option<Entity<OwnPresence>>,
    /// Its own picture, once the engine had it.
    pub picture: Option<Picture>,
    /// The profile revision its picture was asked at.
    named_at: Option<u64>,
    _observing: Vec<Subscription>,
}

impl AccountTile {
    /// Whether the account is signed out.
    #[must_use]
    pub fn signed_out(&self, cx: &App) -> bool {
        matches!(self.models.status.read(cx).account, Account::SignedOut { .. })
    }

    /// Its unread total as last counted (0 before the first count).
    #[must_use]
    pub fn unread(&self, cx: &App) -> u64 {
        self.tray.read(cx).total().unwrap_or(0)
    }

    /// Whether it has a call up, ringing or being set up.
    #[must_use]
    pub fn in_call(&self, cx: &App) -> bool {
        self.models.calls.read(cx).busy()
    }
}

/// What the accounts ask of the app and the window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccountsEvent {
    /// The account of this MRI is shown now.
    Switched(String),
    /// The user asked to add an account.
    AddAsked,
    /// The user asked to sign out of the account of this MRI and remove it.
    RemoveAsked(String),
}

/// The accounts (see the module docs). A GPUI entity.
pub struct Accounts {
    tiles: Vec<AccountTile>,
    active: usize,
    can_add: bool,
    trace: Arc<ViewTrace>,
    /// Each account's own presence observed, by the account's MRI.
    own_observed: Vec<(String, Subscription)>,
}

impl EventEmitter<AccountsEvent> for Accounts {}

impl Accounts {
    /// No account yet; the "+" shown when `can_add`.
    #[must_use]
    pub fn new(can_add: bool, trace: Arc<ViewTrace>) -> Self {
        Self {
            tiles: Vec::new(),
            active: 0,
            can_add,
            trace,
            own_observed: Vec::new(),
        }
    }

    /// Whether the app adds accounts (the rail shows its "+", the account menu its "Sign out and remove").
    #[must_use]
    pub fn can_add(&self) -> bool {
        self.can_add
    }

    #[must_use]
    pub fn tiles(&self) -> &[AccountTile] {
        &self.tiles
    }

    /// The account shown.
    #[must_use]
    pub fn active(&self) -> Option<&AccountTile> {
        self.tiles.get(self.active)
    }

    /// The MRI of the account shown.
    #[must_use]
    pub fn active_key(&self) -> Option<&str> {
        self.active().map(|tile| tile.key.as_str())
    }

    #[must_use]
    pub fn tile(&self, key: &str) -> Option<&AccountTile> {
        self.tiles.iter().find(|tile| tile.key == key)
    }

    /// Adds an account after the others (one already there is left as it is); the first one added is shown. Its picture
    /// is asked for at once and again when its profile changes.
    pub fn add(&mut self, parts: AccountParts, tray: Entity<Tray>, cx: &mut Context<Self>) {
        let key = parts.core.me().to_owned();
        if self.tile(&key).is_some() {
            return;
        }
        let observing = vec![
            cx.observe(&parts.models.chats, |this: &mut Self, _, cx| this.ask_pictures(cx)),
            cx.observe(&parts.models.status, |_, _, cx| cx.notify()),
            cx.observe(&parts.models.calls, |_, _, cx| cx.notify()),
            cx.observe(&tray, |_, _, cx| cx.notify()),
        ];
        self.trace
            .record(&TraceLine::new("account", "added").people([key.as_str()]));
        self.tiles.push(AccountTile {
            key,
            name: parts.name,
            detail: parts.detail,
            core: parts.core,
            models: parts.models,
            tray,
            own_presence: None,
            picture: None,
            named_at: None,
            _observing: observing,
        });
        self.ask_pictures(cx);
        cx.notify();
    }

    /// Hands account `key` its own presence (the desktop makes one per account).
    pub fn set_own_presence(&mut self, key: &str, own: Entity<OwnPresence>, cx: &mut Context<Self>) {
        if let Some(tile) = self.tiles.iter_mut().find(|tile| tile.key == key) {
            let observing = cx.observe(&own, |_, _, cx| cx.notify());
            self.own_observed.push((key.to_owned(), observing));
            tile.own_presence = Some(own);
            cx.notify();
        }
    }

    /// Takes account `key` out; when it was shown, the one before it (else the first) is shown instead and told
    /// ([`AccountsEvent::Switched`]).
    pub fn remove(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(index) = self.tiles.iter().position(|tile| tile.key == key) else {
            return;
        };
        let was_active = index == self.active;
        self.tiles.remove(index);
        self.own_observed.retain(|(observed, _)| observed != key);
        self.trace.record(&TraceLine::new("account", "removed").people([key]));
        if index < self.active || (was_active && self.active > 0) {
            self.active -= 1;
        }
        if was_active && let Some(shown) = self.active_key().map(str::to_owned) {
            self.trace
                .record(&TraceLine::new("account", "switched").people([shown.as_str()]));
            cx.emit(AccountsEvent::Switched(shown));
        }
        cx.notify();
    }

    /// Shows account `key`; nothing when it is shown already or isn't one.
    pub fn switch(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(index) = self.tiles.iter().position(|tile| tile.key == key) else {
            return;
        };
        if index == self.active {
            return;
        }
        self.active = index;
        self.trace.record(&TraceLine::new("account", "switched").people([key]));
        cx.emit(AccountsEvent::Switched(key.to_owned()));
        cx.notify();
    }

    /// Shows the next account (`forward`) or the one before, round.
    pub fn cycle(&mut self, forward: bool, cx: &mut Context<Self>) {
        let count = self.tiles.len();
        if count < 2 {
            return;
        }
        let next = if forward {
            (self.active + 1) % count
        } else {
            (self.active + count - 1) % count
        };
        let key = self.tiles[next].key.clone();
        self.switch(&key, cx);
    }

    /// The user asked to add an account.
    pub fn ask_add(&mut self, cx: &mut Context<Self>) {
        if self.can_add {
            self.trace.record(&TraceLine::new("account", "add-asked"));
            cx.emit(AccountsEvent::AddAsked);
        }
    }

    /// The user asked to sign out of account `key` and remove it.
    pub fn ask_remove(&mut self, key: &str, cx: &mut Context<Self>) {
        if self.can_add && self.tile(key).is_some() {
            cx.emit(AccountsEvent::RemoveAsked(key.to_owned()));
        }
    }

    /// Whether an account other than `key` has a call up, ringing or being set up: `key`'s shell offers none then.
    #[must_use]
    pub fn busy_elsewhere(&self, key: &str, cx: &App) -> bool {
        self.tiles.iter().any(|tile| tile.key != key && tile.in_call(cx))
    }

    /// Account `key`'s own picture (and its name, when a profile gave it), as the engine had them.
    fn pictured(&mut self, key: &str, shown: Shown, cx: &mut Context<Self>) {
        let Some(tile) = self.tiles.iter_mut().find(|tile| tile.key == key) else {
            return;
        };
        if let Some(name) = shown.name.clone().filter(|_| shown.profiled()) {
            tile.name = name;
        }
        tile.picture = shown.picture;
        cx.notify();
    }

    /// Asks each account's engine for its own picture when its profile changed since it was last asked.
    fn ask_pictures(&mut self, cx: &mut Context<Self>) {
        for tile in &mut self.tiles {
            let at = tile.models.chats.read(cx).named_at(&tile.key);
            if tile.named_at == Some(at) {
                continue;
            }
            tile.named_at = Some(at);
            let asking = tile.core.person(tile.key.clone());
            let key = tile.key.clone();
            cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let Ok(person) = asking.await else { return };
                let _ = this.update(cx, |this, cx| this.pictured(&key, Shown::of(person), cx));
            })
            .detach();
        }
    }
}
