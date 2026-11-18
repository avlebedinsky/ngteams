//! Teams and channels as two or three ngteams clients use them: real server processes (one node on SQLite, two on
//! Postgres and Redis), accounts the operator added, and each account's session, typed client and engine on its store.

mod admin;
mod joining;
mod lifecycle;
mod membership;
mod nodes;
mod own_view;
mod posts;
mod private;
pub(super) mod support;
