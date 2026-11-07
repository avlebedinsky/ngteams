//! The tray item (M2 Task 14): a `StatusNotifierItem` showing the account's unread total, with "Open" and "Quit".
//!
//! - **The total** ([`Tray`]): the unread chats, as Teams' own badge counts them: a chat the chat list shows (not a
//!   team's channel, not the account's own streams) counts once while the store holds any message of it unread, however
//!   many (Teams keeps one yes or no per chat: `docs/protocol/chat.md`, "Which chats are unread"). Counted again a moment
//!   after the models say a chat changed, each chat asked again only when it changed since it was counted, and summed
//!   over the window's accounts. The item's title and tooltip say it ("ngteams: 3 unread chats"), its status is
//!   `NeedsAttention` while it isn't 0, `Active` otherwise. Its icon is the app's, carried as pixels
//!   ([`crate::app_icon::pixmaps`]) with Teams' red unread badge and the count on it ("99+" past 99) while the total
//!   isn't 0, never a name from the theme: the `StatusNotifierItem` spec has no counter of its own, so the number is
//!   drawn into the icon, and into its attention icon too (a host shows that one while the item needs attention).
//! - **The launcher's count**: the same total goes to the desktop's task manager as the Unity launcher entry's `count`
//!   (`com.canonical.Unity.LauncherEntry.Update` for `application://<app id>.desktop`), which KDE Plasma's task manager
//!   and docks draw on the app's task button; hidden at 0. Nothing hears it on a desktop without one.
//! - **The item** ([`BusTray`]): `org.kde.StatusNotifierItem` at [`ITEM_PATH`] and its menu, `com.canonical.dbusmenu`
//!   at [`MENU_PATH`], on the connection to the bus the app is given ([`crate::bus`]), under the name
//!   `org.kde.StatusNotifierItem-<pid>-1`, registered with the watcher (`org.kde.StatusNotifierWatcher`). Activating
//!   it (a click) or the menu's "Open" asks for the window ([`TrayEvent::Open`]); "Quit" quits the app
//!   ([`TrayEvent::Quit`]). Without a watcher on the bus there is no item, and closing the window quits the app.
//! - **The watcher's comings and goings** ([`Hosted`]): the item follows the owner of the watcher's name. A new owner
//!   (a panel restarted) gets the item registered again; one that leaves with none after it, or the connection to the
//!   bus lost, leaves the item shown nowhere, which the app is told so closing its window quits it again.
//!
//! In a view test the item is a fake [`TrayIcon`].

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt as _;
use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;
use gpui::{App, AppContext as _, AsyncApp, Context, Entity, Subscription, WeakEntity};
use tokio::sync::mpsc::UnboundedSender;
use zbus::Connection;
use zbus::interface;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Str, Value};

use crate::app_icon::{self, Pixmaps};
use crate::bridge::Core;
use crate::chat_list::{self, LIST_LIMIT};
use crate::models::Models;
use crate::trace::{TraceLine, ViewTrace};

/// The item's object path.
pub const ITEM_PATH: &str = "/StatusNotifierItem";
/// Its menu's object path.
pub const MENU_PATH: &str = "/MenuBar";
/// The watcher's bus name and object path.
pub const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
pub const WATCHER_PATH: &str = "/StatusNotifierWatcher";
/// The menu's items, by id (0 is its root).
pub const MENU_OPEN: i32 = 1;
pub const MENU_QUIT: i32 = 2;
/// How long after a change the total is counted again.
pub const COUNT_DELAY: Duration = Duration::from_millis(300);
/// The Unity launcher entry's interface, and the path its signal comes from.
pub const LAUNCHER_ENTRY: &str = "com.canonical.Unity.LauncherEntry";
pub const LAUNCHER_PATH: &str = "/com/canonical/unity/launcherentry/ngteams";

