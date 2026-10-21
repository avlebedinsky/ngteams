//! A local Teams chat service for tests (M1 Task 9), built from the synthetic chat fixtures in
//! `tests/fixtures/chatsvc/`, so the whole native client can be tested offline. Its answers have the shapes those
//! fixtures recorded (`docs/protocol/chat.md`, `docs/protocol/fence-targets.md`); no capture or reference copy is
//! read, here or by a test.
//!
//! **Reach and use.** A dev-dependency only, never a lab binary's dependency (a test in this crate checks every
//! workspace manifest). Clients reach it only through the `test-support` mock origins:
//! `teams_proto::HttpTransport::with_mock_origin` and `teams_fence::LiveMembershipSource::with_mock_origin`, each
//! given [`ChatServiceMock::origin`] with [`CHAT_BASE`] as the chat base, so every URL is built and checked as it would
//! be for the real service first.
//!
//! ```text
//! let mock = ChatServiceMock::start().await?;
//! let trouter = mock.start_trouter()?; // its push channel, at trouter.origin() (M1 Task 16)
//! let token = mock.token(LAB_A).ok_or("no account")?; // a's `ic3` token, for the client's credentials
//! let transport = HttpTransport::with_mock_origin(credentials, fence, CHAT_BASE, &mock.origin())?;
//! // … a sends to AB_CHAT through the fence …
//! let events = mock.take_events(LAB_B); // what b would be told, while no socket of b's takes it
//! ```
//!
//! **The world** it starts with ([`ChatServiceMock::start`]): the lab accounts a, b and c and one outsider, x; the a–b
//! 1:1 chat holding the messages of `messages.json` (an add-member event, plain, edited, quoted and deleted
//! messages); an empty a–c chat (`conversations.json`); an empty a–x chat with the outsider; the a–b–c group chat of
//! `thread-group.json`; and, in a's list only, a team channel and a's notifications stream (`conversations.json`),
//! both read-only. The ids are the constants below, made up (never a real tenant's).
//!
//! **What it serves** (paths under [`CHAT_BASE`]):
//! - reads: the conversation list (`GET v1/users/ME/conversations`, and the sync link it hands out, which names the
//!   caller's MRI), message pages (`GET …/conversations/<id>/messages`, first page, sync and backward links) and the
//!   page around a time (`…/messages/epochTimeStamp/<ms>`), the thread read (`GET v1/threads/<id>`), the member list
//!   (`…/members`) and the filtered view (`…/filtered`, refused as the service refused it);
//! - writes: message posts and typing (`POST …/messages`), edits (`PUT …/messages/<id>`), soft deletes, reactions
//!   (`PUT`/`DELETE …/messages/<id>/properties?name=emotions`), read receipts (`PUT
//!   …/conversations/<id>/properties?name=consumptionhorizon`) and chat creation (`POST v1/threads`). Each changes
//!   the state every later read shows and queues a [`ChatEvent`] for each **other** member of the chat
//!   ([`ChatServiceMock::take_events`]); the writer gets none.
//!
//! **Trouter** ([`ChatServiceMock::start_trouter`], `trouter.rs`): connect, authenticate, `trouter.connected`, pings,
//! the deliveries of the queued events and their replay on connect, from the synthetic `tests/fixtures/trouter/`
//! fixtures; the registrar's `POST /registrar/prod/V2/registrations` is served on this origin.
//!
//! **Headers** (M1 Task 3, `docs/protocol/auth.md`): every request must carry `Authorization: Bearer <token>` with a
//! token the mock handed out ([`ChatServiceMock::token`]), plus `X-MS-Migration: True` and `BehaviorOverride:
//! redirectAs404`, the web client's set the native transport sends. One exception: the thread read
//! (`GET v1/threads/<id>`) takes the token alone, the form the fence's own membership read sends and Task 3 saw the
//! service answer in full. Anything short of that is a 401 with no body (no 401 of the chat service was ever
//! captured, so its body isn't known).
//!
//! **What it doesn't know** it doesn't make up: a request it doesn't model is a 404, a malformed body of one it
//! models a 400, a write by someone who isn't a member (or to a read-only conversation, or an edit or delete of
//! someone else's message) a 403, all with no body. The service's answers to those weren't captured. Its own links
//! (`syncState`, `backwardLink`) carry opaque tokens of its own: the real ones are opaque too, and a client follows
//! them as full URLs. A 1:1 chat's thread read wasn't kept: it takes the group chat's (`thread-group.json`, its
//! `capabilities` among them) with the `uniquerosterthread` the conversation list gives the chat; its `fixedRoster`
//! and its own capabilities aren't known, so they aren't made up.

mod chatsvc;
mod fixtures;
mod reads;
mod trouter;
mod world;
mod writes;

use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, Request};

pub use fixtures::FixtureError;
pub use trouter::{REGIONAL_NAME, Registration, SocketRecord, TrouterMock};

/// The region of the mock's chat base.
pub const REGION: &str = "amer";

/// The chat base a client is built with (`chatServiceAfd` as the region's GTMs give it); its requests go to
/// [`ChatServiceMock::origin`] with this base's path.
pub const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";

/// The regional host the service names new resources on in a 201's `Location` (`docs/protocol/fence-targets.md`).
pub const REGIONAL_HOST: &str = "https://amer.ng.msg.teams.microsoft.com";

