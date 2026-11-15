//! Presence, people and the operator's profile commands as users of three ngteams clients meet them: the real
//! server binary (one node on SQLite, two on Postgres and Redis, the S3 store `NGTEAMS_SERVER_S3_URL` names), accounts
//! and profiles the operator made, and each client as the app runs it (session, engine, push channel). All synthetic.

mod access;
mod operator;
mod people;
mod presence;
pub mod support;
mod watches;