/// What the tray asks the app for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayEvent {
    /// Show the window (open it again if it was closed).
    Open,
    /// Quit the app.
    Quit,
}

/// What became of the watcher the item is registered with ([`BusTray::start`]'s stream).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hosted {
    /// A new watcher took the name, and the item is registered with it.
    Registered,
    /// The watcher left (or a new one refused the item): the item is shown nowhere until another comes.
    Gone,
}

/// The tray item, as the [`Tray`] sees it.
pub trait TrayIcon: Send + Sync + 'static {
    /// Shows `unread` as the account's unread total.
    fn set_unread(&self, unread: u64) -> BoxFuture<'static, ()>;
}

/// The item's title (and tooltip) for an unread total: the app's name, and the total when it isn't 0.
#[must_use]
pub fn title(app: &str, unread: u64) -> String {
    match unread {
        0 => app.to_owned(),
        1 => format!("{app}: 1 unread chat"),
        _ => format!("{app}: {unread} unread chats"),
    }
}

/// The launcher entry's properties for an unread total: its `count`, shown while it isn't 0.
#[must_use]
pub fn launcher_properties(unread: u64) -> HashMap<&'static str, Value<'static>> {
    HashMap::from([
        ("count", Value::from(i64::try_from(unread).unwrap_or(i64::MAX))),
        ("count-visible", Value::from(unread > 0)),
    ])
}

/// The desktop entry the launcher entry names: the app's id, which the install's `.desktop` file is named after.
#[must_use]
pub fn launcher_uri(app_id: &str) -> String {
    format!("application://{app_id}.desktop")
}

/// The item's status for an unread total.
#[must_use]
pub fn status(unread: u64) -> &'static str {
    if unread == 0 { "Active" } else { "NeedsAttention" }
}

/// A chat's count, and the models' revision of its change when it was counted.
#[derive(Clone, Copy, Debug)]
struct Counted {
    at: u64,
    unread: u64,
}

/// The unread total and the item that shows it (see the module docs). A GPUI entity.
pub struct Tray {
    core: Arc<dyn Core>,
    models: Models,
    trace: Arc<ViewTrace>,
    icon: Option<Arc<dyn TrayIcon>>,
    counted: HashMap<String, Counted>,
    total: Option<u64>,
    /// The other accounts' counts, whose totals the item shows summed with this one's ([`Tray::add_account`]).
    others: Vec<(Entity<Tray>, Subscription)>,
    /// The total the item was last shown (this account's and the others').
    shown: Option<u64>,
    /// A count is under way, and whether the models changed since it began.
    counting: bool,
    again: bool,
    _observing: Subscription,
}

impl Tray {
    /// The total over the models, counted at once and after every change; shown once an item is given
    /// ([`Tray::set_icon`]).
    pub fn new(core: Arc<dyn Core>, models: Models, trace: Arc<ViewTrace>, cx: &mut Context<Self>) -> Self {
        let observing = cx.observe(&models.chats, |this: &mut Self, _, cx| this.schedule(cx));
        let mut tray = Self {
            core,
            models,
            trace,
            icon: None,
            counted: HashMap::new(),
            total: None,
            others: Vec::new(),
            shown: None,
            counting: false,
            again: false,
            _observing: observing,
        };
        tray.schedule(cx);
        tray
    }

    /// The item from now on; it is shown the total at once.
    pub fn set_icon(&mut self, icon: Arc<dyn TrayIcon>, cx: &mut Context<Self>) {
        if let Some(total) = self.combined(cx) {
            cx.background_spawn(icon.set_unread(total)).detach();
        }
        self.icon = Some(icon);
    }

    /// Another account's count (an app of several accounts): the item shows the totals summed, counted again whenever
    /// that one's changes.
    pub fn add_account(&mut self, other: Entity<Tray>, cx: &mut Context<Self>) {
        let observing = cx.observe(&other, |this: &mut Self, _, cx| this.show(cx));
        self.others.push((other, observing));
        self.show(cx);
    }

