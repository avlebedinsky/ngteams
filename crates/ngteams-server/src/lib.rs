//! The own Teams-compatible server (`docs/server.md`): ngteams signs in to it instead of Microsoft and finds every
//! service it serves under one origin, with the same wire as Microsoft's. No client crate depends on it.
//!
//! - [`config`]: the configuration (figment: defaults, TOML, `NGTEAMS_SERVER_*`);
//! - [`db`]: Postgres or SQLite through Diesel: accounts, refresh tokens, signing keys, the admin audit;
//! - [`auth`]: passwords, signing keys, tokens and the request extractors that check them;
//! - [`routes`]: the endpoints; [`services`]: the served services and the GTMs authz names them by;
//! - [`fanout`]: payloads between nodes (in-process or Redis) and their replay journal; [`storage`]: files
//!   (`object_store`);
//! - `media`: pictures in messages, files sent in chats and link previews, kept in [`storage`];
//! - [`trouter`]: the push socket; [`registrar`]: the registrations of its endpoints;
//! - [`throttle`]: failed sign-ins per account;
//! - [`app`]: the shared state and the router; [`serve`]: the listener, plain or TLS; [`cli`]: the command line;
//!   [`admin`]: account administration (the admin API, the first-admin setup, the account commands);
//! - `calendar`: events, series and meetings (the middle tier's calendar routes);
//! - [`calling`]: the call controller (a 1:1 call's signaling across nodes) and the TURN credentials;
//! - [`presence`]: who shows as what (UPS), pushed to the endpoints that watch; [`people`]: the directory's profiles
//!   and the reads people are named and drawn from;
//! - `teams`: teams and channels (CSA, the middle tier's and the provisioning service's team routes); a channel
//!   is a thread of the chat service;
//! - [`search`]: the title bar's search (Substrate's suggestions and results) over a full-text index of the messages;
//! - [`workflows`]: Workflows with incoming webhooks (the Flow API subset the client uses, and the webhooks);
//! - [`apps`]: the app registry, a bot's credentials and the keys activities to bots are signed with.

pub mod admin;
pub mod app;
pub mod apps;
pub mod auth;
pub mod bots;
mod calendar;
pub mod calling;
mod chatsvc;
pub mod cli;
pub mod config;
pub mod db;
pub mod fanout;
mod media;
pub mod mixer;
pub mod outbound;
pub mod outgoing;
pub mod people;
pub mod presence;
pub mod registrar;
pub mod routes;
pub mod search;
pub mod serve;
pub mod services;
pub mod storage;
mod teams;
pub mod throttle;
pub mod trouter;
pub mod workflows;
