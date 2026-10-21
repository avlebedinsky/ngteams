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
//! - [`trace`]: the view trace, one JSON line per view change (ids, kinds and times, never text) on a descriptor the
//!   caller hands in, read by the live test.
//! - [`view`]: the window's root view; [`window`] opens it.
//!
//! No view builds a request: the UI has no HTTP or socket client of its own, and GPUI's HTTP client stays the null
//! one (nothing here hands GPUI another), so an image URL loads nothing. Pictures will reach a view only as bytes
//! from the engine.
//!
//! M2 Task 6 (`docs/plans/20261021-native-teams-m2-gpui-daily-driver.md`).

pub mod bridge;
pub mod models;
pub mod trace;
pub mod view;
pub mod window;

pub use bridge::{Bridge, Core, EventSource};
pub use models::{Account, Chats, Engine, Link, Models, Status};
pub use trace::{TraceId, TraceLine, ViewTrace};
pub use view::Root;