    /// Takes another account's count out of the sum.
    pub fn remove_account(&mut self, other: &Entity<Tray>, cx: &mut Context<Self>) {
        self.others.retain(|(tray, _)| tray != other);
        self.show(cx);
    }

    /// The total the item shows: this account's and the others', once this one was counted.
    #[must_use]
    pub fn combined(&self, cx: &App) -> Option<u64> {
        let others: u64 = self.others.iter().filter_map(|(other, _)| other.read(cx).total()).sum();
        self.total.map(|total| total + others)
    }

    /// Shows the item the combined total when it changed.
    fn show(&mut self, cx: &mut Context<Self>) {
        let combined = self.combined(cx);
        if combined == self.shown {
            return;
        }
        self.shown = combined;
        if let Some(total) = combined {
            self.trace.record(&TraceLine::new("tray", "unread").unread([total]));
            if let Some(icon) = &self.icon {
                cx.background_spawn(icon.set_unread(total)).detach();
            }
        }
    }

    /// Takes the item away (another account's count shows it from now on).
    pub fn take_icon(&mut self) -> Option<Arc<dyn TrayIcon>> {
        self.shown = None;
        self.icon.take()
    }

    /// Whether an item was given.
    #[must_use]
    pub fn has_icon(&self) -> bool {
        self.icon.is_some()
    }

    /// The unread total (the unread chats) as last counted.
    #[must_use]
    pub fn total(&self) -> Option<u64> {
        self.total
    }

    /// The counted chats that hold anything unread, each with how many of its messages are, as last counted: the
    /// chats [`Tray::total`] counts (in no order).
    pub fn unread_chats(&self) -> impl Iterator<Item = (&str, u64)> {
        self.counted
            .iter()
            .filter(|(_, counted)| counted.unread > 0)
            .map(|(chat, counted)| (chat.as_str(), counted.unread))
    }

    fn schedule(&mut self, cx: &mut Context<Self>) {
        if self.counting {
            self.again = true;
            return;
        }
        self.counting = true;
        cx.spawn(async move |this, cx: &mut AsyncApp| count(this, cx).await)
            .detach();
    }

    /// A count's result: the chats it counted, as of `revisions`; the total is how many of them hold anything unread.
    /// The observers are told when the total or a chat's count changed (the control socket's `unread` follows both).
    fn counted(&mut self, chats: Vec<(String, Counted)>, cx: &mut Context<Self>) -> bool {
        let counted: HashMap<String, Counted> = chats.into_iter().collect();
        let recounted = counted.len() != self.counted.len()
            || counted
                .iter()
                .any(|(chat, now)| self.counted.get(chat).is_none_or(|before| before.unread != now.unread));
        self.counted = counted;
        let unread = self.counted.values().filter(|counted| counted.unread > 0).count();
        let total = u64::try_from(unread).unwrap_or(u64::MAX);
        if self.total != Some(total) {
            self.total = Some(total);
            self.show(cx);
            cx.notify();
        } else if recounted {
            cx.notify();
        }
        if std::mem::take(&mut self.again) {
            true
        } else {
            self.counting = false;
            false
        }
    }
}

/// Counts the total after [`COUNT_DELAY`], again while the models changed during a count.
async fn count(this: WeakEntity<Tray>, cx: &mut AsyncApp) {
    loop {
        cx.background_executor().timer(COUNT_DELAY).await;
        let Ok((core, models, known)) = this.update(cx, |tray, _| {
            (Arc::clone(&tray.core), tray.models.clone(), tray.counted.clone())
        }) else {
            return;
        };
        let Ok(conversations) = core.conversations(LIST_LIMIT).await else {
            // a stopped engine: the total stays as it was
            let _ = this.update(cx, |tray, _| tray.counting = false);
            return;
        };
        let mut chats = Vec::new();
        for conversation in conversations
            .iter()
            .filter(|conversation| chat_list::listed(conversation))
        {
            let at = cx.update(|cx| models.chats.read(cx).changed_at(&conversation.id));
            let counted = match known.get(&conversation.id) {
                Some(counted) if counted.at == at => *counted,
                _ => Counted {
                    at,
                    unread: core.unread_count(conversation.id.clone()).await.unwrap_or(0),
                },
            };
            chats.push((conversation.id.clone(), counted));
        }
        match this.update(cx, |tray, cx| tray.counted(chats, cx)) {
            Ok(true) => {}
            _ => return,
        }
    }
}

