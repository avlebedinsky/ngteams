//! `teams-ctl`: the control socket of the running `ngteams` (`docs/plans/20261106-ngteamsctl.md`).
//!
//! `ngteamsctl` drives the running app over a Unix socket speaking JSON lines. This crate holds what both ends share,
//! and no GPUI: the server lives in the app (`crates/ngteams/src/ctl/`).
//!
//! - [`protocol`]: the requests, responses, error codes and events on the wire, and [`PROTOCOL_VERSION`]. They are
//!   plain serde types of their own, never the engine's or the store's, so the wire doesn't follow internal renames.
//! - [`path`]: where each mode's socket is (personal, a lab alias, the read-only runner), `$NGTEAMS_CTL_SOCKET`, and
//!   the private directory it is bound in.
//! - [`codec`]: one JSON value per line, each line capped in size, a response paired with its request by id.
//! - [`client`]: a connect and reads bounded in time, the `hello` handshake, requests, and a `subscribe` stream.

pub mod client;
pub mod codec;
pub mod path;
pub mod protocol;

pub use client::{Client, ClientError, Events, Timeouts, connect_stream};
pub use codec::{CodecError, LineReader, MAX_LINE};
pub use path::{Env, Mode, PathError, prepare_dir, socket_path};
pub use protocol::{
    CtlError, CtlEvent, ErrorCode, EventFilter, EventLine, PROTOCOL_VERSION, Request, RequestLine, Response,
    ResponseLine,
};
