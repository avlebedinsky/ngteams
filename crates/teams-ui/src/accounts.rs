//! An app's accounts: each with its own engine, models, notifications and own presence, all live at once, one of
//! them shown in the window. The desktop can add accounts; a lab alias keeps its single broker account.
//!
//! [`Accounts`] is the GPUI entity every account's shell shares. Its rail shows each account's avatar above Settings
//! (its picture, else its initials), the shown one marked and carrying the account's own presence dot, another one
//! with its unread total as a badge (from its [`Tray`] count) and a mark when it is signed out; a click on another
//! account switches to it at once ([`AccountsEvent::Switched`], which the window acts on: its shell is kept, so it
//! comes back as it was left), a click on the shown one opens its account menu (the shell's). The "+" under the
//! avatars asks the app to add an account ([`AccountsEvent::AddAsked`], only in an app that can:
//! [`Accounts::can_add`]), and the account menu's "Sign out and remove" asks to remove one
//! ([`AccountsEvent::RemoveAsked`], when [`Accounts::can_remove`]). The app does what is asked (a sign-in, an engine started or stopped) and tells
//! the model ([`Accounts::add`], [`Accounts::remove`]).
//!
//! Calls are one at a time across the accounts (`teams_core::CallFloor` decides it for the writes): an account's
//! shell offers no call while another account has one up or ringing ([`Accounts::busy_elsewhere`]).
//!
//! The user can choose a picture for each account ([`crate::account_picture`]): [`Accounts::change_picture`] and
//! [`Accounts::use_profile_picture`]. The rail, the title bar, the account menu and Settings > Accounts show it
//! ([`AccountTile::face`]) in place of the profile picture.
//!
//! Traced: `account added`, `account removed` and `account switched`, each with the account's MRI, and `account
//! add-asked`; `account picture-chosen`, `account picture-refused` and `account picture-reset`, each with the MRI.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    App, AppContext as _, AsyncApp, Context, Entity, EventEmitter, PathPromptOptions, SharedString, Subscription, Task,
    WeakEntity,
};

use crate::account_picture::{AccountPictures, PictureError, Turn};

use crate::app_admin::AppAdmin;
use crate::bridge::Core;
use crate::models::{Account, Models};
use crate::own_presence::OwnPresence;
use crate::people::{Picture, Shown};
use crate::trace::{TraceLine, ViewTrace};
use crate::tray::Tray;
use crate::user_admin::UserAdmin;

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
    /// Its own server (the host, with a port that isn't 443); `None` for a Microsoft 365 account.
    pub server: Option<SharedString>,
}

/// One account as the rail shows it.
pub struct AccountTile {
    pub key: String,
    pub name: SharedString,
    pub detail: SharedString,
    /// Its own server; `None` for a Microsoft 365 account.
    pub server: Option<SharedString>,
    /// Its server's account management, once the app found the account to be an administrator there
    /// ([`Accounts::set_admin`]); `None` for every other account.
    pub admin: Option<Arc<dyn UserAdmin>>,
    /// Its server's apps and bots, once the app found the account to be an administrator of a server that serves
    /// apps ([`Accounts::set_app_admin`]); `None` for every other account.
    pub app_admin: Option<Arc<dyn AppAdmin>>,
    pub core: Arc<dyn Core>,
    pub models: Models,
    /// Its unread total, counted as the tray counts it.
    pub tray: Entity<Tray>,
    /// Its own presence, once the desktop made it.
    pub own_presence: Option<Entity<OwnPresence>>,
    /// Its own picture, once the engine had it.
    pub picture: Option<Picture>,
    /// The picture the user chose for it ([`crate::account_picture`]); `None` shows its profile picture.
    pub chosen_picture: Option<Picture>,
    /// The profile revision its picture was asked at.
    named_at: Option<u64>,
    _observing: Vec<Subscription>,
}

