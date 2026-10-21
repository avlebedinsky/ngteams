//! Teams internal API requests and the native request policy. Every mutating request takes a `WritePermit` that only
//! `teams-fence` can issue.
//!
//! A request comes in one of three classes, and a [`Transport`] takes nothing else:
//! - a [`Read`]: always sent as a `GET`, with no body;
//! - an [`OwnState`] request: the reviewed list of requests that touch only the account's own state, sealed (only
//!   this crate adds to it; it starts empty) and logged to `fence.log` before it is sent;
//! - a write: a request others could notice implements [`Mutating`] and names its one target. [`Client::write`] takes
//!   the request together with a [`WritePermit`](teams_fence::WritePermit), so a mutating call without a permit doesn't
//!   compile, and a permit can't be made outside `teams-fence`. At runtime [`Authorized::new`] checks that the permit
//!   was issued for the request's own target before anything reaches the [`Transport`], which sends the request's
//!   sealed wire form and returns the service's answer.
//!
//! On the push channel's socket the same holds for frames: a [`FrameSink`] sends only an [`OwnFrame`], the sealed,
//! reviewed list of the Trouter session's own frames (authentication, keep-alive, the message-loss acknowledgement,
//! the disconnect), each logged to `fence.log` before it is sent; there is no answer to a delivery and no
//! `user.activity`.
//!
//! [`HttpTransport`] is the live transport: a fixed host allowlist, `https` only, no proxy, no redirects, the account's
//! tokens from its `teams-auth` session. Only the `test-support` feature (tests, never a lab binary) points it at a
//! local mock ([`HttpTransport::with_mock_origin`]).

pub mod chat;
mod client;
mod endpoint;
mod frame;
mod http;
mod own;
mod read;
mod write;

pub use client::{Client, Response, Transport, WriteError};
pub use endpoint::{Endpoint, Method, Service};
pub use frame::{
    Authenticate, ConnectParams, Disconnect, DroppedIndicator, FrameError, FrameSink, FrameText, FrameWriter,
    MessageLossAck, OwnFrame, Ping, Sent,
};
pub use http::{
    BuildError, CONNECT_TIMEOUT, Credentials, DEFAULT_RETRY_WAIT, Failure, HttpError, HttpTransport, IC3_AUDIENCE,
    MAX_BODY, MAX_RETRY_WAIT, REQUEST_TIMEOUT, Refusal, TROUTER_HOST,
};
pub use own::OwnState;
#[cfg(feature = "test-support")]
pub use own::ProbeOwnState;
pub use read::Read;
pub use write::{
    AddToCall, Authorized, ConversationUrl, Mutating, SendMessage, StartCall, TEST_CALL_SCENARIO, TargetMismatch, Wire,
};
