//! The tray item (M2 Task 14): a `StatusNotifierItem` showing the account's unread total, with "Open" and "Quit".
//!
//! - **The total** ([`Tray`]): the store's unread count summed over the chats the chat list shows (not a team's
//!   channels, not the account's own streams), counted again a moment after the models say a chat changed, each chat
//!   asked again only when it changed since it was counted. The item's title and tooltip say it ("Teams: 3 unread"),
//!   its status is `NeedsAttention` while it isn't 0, `Active` otherwise.
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
use gpui::{AppContext as _, AsyncApp, Context, WeakEntity};
use tokio::sync::mpsc::UnboundedSender;
use zbus::Connection;
use zbus::interface;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Str, Value};

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
/// The icons from the desktop's theme: none unread, some unread.
pub const ICON: &str = "internet-chat";
pub const ICON_UNREAD: &str = "mail-unread";

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
    if unread == 0 {
        app.to_owned()
    } else {
        format!("{app}: {unread} unread")
    }
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
    /// A count is under way, and whether the models changed since it began.
    counting: bool,
    again: bool,
    _observing: gpui::Subscription,
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
            counting: false,
            again: false,
            _observing: observing,
        };
        tray.schedule(cx);
        tray
    }

    /// The item from now on; it is shown the total at once.
    pub fn set_icon(&mut self, icon: Arc<dyn TrayIcon>, cx: &mut Context<Self>) {
        if let Some(total) = self.total {
            cx.background_spawn(icon.set_unread(total)).detach();
        }
        self.icon = Some(icon);
    }

    /// Whether an item was given.
    #[must_use]
    pub fn has_icon(&self) -> bool {
        self.icon.is_some()
    }

    /// The unread total as last counted.
    #[must_use]
    pub fn total(&self) -> Option<u64> {
        self.total
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

    /// A count's result: the chats it counted, as of `revisions`.
    fn counted(&mut self, chats: Vec<(String, Counted)>, cx: &mut Context<Self>) -> bool {
        self.counted = chats.into_iter().collect();
        let total = self.counted.values().map(|counted| counted.unread).sum();
        if self.total != Some(total) {
            self.total = Some(total);
            self.trace.record(&TraceLine::new("tray", "unread").unread([total]));
            if let Some(icon) = &self.icon {
                cx.background_spawn(icon.set_unread(total)).detach();
            }
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
    events: UnboundedSender<TrayEvent>,
}

/// An icon's pixels as the item's properties carry them (none: the icon is named from the theme).
type Pixmaps = Vec<(i32, i32, Vec<u8>)>;

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

    #[zbus(property)]
    fn icon_name(&self) -> &'static str {
        if self.unread == 0 { ICON } else { ICON_UNREAD }
    }

    #[zbus(property)]
    #[expect(clippy::unused_self, reason = "a D-Bus property zbus reads on the object")]
    fn icon_pixmap(&self) -> Pixmaps {
        Vec::new()
    }

    #[zbus(property)]
    #[expect(clippy::unused_self, reason = "a D-Bus property zbus reads on the object")]
    fn attention_icon_name(&self) -> &'static str {
        ICON_UNREAD
    }

    #[zbus(property)]
    fn tool_tip(&self) -> (String, Pixmaps, String, String) {
        (
            self.icon_name().to_owned(),
            Vec::new(),
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

    async fn show_unread(connection: Connection, unread: u64) -> zbus::Result<()> {
        let item = connection.object_server().interface::<_, Item>(ITEM_PATH).await?;
        {
            let mut shown = item.get_mut().await;
            if shown.unread == unread {
                return Ok(());
            }
            shown.unread = unread;
        }
        let emitter = item.signal_emitter();
        Item::new_title(emitter).await?;
        Item::new_icon(emitter).await?;
        Item::new_tool_tip(emitter).await?;
        Item::new_status(emitter, status(unread)).await
    }
}

impl TrayIcon for BusTray {
    fn set_unread(&self, unread: u64) -> BoxFuture<'static, ()> {
        let connection = self.connection.clone();
        Box::pin(async move {
            // a bus that went away takes the item with it: nothing left to show
            let _ = Self::show_unread(connection, unread).await;
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_says_the_total() {
        assert_eq!(title("Teams", 0), "Teams");
        assert_eq!(title("Teams", 3), "Teams: 3 unread");
        assert_eq!(status(0), "Active");
        assert_eq!(status(1), "NeedsAttention");
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