impl AccountTile {
    /// The picture that shows which account it is: the one the user chose, else its profile picture.
    #[must_use]
    pub fn face(&self) -> Option<&Picture> {
        self.chosen_picture.as_ref().or(self.picture.as_ref())
    }

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
    can_remove: bool,
    trace: Arc<ViewTrace>,
    /// Each account's own presence observed, by the account's MRI.
    own_observed: Vec<(String, Subscription)>,
    /// Where the pictures the user chose are kept; `None`: no picture can be chosen.
    pictures: Option<AccountPictures>,
    /// The account whose last chosen picture can't be used, and why.
    picture_problem: Option<(String, SharedString)>,
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
            can_remove: can_add,
            trace,
            own_observed: Vec::new(),
            pictures: None,
            picture_problem: None,
        }
    }

    /// The accounts with the pictures the user chose kept by `pictures`; `None`: none can be chosen.
    #[must_use]
    pub fn with_pictures(mut self, pictures: Option<AccountPictures>) -> Self {
        self.pictures = pictures;
        self
    }

    /// Whether the user can choose a picture for an account.
    #[must_use]
    pub fn can_choose_pictures(&self) -> bool {
        self.pictures.is_some()
    }

    /// Why the last picture chosen for account `key` can't be used.
    #[must_use]
    pub fn picture_problem(&self, key: &str) -> Option<&SharedString> {
        self.picture_problem
            .as_ref()
            .filter(|(account, _)| account == key)
            .map(|(_, why)| why)
    }

    /// Whether the app adds accounts (the rail shows its "+").
    #[must_use]
    pub fn can_add(&self) -> bool {
        self.can_add
    }

    /// Whether an existing account may be signed out and removed.
    #[must_use]
    pub fn can_remove(&self) -> bool {
        self.can_remove
    }

    /// A single-account session may forget its account but cannot add another one.
    pub fn prevent_adding(&mut self, cx: &mut Context<Self>) {
        self.can_add = false;
        cx.notify();
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
            key: key.clone(),
            name: parts.name,
            detail: parts.detail,
            server: parts.server,
            admin: None,
            app_admin: None,
            core: parts.core,
            models: parts.models,
            tray,
            own_presence: None,
            picture: None,
            chosen_picture: None,
            named_at: None,
            _observing: observing,
        });
        self.ask_pictures(cx);
        self.load_chosen_picture(&key, cx);
        cx.notify();
    }

    /// Reads the picture the user chose for account `key` before, if any.
    fn load_chosen_picture(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(pictures) = self.pictures.clone() else {
            return;
        };
        let key = key.to_owned();
        let turn = pictures.turn(&key);
        let (account, reading) = (key.clone(), pictures.clone());
        let loading = cx.background_spawn(async move { reading.load(&account) });
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let Some(png) = loading.await else { return };
            let _ = this.update(cx, |this, cx| {
                // a picture chosen or reset since then is the last choice
                if !pictures.is_current(&key, turn) {
                    return;
                }
                if let Some(tile) = this.tiles.iter_mut().find(|tile| tile.key == key) {
                    tile.chosen_picture.get_or_insert_with(|| Picture::png(png));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// "Change picture" of account `key`: the user picks a picture file, and the account shows it from then on. The
    /// task gives why the picture can't be used, as [`Self::use_picture`] does.
    pub fn change_picture(&mut self, key: &str, cx: &mut Context<Self>) -> Task<Option<SharedString>> {
        if self.pictures.is_none() || self.tile(key).is_none() {
            return Task::ready(None);
        }
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Use picture".into()),
            title: Some("Choose a picture".into()),
            filters: crate::account_picture::file_filters(),
        });
        let key = key.to_owned();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let source = picked
                .await
                .ok()
                .and_then(Result::ok)
                .flatten()
                .and_then(|paths| paths.into_iter().next())?;
            let using = this.update(cx, |this, cx| this.use_picture(&key, source, cx)).ok()?;
            using.await
        })
    }

    /// Keeps the picture file at `source` as the picture of account `key` and shows it, or says why it can't. The task
    /// gives why it can't (also kept for [`Self::picture_problem`]); `None` when it is used, or when a later choice of
    /// the user came first.
    pub fn use_picture(&mut self, key: &str, source: PathBuf, cx: &mut Context<Self>) -> Task<Option<SharedString>> {
        let Some(pictures) = self.pictures.clone().filter(|_| self.tile(key).is_some()) else {
            return Task::ready(None);
        };
        let key = key.to_owned();
        let turn = pictures.turn(&key);
        let account = key.clone();
        let saving = cx.background_spawn(async move { pictures.save(&account, &source, turn) });
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let saved = saving.await;
            this.update(cx, |this, cx| this.picture_saved(&key, turn, saved, cx))
                .ok()
                .flatten()
        })
    }

    /// The end of a save of turn `turn`; returns why the picture can't be used.
    fn picture_saved(
        &mut self,
        key: &str,
        turn: Turn,
        saved: Result<Option<Vec<u8>>, PictureError>,
        cx: &mut Context<Self>,
    ) -> Option<SharedString> {
        let pictures = self.pictures.as_ref()?;
        if !pictures.is_current(key, turn) {
            // a later choice of the user, or the removal of the account, came first: it decides
            return None;
        }
        let Some(tile) = self.tiles.iter_mut().find(|tile| tile.key == key) else {
            // the account left the window while its picture was saved: its file goes too
            if matches!(saved, Ok(Some(_))) {
                self.forget_picture(key);
            }
            return None;
        };
        match saved {
            Ok(None) => None,
            Ok(Some(png)) => {
                tile.chosen_picture = Some(Picture::png(png));
                self.picture_problem = None;
                self.trace
                    .record(&TraceLine::new("account", "picture-chosen").people([key]));
                cx.notify();
                None
            }
            Err(error) => {
                log::warn!("a picture for an account can't be used: {error}");
                let why: SharedString = match error {
                    PictureError::TooLarge(_) => "That picture is too large. Choose one smaller than 64 MB.".into(),
                    PictureError::Unopened(error) => format!("That file can't be opened: {error}").into(),
                    PictureError::Unreadable(_) => {
                        "That file can't be used. Choose a PNG, JPEG, GIF or WebP picture.".into()
                    }
                    PictureError::Unsaved(error) => format!("The picture can't be saved: {error}").into(),
                };
                self.picture_problem = Some((key.to_owned(), why.clone()));
                self.trace
                    .record(&TraceLine::new("account", "picture-refused").people([key]));
                cx.notify();
                Some(why)
            }
        }
    }

    /// "Use profile picture" of account `key`: the picture the user chose goes, and its profile picture shows again.
    /// A picture that is still being saved for it is not kept.
    pub fn use_profile_picture(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(tile) = self.tiles.iter_mut().find(|tile| tile.key == key) else {
            return;
        };
        tile.chosen_picture = None;
        if self.picture_problem.as_ref().is_some_and(|(account, _)| account == key) {
            self.picture_problem = None;
        }
        self.trace
            .record(&TraceLine::new("account", "picture-reset").people([key]));
        cx.notify();
        self.forget_picture(key);
    }

    /// Deletes the file of the picture the user chose for account `key` (one small file: done at once, so it comes
    /// before each later choice).
    fn forget_picture(&self, key: &str) {
        if let Some(pictures) = &self.pictures
            && let Err(error) = pictures.reset(key)
        {
            log::warn!("the picture of an account can't be deleted: {error}");
        }
    }

    /// Hands account `key` its server's account management (`None` takes it away): its settings then offer Manage
    /// users.
    pub fn set_admin(&mut self, key: &str, admin: Option<Arc<dyn UserAdmin>>, cx: &mut Context<Self>) {
        if let Some(tile) = self.tiles.iter_mut().find(|tile| tile.key == key) {
            tile.admin = admin;
            cx.notify();
        }
    }

    /// Hands account `key` its server's apps and bots (`None` takes them away): its settings then offer Manage apps.
    pub fn set_app_admin(&mut self, key: &str, admin: Option<Arc<dyn AppAdmin>>, cx: &mut Context<Self>) {
        if let Some(tile) = self.tiles.iter_mut().find(|tile| tile.key == key) {
            tile.app_admin = admin;
            cx.notify();
        }
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
        if self.can_remove && self.tile(key).is_some() {
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