/// `org.kde.StatusNotifierItem`.
struct Item {
    id: String,
    app: String,
    unread: u64,
    /// The icon for `unread`, drawn when it changed.
    icon: Pixmaps,
    events: UnboundedSender<TrayEvent>,
}

#[interface(name = "org.kde.StatusNotifierItem")]
impl Item {
    fn activate(&self, x: i32, y: i32) {
        // where the click was doesn't matter
        let _ = (x, y);
        // an app that is quitting has nobody left to show
        let _ = self.events.send(TrayEvent::Open);
    }

    fn secondary_activate(&self, x: i32, y: i32) {
        let _ = (x, y);
        let _ = self.events.send(TrayEvent::Open);
    }

    #[expect(clippy::unused_self, reason = "a D-Bus method zbus calls on the object")]
    fn context_menu(&self, x: i32, y: i32) {
        let _ = (x, y);
        // the host shows the menu from `Menu`
    }

    #[expect(clippy::unused_self, reason = "a D-Bus method zbus calls on the object")]
    fn scroll(&self, delta: i32, orientation: &str) {
        // scrolling over the item does nothing
        let _ = (delta, orientation);
    }

    #[zbus(property)]
    #[expect(clippy::unused_self, reason = "a D-Bus property zbus reads on the object")]
    fn category(&self) -> &'static str {
        "Communications"
    }

    #[zbus(property)]
    fn id(&self) -> &str {
        &self.id
    }

    #[zbus(property)]
    fn title(&self) -> String {
        title(&self.app, self.unread)
    }

    #[zbus(property)]
    fn status(&self) -> &'static str {
        status(self.unread)
    }

    #[zbus(property)]
    #[expect(clippy::unused_self, reason = "a D-Bus property zbus reads on the object")]
    fn window_id(&self) -> i32 {
        0
    }

    // no name: a host draws a named icon over the pixels, and the theme names no badged one
    #[zbus(property)]
    #[expect(clippy::unused_self, reason = "a D-Bus property zbus reads on the object")]
    fn icon_name(&self) -> &'static str {
        ""
    }

    #[zbus(property)]
    fn icon_pixmap(&self) -> Pixmaps {
        self.icon.clone()
    }

    #[zbus(property)]
    #[expect(clippy::unused_self, reason = "a D-Bus property zbus reads on the object")]
    fn attention_icon_name(&self) -> &'static str {
        ""
    }

    // the count, as the icon: a host shows this one while the item needs attention
    #[zbus(property)]
    fn attention_icon_pixmap(&self) -> Pixmaps {
        self.icon.clone()
    }

    #[zbus(property)]
    fn tool_tip(&self) -> (String, Pixmaps, String, String) {
        (
            String::new(),
            self.icon.clone(),
            title(&self.app, self.unread),
            String::new(),
        )
    }

    #[zbus(property)]
    #[expect(clippy::unused_self, reason = "a D-Bus property zbus reads on the object")]
    fn item_is_menu(&self) -> bool {
        false
    }

    #[zbus(property)]
    #[expect(clippy::unused_self, reason = "a D-Bus property zbus reads on the object")]
    fn menu(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from(MENU_PATH).unwrap_or_default()
    }

    #[zbus(signal)]
    async fn new_title(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn new_icon(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn new_attention_icon(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn new_tool_tip(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn new_status(emitter: &SignalEmitter<'_>, status: &str) -> zbus::Result<()>;
}

/// One menu item: its id and its properties.
type MenuItem = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

/// `com.canonical.dbusmenu`: "Open" and "Quit".
struct Menu {
    events: UnboundedSender<TrayEvent>,
}

fn text(value: &str) -> OwnedValue {
    OwnedValue::from(Str::from(value.to_owned()))
}

/// A menu item's properties, those `names` asks for (all when it names none).
fn properties(id: i32, names: &[String]) -> Option<HashMap<String, OwnedValue>> {
    let all: Vec<(&str, OwnedValue)> = match id {
        0 => vec![("children-display", text("submenu"))],
        MENU_OPEN => vec![
            ("label", text("Open Teams")),
            ("enabled", OwnedValue::from(true)),
            ("visible", OwnedValue::from(true)),
        ],
        MENU_QUIT => vec![
            ("label", text("Quit")),
            ("enabled", OwnedValue::from(true)),
            ("visible", OwnedValue::from(true)),
        ],
        _ => return None,
    };
    Some(
        all.into_iter()
            .filter(|(name, _)| names.is_empty() || names.iter().any(|wanted| wanted == name))
            .map(|(name, value)| (name.to_owned(), value))
            .collect(),
    )
}

/// The menu's layout from `parent` down (its root holds the two items).
fn layout(parent: i32, names: &[String]) -> zbus::fdo::Result<MenuItem> {
    let own = properties(parent, names).ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("no item {parent}")))?;
    let mut children = Vec::new();
    if parent == 0 {
        for id in [MENU_OPEN, MENU_QUIT] {
            let item = properties(id, names).unwrap_or_default();
            let child = Value::from((id, item, Vec::<OwnedValue>::new()));
            children.push(OwnedValue::try_from(child).map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?);
        }
    }
    Ok((parent, own, children))
}

#[interface(name = "com.canonical.dbusmenu")]
impl Menu {
    #[expect(
        clippy::unused_self,
        clippy::needless_pass_by_value,
        reason = "a D-Bus method zbus calls on the object, handing it its arguments by value"
    )]
    fn get_layout(
        &self,
        parent_id: i32,
        recursion_depth: i32,
        property_names: Vec<String>,
    ) -> zbus::fdo::Result<(u32, MenuItem)> {
        // the menu is one level: every depth gets all of it
        let _ = recursion_depth;
        Ok((1, layout(parent_id, &property_names)?))
    }

    #[expect(
        clippy::unused_self,
        clippy::needless_pass_by_value,
        reason = "a D-Bus method zbus calls on the object, handing it its arguments by value"
    )]
    fn get_group_properties(
        &self,
        ids: Vec<i32>,
        property_names: Vec<String>,
    ) -> Vec<(i32, HashMap<String, OwnedValue>)> {
        ids.into_iter()
            .filter_map(|id| Some((id, properties(id, &property_names)?)))
            .collect()
    }

    #[expect(clippy::unused_self, reason = "a D-Bus method zbus calls on the object")]
    fn get_property(&self, id: i32, name: &str) -> zbus::fdo::Result<OwnedValue> {
        properties(id, &[name.to_owned()])
            .and_then(|mut found| found.remove(name))
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("no property {name} on item {id}")))
    }

    fn event(&self, id: i32, event_id: &str, data: OwnedValue, timestamp: u32) {
        // an event's data and time don't change what it asks
        let _ = (data, timestamp);
        self.clicked(id, event_id);
    }

    fn event_group(&self, events: Vec<(i32, String, OwnedValue, u32)>) -> Vec<i32> {
        let mut unknown = Vec::new();
        for (id, event_id, _, _) in events {
            if properties(id, &[]).is_none() {
                unknown.push(id);
            }
            self.clicked(id, &event_id);
        }
        unknown
    }

    #[expect(clippy::unused_self, reason = "a D-Bus method zbus calls on the object")]
    fn about_to_show(&self, id: i32) -> bool {
        // the menu never changes: nothing to update before it shows
        let _ = id;
        false
    }

    #[expect(clippy::unused_self, reason = "a D-Bus method zbus calls on the object")]
    fn about_to_show_group(&self, ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
        // the menu never changes: no item needs an update
        drop(ids);
        (Vec::new(), Vec::new())
    }

    #[zbus(property)]
    #[expect(clippy::unused_self, reason = "a D-Bus property zbus reads on the object")]
    fn version(&self) -> u32 {
        3
    }

    #[zbus(property)]
    #[expect(clippy::unused_self, reason = "a D-Bus property zbus reads on the object")]
    fn text_direction(&self) -> &'static str {
        "ltr"
    }

    #[zbus(property)]
    #[expect(clippy::unused_self, reason = "a D-Bus property zbus reads on the object")]
    fn status(&self) -> &'static str {
        "normal"
    }

    #[zbus(property)]
    #[expect(clippy::unused_self, reason = "a D-Bus property zbus reads on the object")]
    fn icon_theme_path(&self) -> Vec<String> {
        Vec::new()
    }
}

