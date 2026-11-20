//! End-to-end tests: the real client crates (`teams-auth`'s session and clients, `teams-core`'s account bring-up)
//! against in-process servers and the real binary on loopback, on SQLite and, when `NGTEAMS_SERVER_PG_URL` is set, on Postgres
//! (Redis cases when `NGTEAMS_SERVER_REDIS_URL` is set).

#![cfg(test)]

mod admin;
mod admin_api;
mod authz;
#[cfg(target_os = "linux")]
mod binary;
mod bots;
mod calendar;
mod calling;
mod chatsvc;
mod crowd;
mod deployments;
mod endpoints;
mod first_write;
mod first_write_accounts;
mod first_write_cancel;
mod media;
mod migrations;
mod node;
mod outgoing;
mod people;
mod presence;
mod search;
mod shared;
mod sign_in;
mod support;
mod teams;
mod trouter;
mod workflows;
