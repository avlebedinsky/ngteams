//! Teams internal API requests and the native request policy. Every contacting write takes a `WritePermit` that only
//! `teams-fence` can issue.
//!
//! A request comes in one of three classes, and a [`Transport`] takes nothing else:
//! - a [`Read`]: always sent as a `GET`, with no body (the chat service's, Microsoft Graph's reads of a person's name
//!   and picture, [`GraphUser`] and [`GraphPicture`], and CSA's read of the account's teams, [`CsaTeams`]);
//! - a plain [`Request`]: any method and body, the trait open to any crate. The transport builds its URL from its
//!   [`Service`]'s base and asks [`teams_fence::contacting`], the lab rule's one classifier: sent when it reaches nobody
//!   (the messaging stack's [`EndpointRegistration`], the middle tier's profile lookup [`ShortProfileLookup`], UPS's
//!   presence read [`PresenceRead`], whoever they name), logged to `fence.log` as `allowed` before it goes; refused with
//!   a typed error, before anything is sent, when it is a write or a request on a contacting service the classifier
//!   can't read;
//! - a write: a request that reaches other people implements [`Mutating`] and names its one target. [`Client::write`]
//!   takes the request together with a [`WritePermit`](teams_fence::WritePermit), so a mutating call without a permit
//!   doesn't compile, and a permit can't be made outside `teams-fence`. At runtime [`Authorized::new`] checks that the
//!   permit was issued for the request's own target before anything reaches the [`Transport`], which sends the
//!   request's sealed wire form and returns the service's answer. The chat writes (a message post, an edit, a delete, a
//!   reaction and taking it back, a read receipt, typing, a new chat) are such requests, each with its target and the
//!   wire form the web client sent; a message's text is a [`chat::MessageText`], never raw HTML. So is the account's own
//!   availability (UPS's presence endpoint `PUT`, [`SetEndpointAvailability`], and the manual status,
//!   [`ForceAvailability`]), on the account itself, naming only the endpoint a registration of this run registered for
//!   that account ([`RegisteredEndpoint`], which only [`register`] gives, over the native transport:
//!   [`EndpointRegistrar`], recorded for its fence's own account).
//!
//! The personal build's sealed lists stay beside them: [`OwnState`] and [`PostRead`] (each entry also a plain request),
//! sent by [`HttpTransport::own_state`] and [`HttpTransport::post_read`] (the latter only inside an
//! [`AuthorizedPostRead`] with the [`PostReadPermit`](teams_fence::PostReadPermit) `Fence::check_post_read` gave for its
//! exact MRIs, in its entry's captured shape, once).
//!
//! On the push channel's socket a [`FrameSink`] sends only an [`OwnFrame`], a typed frame this crate builds (today the
//! Trouter session's own: authentication, keep-alive, the message-loss acknowledgement, the disconnect), never raw
//! text, and none of them is written to `fence.log` (a frame isn't a contacting write). There is no answer to a
//! delivery.
//!
//! [`HttpTransport`] is the live transport: one base per service, `https` only, no proxy, no redirects, each of the
//! account's tokens (from its `teams-auth` session) only to its own service's host. Only the `test-support` feature
//! (tests, never a lab binary) points it at a local mock (`HttpTransport::with_mock_origin`). It is also where the
//! fence's live membership read takes the native client's own credential from ([`teams_fence::CredentialSource`]):
//! the account's `ic3` token and the transport's chat base.

pub mod chat;
mod client;
mod endpoint;
mod frame;
mod http;
mod own;
mod post_read;
mod read;
mod request;
mod write;

pub use client::{Client, PostReadError, Response, Transport, WriteError};
pub use endpoint::{Endpoint, Method, Service};
pub use frame::{
    Authenticate, ConnectParams, Disconnect, DroppedIndicator, FrameError, FrameSink, FrameText, FrameWriter,
    MessageLossAck, OwnFrame, Ping, Sent,
};
pub use http::{
    BuildError, CONNECT_TIMEOUT, CSA_AUDIENCE, Credentials, DEFAULT_RETRY_WAIT, Failure, GRAPH_AUDIENCE, GRAPH_HOST,
    HttpError, HttpTransport, IC3_AUDIENCE, MAX_BODY, MAX_RETRY_WAIT, MIDDLE_TIER_AUDIENCE, PRESENCE_AUDIENCE,
    REQUEST_TIMEOUT, Refusal, TROUTER_HOST,
};
#[cfg(feature = "test-support")]
pub use own::ProbeOwnState;
pub use own::{
    CLIENT_BUILD, EndpointRegistrar, EndpointRegistration, MESSAGING_APP_ID, MESSAGING_TEMPLATE, OwnState,
    REGISTRATION_TTL, RegisteredEndpoint, TROUTER_DOMAIN, register,
};
#[cfg(feature = "test-support")]
pub use post_read::ProbePostRead;
pub use post_read::{
    AnswerError, AuthorizedPostRead, PostRead, PostReadMismatch, PresenceRead, ShortProfile, ShortProfileLookup,
    UserPresence,
};
pub use read::{
    CSA_TEAMS_PATH, CSA_TEAMS_QUERY, Channel, CsaAnswerError, CsaTeams, GRAPH_PICTURE_SIZE, GRAPH_USER_SELECT,
    GraphAnswerError, GraphPicture, GraphUser, MAX_PICTURE, Picture, PictureFormat, Read, Team, TeamsAnswer, object_id,
};
pub use request::Request;
pub use write::{
    AddToCall, Author, Authorized, ConversationUrl, CreateChat, DeleteMessage, EditMessage, ForceAvailability,
    ManualAvailability, Mutating, Original, ReadReceipt, RemoveReaction, SendMessage, SetEndpointAvailability,
    SetReaction, StartCall, TEST_CALL_SCENARIO, TargetMismatch, Typing, Wire,
};