/// The made-up tenant every account and chat belongs to.
pub const TENANT: &str = "00000000-0000-4000-8000-000000000001";

/// Lab account a.
pub const LAB_A: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
/// Lab account b.
pub const LAB_B: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
/// Lab account c.
pub const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
/// An account outside the lab, in a 1:1 chat with a ([`AX_CHAT`]): a fence over the lab roster denies writes there.
pub const OUTSIDER: &str = "8:orgid:00000000-0000-4000-8000-0000000000ff";

/// The a–b 1:1 chat, holding the messages of `messages.json`.
pub const AB_CHAT: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
/// The a–c 1:1 chat, empty.
pub const AC_CHAT: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000c@unq.gbl.spaces";
/// The 1:1 chat of a and the outsider, empty.
pub const AX_CHAT: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-0000000000ff@unq.gbl.spaces";
/// The a–b–c group chat of `thread-group.json`, empty.
pub const ABC_CHAT: &str = "19:00000000000000000000000000000abc@thread.v2";
/// A team channel in a's list: read-only.
pub const CHANNEL: &str = "19:channel0001@thread.tacv2";
/// a's notifications stream: read-only.
pub const NOTIFICATIONS: &str = "48:notifications";

/// What a write did, queued for each other member of its chat, until the mock's Trouter ([`TrouterMock`]) delivers it
/// to one of the member's registered endpoints (a message, typing, an edit, a delete, a reaction; read receipts and
/// chat creation stay queued: no capture showed their delivery to another member).
#[derive(Clone, Debug, PartialEq)]
pub struct ChatEvent {
    /// The chat it happened in.
    pub conversation: String,
    /// The writer's MRI.
    pub from: String,
    pub kind: EventKind,
    /// The message as a read gives it after the write ([`EventKind::Message`], [`EventKind::Edit`],
    /// [`EventKind::Delete`], [`EventKind::Reaction`]); the thread read for [`EventKind::ChatCreated`];
    /// `{"consumptionhorizon": …}` for [`EventKind::ReadReceipt`]; `{"messagetype": …}` for [`EventKind::Typing`].
    pub resource: Value,
}

/// The kind of a [`ChatEvent`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Message,
    Typing,
    Edit,
    Delete,
    Reaction,
    ReadReceipt,
    ChatCreated,
}

/// The running mock: a local HTTP server on loopback and the state its answers come from. Dropping it stops serving.
pub struct ChatServiceMock {
    server: MockServer,
    state: Arc<Mutex<world::State>>,
}

impl std::fmt::Debug for ChatServiceMock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatServiceMock")
            .field("origin", &self.server.uri())
            .finish_non_exhaustive()
    }
}

impl ChatServiceMock {
    /// Starts the mock with its world (see the crate docs) on a free loopback port.
    ///
    /// # Errors
    /// When an embedded fixture doesn't have the shape the mock reads from it.
    pub async fn start() -> Result<Self, FixtureError> {
        let state = Arc::new(Mutex::new(world::State::lab()?));
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(chatsvc::ChatService::new(Arc::clone(&state)))
            .mount(&server)
            .await;
        Ok(Self { server, state })
    }

    /// The origin clients send to (`http://127.0.0.1:<port>`), for the `test-support` mock origins.
    #[must_use]
    pub fn origin(&self) -> String {
        self.server.uri()
    }

    /// The bearer token the mock takes as `mri`'s (an `ic3` token), if `mri` is one of its accounts.
    #[must_use]
    pub fn token(&self, mri: &str) -> Option<String> {
        self.lock().account(mri).map(|account| account.token.clone())
    }

    /// Replaces `mri`'s token: from now on the old one gets a 401. `false` when `mri` isn't one of its accounts.
    pub fn set_token(&self, mri: &str, token: impl Into<String>) -> bool {
        self.lock().set_token(mri, token.into())
    }

    /// Starts the mock's Trouter over this mock's world, on a free loopback port of its own: it delivers the events
    /// queued here to the sockets of registered endpoints (see [`TrouterMock`]), and the registrar it needs is served
    /// on this mock's own origin.
    ///
    /// # Errors
    /// When no port can be bound, or an embedded Trouter fixture doesn't have the shape the mock reads.
    pub fn start_trouter(&self) -> std::io::Result<TrouterMock> {
        TrouterMock::start(Arc::clone(&self.state))
    }

    /// Takes the events queued for `mri` so far, oldest first; the queue is empty afterwards.
    #[must_use]
    pub fn take_events(&self, mri: &str) -> Vec<ChatEvent> {
        self.lock().events.remove(mri).unwrap_or_default()
    }

    /// Queues `event` for `mri` as the service would for one of its endpoints: the writer's own echo of its message,
    /// say, which the mock never queues by itself. It is delivered as any queued event is (see [`TrouterMock`]).
    pub fn queue_event(&self, mri: &str, event: ChatEvent) {
        let mut world = self.lock();
        world.events.entry(mri.to_owned()).or_default().push(event);
        world.changed();
    }

    /// Every request the mock got, in order (to check what a client sent, and how often).
    pub async fn received_requests(&self) -> Vec<Request> {
        self.server.received_requests().await.unwrap_or_default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, world::State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
