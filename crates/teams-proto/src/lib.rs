//! Teams internal API requests and the native request policy. Every mutating request takes a `WritePermit` that only
//! `teams-fence` can issue.
//!
//! A request comes in one of four classes, and a [`Transport`] takes nothing else:
//! - a [`Read`]: always sent as a `GET`, with no body (the chat service's, and Microsoft Graph's two reads of a
//!   person's name and picture, [`GraphUser`] and [`GraphPicture`]);
//! - an [`OwnState`] request: the reviewed list of requests that touch only the account's own state, sealed (only
//!   this crate adds to it: the messaging stack's [`EndpointRegistration`]) and logged to `fence.log` before it is
//!   sent;
//! - a [`PostRead`]: the reviewed list of requests sent as a `POST` that change nothing (the middle tier's profile
//!   lookups), sealed. Each is sent only inside an [`AuthorizedPostRead`] with the
//!   [`PostReadPermit`](teams_fence::PostReadPermit) `Fence::check_post_read` gave for its exact MRIs (in a lab build,
//!   lab accounts only), only in its entry's captured shape, logged to `fence.log` before it is sent, and once;
//! - a write: a request others could notice implements [`Mutating`] and names its one target. [`Client::write`] takes
//!   the request together with a [`WritePermit`](teams_fence::WritePermit), so a mutating call without a permit doesn't
//!   compile, and a permit can't be made outside `teams-fence`. At runtime [`Authorized::new`] checks that the permit
//!   was issued for the request's own target before anything reaches the [`Transport`], which sends the request's
//!   sealed wire form and returns the service's answer. The chat writes (a message post, an edit, a delete, a
//!   reaction and taking it back, a read receipt, typing, a new chat) are such requests, each with its target and the
//!   wire form the web client sent; a message's text is a [`chat::MessageText`], never raw HTML.
//!
//! On the push channel's socket the same holds for frames: a [`FrameSink`] sends only an [`OwnFrame`], the sealed,
//! reviewed list of the Trouter session's own frames (authentication, keep-alive, the message-loss acknowledgement,
//! the disconnect), each logged to `fence.log` before it is sent; there is no answer to a delivery and no
//! `user.activity`.
//!
//! [`HttpTransport`] is the live transport: a fixed host allowlist, `https` only, no proxy, no redirects, the account's
//! tokens from its `teams-auth` session. Only the `test-support` feature (tests, never a lab binary) points it at a
//! local mock ([`HttpTransport::with_mock_origin`]). It is also where the fence's live membership read takes the
//! native client's own credential from ([`teams_fence::CredentialSource`]): the account's `ic3` token and the
//! transport's chat base.

pub mod chat;
mod client;
mod endpoint;
mod frame;
mod http;
mod own;
mod post_read;
mod read;
mod write;

pub use client::{Client, PostReadError, Response, Transport, WriteError};
pub use endpoint::{Endpoint, Method, Service};
pub use frame::{
    Authenticate, ConnectParams, Disconnect, DroppedIndicator, FrameError, FrameSink, FrameText, FrameWriter,
    MessageLossAck, OwnFrame, Ping, Sent,
};
pub use http::{
    BuildError, CONNECT_TIMEOUT, Credentials, DEFAULT_RETRY_WAIT, Failure, GRAPH_AUDIENCE, GRAPH_HOST, HttpError,
    HttpTransport, IC3_AUDIENCE, MAX_BODY, MAX_RETRY_WAIT, MIDDLE_TIER_AUDIENCE, REQUEST_TIMEOUT, Refusal,
    TROUTER_HOST,
};
#[cfg(feature = "test-support")]
pub use own::ProbeOwnState;
pub use own::{
    CLIENT_BUILD, EndpointRegistration, MESSAGING_APP_ID, MESSAGING_TEMPLATE, OwnState, REGISTRATION_TTL,
    TROUTER_DOMAIN,
};
#[cfg(feature = "test-support")]
pub use post_read::ProbePostRead;
pub use post_read::{AnswerError, AuthorizedPostRead, PostRead, PostReadMismatch, ShortProfile, ShortProfileLookup};
pub use read::{
    GRAPH_PICTURE_SIZE, GRAPH_USER_SELECT, GraphAnswerError, GraphPicture, GraphUser, MAX_PICTURE, Picture,
    PictureFormat, Read, object_id,
};
pub use write::{
    AddToCall, Author, Authorized, ConversationUrl, CreateChat, DeleteMessage, EditMessage, Mutating, Original,
    ReadReceipt, RemoveReaction, SendMessage, SetReaction, StartCall, TEST_CALL_SCENARIO, TargetMismatch, Typing, Wire,
};