impl Menu {
    fn clicked(&self, id: i32, event_id: &str) {
        if event_id != "clicked" {
            return;
        }
        let event = match id {
            MENU_OPEN => TrayEvent::Open,
            MENU_QUIT => TrayEvent::Quit,
            _ => return,
        };
        // an app that is quitting has nobody left to tell
        let _ = self.events.send(event);
    }
}

/// Registers the item named `name` with the watcher on `connection`.
async fn register(connection: &Connection, name: &str) -> zbus::Result<()> {
    connection
        .call_method(
            Some(WATCHER_NAME),
            WATCHER_PATH,
            Some(WATCHER_NAME),
            "RegisterStatusNotifierItem",
            &(name,),
        )
        .await?;
    Ok(())
}

/// The item on the bus (see the module docs).
pub struct BusTray {
    connection: Connection,
    name: String,
    /// The launcher entry's desktop entry ([`launcher_uri`]).
    launcher: String,
}

impl BusTray {
    /// Serves the item and its menu on `connection` as `app` (its id `app_id`), takes its name and registers it with
    /// the watcher; what the user asks of it goes to `events`. With it, what becomes of the watcher from then on
    /// ([`Hosted`]: the item registered again with each new one); the stream ends once the connection to the bus is
    /// lost, the item gone with it.
    ///
    /// # Errors
    /// The objects or the name can't be had, the watcher's comings and goings can't be followed, or no watcher takes
    /// the item (then nothing of it is left on the bus).
    pub async fn start(
        connection: &Connection,
        app_id: &str,
        app: &str,
        events: UnboundedSender<TrayEvent>,
    ) -> zbus::Result<(Self, BoxStream<'static, Hosted>)> {
        let name = format!("org.kde.StatusNotifierItem-{}-1", std::process::id());
        let tray = Self {
            connection: connection.clone(),
            name,
            launcher: launcher_uri(app_id),
        };
        // followed before the first registration, so no new watcher between the two is missed
        let owners = zbus::fdo::DBusProxy::new(connection)
            .await?
            .receive_name_owner_changed_with_args(&[(0, WATCHER_NAME)])
            .await?;
        match tray.serve(app_id, app, events).await {
            Ok(()) => {}
            Err(error) => {
                tray.withdraw().await;
                return Err(error);
            }
        }
        let (registering, closing) = (connection.clone(), connection.clone());
        let name = tray.name.clone();
        let hosted = owners
            .then(move |changed| {
                let (connection, name) = (registering.clone(), name.clone());
                async move {
                    let owned = changed.args().is_ok_and(|args| args.new_owner().is_some());
                    if owned && register(&connection, &name).await.is_ok() {
                        Hosted::Registered
                    } else {
                        Hosted::Gone
                    }
                }
            })
            .take_until(async move { closing.closed().await })
            .boxed();
        Ok((tray, hosted))
    }

