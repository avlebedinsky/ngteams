//! Pictures, files, download links and link previews as users of two or three ngteams clients meet them: real
//! server processes (one node on `SQLite`, two nodes on one Postgres and one Redis), operator-created users, each
//! client's session, engine, store and push channel, and the blobs in the store `NGTEAMS_SERVER_S3_URL` names
//! (else a directory), which the nodes are given by configuration alone.

mod files;
mod limits;
mod links;
mod nodes;
mod pictures;
mod previews;
mod storage;
pub mod support;
