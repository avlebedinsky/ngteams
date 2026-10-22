//! `teams-ui`: the native Teams client's GPUI views and models, over `teams-core`'s engine.
//!
//! - [`bridge`]: the [`Bridge`](bridge::Bridge) owns the engine's [`EngineHandle`](teams_core::EngineHandle) (its
//!   events and its shutdown); one foreground task drains its events into the [`models`]; views share only its
//!   request side, behind the [`Core`](bridge::Core) trait, and ask for data with a query's
//!   [`Reply`](teams_core::Reply). Shutdown ends the drain and shuts the engine down off the foreground, joining its
//!   thread.
//! - [`models`]: GPUI entities the views observe: the account's [`Status`](models::Status) (the push channel, the
//!   engine, signed in or out) and what changed in the [`Chats`](models::Chats). Everything shown comes from the store,
//!   through a query; the models say when to ask again.
//! - [`shell`]: the window's three panes: the rail (Chat, Teams, Settings), the list pane, resizable, and the
//!   conversation pane; Ctrl+K opens the [`switcher`] (the chat switcher, fuzzy over the chats' names).
//! - [`chat_list`]: the chats by last activity, from the store, kept up to date by the models' changes.
//! - [`people`]: people as the views show them, a name and a picture from the engine (M2 Task 12); [`presence`] their
//!   presence, a dot in the chat list, the chat's header and the mention picker (M2 Task 17).
//! - [`chat_view`]: a chat's messages, the newest at the bottom, paged from the store (and the service's older
//!   history), grouped by sender, with day separators, the "New messages" divider, typing and the messages' actions;
//!   [`rich_text`] is our own renderer of a message's sanitized rich text (never an HTML or markdown view); the
//!   account's writes show there as they go out ([`sending`]: each outbox row's state, "send again").
//! - [`compose`]: the compose box under a chat's messages: text with line breaks, links and mentions (by MRI, from the
//!   chat's members) into a message, Enter sends, replies and edits, typing; every write through the engine.
//! - [`search`]: local search (Ctrl+Shift+F, and the switcher's messages) over the messages the store holds, never the
//!   service's search; a hit opens its chat at the message.
//! - [`teams_list`]: the Teams rail's list, the account's teams and their channels (M2 Task 16); [`channel_view`] a
//!   channel's posts with their reply chains, read-only: no composer, and no write a channel could take.
//! - [`settings`]: the settings and their file (`0600` in a `0700` directory; defaults for what can't be read);
//!   [`settings_view`] changes them.
//! - [`trace`]: the view trace, one JSON line per view change (ids, kinds and times, never text) on a descriptor the
//!   caller hands in, read by the live test.
//! - [`desktop`]: the app on the desktop: the main window (opened again from the tray or a notification), the
//!   [`notify`] notifications of messages that come (as the "notifications" setting says; a chat shown in a focused
//!   window never notifies; activating one opens its chat) and the [`tray`] item (the unread total, open, quit;
//!   closing the window keeps the app in the tray), both on the bus the app is given ([`bus`]) and nowhere else.
//! - [`view`]: the window's root view; [`window`] opens it inside the kit's root, which applies the theme to the
//!   window (its font size as the rem size, its font family and colours) and hosts the kit's dialogs and tooltips.
//! - [`theme`]: the UI kit's (`gpui-component`) theme in Teams-like colours and compact sizes, light and dark
//!   following the window's appearance.
//!
//! No view builds a request: the UI has no HTTP or socket client of its own, and GPUI's HTTP client stays the null
//! one (nothing here hands GPUI another), so an image URL loads nothing. Pictures reach a view only as bytes from
//! the engine ([`people`]).
//!
//! M2 Tasks 6–17 (`docs/plans/20261021-native-teams-m2-gpui-daily-driver.md`).

pub mod bridge;
pub mod bus;
pub mod channel_view;
pub mod chat_list;
pub mod chat_view;
pub mod compose;
pub mod desktop;
pub mod models;
pub mod notify;
pub mod people;
pub mod presence;
pub mod rich_text;
pub mod search;
pub mod sending;
pub mod settings;
pub mod settings_view;
pub mod shell;
pub mod switcher;
pub mod teams_list;
pub mod theme;
pub mod trace;
pub mod tray;
pub mod view;
pub mod window;

pub use bridge::{Bridge, Core, EventSource, StoppedCore};
pub use channel_view::ChannelView;
pub use chat_list::{ChatList, ChatListEvent, ChatRow};
pub use chat_view::{ChatView, ChatViewEvent, MessageAction};
pub use compose::{Compose, ComposeEvent, ComposeParts};
pub use desktop::{Desktop, DesktopEvent, DesktopParts};
pub use models::{Account, Chats, Engine, Feed, FeedEvent, Link, Models, Status};
pub use notify::{Notice, NoticeKind, NotificationServer, Notifier, NotifierEvent};
pub use people::Shown;
pub use presence::Presences;
pub use search::{Hit, HitTarget, Search, SearchEvent};
pub use settings::{Settings, SettingsFile, SettingsModel};
pub use shell::{Shell, ShellParts};
pub use switcher::Switcher;
pub use teams_list::{ChannelChosen, TeamsList, TeamsListEvent};
pub use trace::{TraceId, TraceLine, ViewTrace};
pub use tray::{Tray, TrayEvent, TrayIcon};
pub use view::Root;