    async fn serve(&self, app_id: &str, app: &str, events: UnboundedSender<TrayEvent>) -> zbus::Result<()> {
        let server = self.connection.object_server();
        let item = Item {
            id: app_id.to_owned(),
            app: app.to_owned(),
            unread: 0,
            icon: app_icon::pixmaps(0),
            events: events.clone(),
        };
        server.at(ITEM_PATH, item).await?;
        server.at(MENU_PATH, Menu { events }).await?;
        self.connection.request_name(self.name.as_str()).await?;
        register(&self.connection, &self.name).await
    }

    /// Takes the item and its name off the bus.
    async fn withdraw(&self) {
        let server = self.connection.object_server();
        // what was never served has nothing to take back
        let _ = server.remove::<Item, _>(ITEM_PATH).await;
        let _ = server.remove::<Menu, _>(MENU_PATH).await;
        let _ = self.connection.release_name(self.name.as_str()).await;
    }

    /// The item's bus name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    async fn show_unread(connection: Connection, launcher: String, unread: u64) -> zbus::Result<()> {
        let item = connection.object_server().interface::<_, Item>(ITEM_PATH).await?;
        {
            let mut shown = item.get_mut().await;
            if shown.unread == unread {
                return Ok(());
            }
            shown.unread = unread;
            shown.icon = app_icon::pixmaps(unread);
        }
        let emitter = item.signal_emitter();
        Item::new_title(emitter).await?;
        Item::new_icon(emitter).await?;
        Item::new_attention_icon(emitter).await?;
        Item::new_tool_tip(emitter).await?;
        Item::new_status(emitter, status(unread)).await?;
        connection
            .emit_signal(
                None::<()>,
                LAUNCHER_PATH,
                LAUNCHER_ENTRY,
                "Update",
                &(launcher.as_str(), launcher_properties(unread)),
            )
            .await
    }
}

impl TrayIcon for BusTray {
    fn set_unread(&self, unread: u64) -> BoxFuture<'static, ()> {
        let (connection, launcher) = (self.connection.clone(), self.launcher.clone());
        Box::pin(async move {
            // a bus that went away takes the item with it: nothing left to show
            let _ = Self::show_unread(connection, launcher, unread).await;
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_says_the_total() {
        assert_eq!(title("App", 0), "App");
        assert_eq!(title("App", 1), "App: 1 unread chat");
        assert_eq!(title("App", 3), "App: 3 unread chats");
        assert_eq!(title("App", 250), "App: 250 unread chats");
        assert_eq!(status(0), "Active");
        assert_eq!(status(1), "NeedsAttention");
    }

    #[test]
    fn the_launcher_entry_counts_while_anything_is_unread() {
        assert_eq!(launcher_uri("com.example.app"), "application://com.example.app.desktop");
        let none = launcher_properties(0);
        assert_eq!(none["count"], Value::from(0_i64));
        assert_eq!(none["count-visible"], Value::from(false));
        let some = launcher_properties(120);
        assert_eq!(some["count"], Value::from(120_i64));
        assert_eq!(some["count-visible"], Value::from(true));
        assert_eq!(launcher_properties(u64::MAX)["count"], Value::from(i64::MAX));
    }

    #[test]
    fn the_menu_holds_open_and_quit() {
        let (id, root, children) = layout(0, &[]).unwrap();
        assert_eq!(id, 0);
        assert!(root.contains_key("children-display"));
        assert_eq!(children.len(), 2);
        let labels = properties(MENU_QUIT, &["label".to_owned()]).unwrap();
        assert_eq!(labels.len(), 1);
        assert!(layout(9, &[]).is_err());
    }
}
