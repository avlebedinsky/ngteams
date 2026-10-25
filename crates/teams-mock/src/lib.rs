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
//! both read-only; the team's other channel, read-only, in nobody's list, holding the posts and replies of
//! `channel-messages.json` (M2 Task 16). The ids are the constants below, made up (never a real tenant's).
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
//! **CSA** (`csa.rs`, M2 Task 16): its one read the native client sends, the account's teams and their channels (`GET
//! /api/csa/<region>/api/v3/teams/users/me` with the web client's query), from `tests/fixtures/csa/teams.json`, under
//! the CSA base `https://teams.microsoft.com/api/csa/amer`, taking the account's token alone.
//!
//! **UPS** (`ups.rs`, M2 Task 19): the account's own availability, its presence endpoint's `PUT` and its manual status
//! (set, or cleared with no body), in the shapes the web client sent, under the presence base [`PRESENCE_BASE`],
//! naming an endpoint the account registered with the registrar here; answered `201` and `200` with no body, as UPS
//! answered, and kept ([`ChatServiceMock::own_availability`]).
//!
//! **Calls** (`calling.rs`, M3 Task 13): the flight proxy's call requests under `/api/v2/` with the account's
//! skypetoken ([`ChatServiceMock::skypetoken`]): the start, the callee's `attach`, `progress`, endpoint state, `accept`
//! and `reject`, the acknowledgement, the endpoint metadata, `add`, `leave` and the join of a running call, each
//! answered in its captured shape; the invitations, acceptances and ends they cause delivered to the calling endpoints
//! the accounts registered, through the mock's Trouter; calls a caller the mock plays places
//! ([`ChatServiceMock::ring`]), their ring's end and their hang-up; every call's record ([`ChatServiceMock::calls`]).
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

mod calling;
mod chatsvc;
mod csa;
mod fixtures;
#[cfg(feature = "media")]
pub mod media;
mod reads;
mod trouter;
mod ups;
mod world;
mod writes;

use std::fmt::Write as _;
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;
use wiremock::matchers::{any, method};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

pub use calling::{ANSWERED_ELSEWHERE, CallPhase, Leg, MockCall, TIMED_OUT};
pub use fixtures::FixtureError;
pub use trouter::{CALLING_APP, MESSAGING_APP, REGIONAL_NAME, Registration, SocketRecord, TrouterMock};
pub use ups::{AvailabilityChange, OwnAvailability};

/// The region of the mock's chat base.
pub const REGION: &str = "amer";

/// The chat base a client is built with (`chatServiceAfd` as the region's GTMs give it); its requests go to
/// [`ChatServiceMock::origin`] with this base's path.
pub const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";

/// The CSA base a client is given (`chatSvcAggAfd` as the region's GTMs give it); its requests go to
/// [`ChatServiceMock::origin`] with this base's path.
pub const CSA_BASE: &str = "https://teams.microsoft.com/api/csa/amer";

/// The presence base a client is given (`presenceUPS` as the region's GTMs give it); its requests go to
/// [`ChatServiceMock::origin`] with this base's path.
pub const PRESENCE_BASE: &str = "https://teams.microsoft.com/ups/noam";

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

/// The echo service (a bot): the test call adds it; a fence approves it as a service participant.
pub const ECHO: &str = "28:00000000-0000-4000-8000-0000000000ec";

/// The a–b 1:1 chat, holding the messages of `messages.json`.
pub const AB_CHAT: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
/// The a–c 1:1 chat, empty.
pub const AC_CHAT: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000c@unq.gbl.spaces";
/// The 1:1 chat of a and the outsider, empty.
pub const AX_CHAT: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-0000000000ff@unq.gbl.spaces";
/// The a–b–c group chat of `thread-group.json`, empty.
pub const ABC_CHAT: &str = "19:00000000000000000000000000000abc@thread.v2";

/// A `lab.toml` (`teams_fence::lab::LabConfig::parse`) of the mock's lab: accounts a, b and c with the MRIs
/// [`LAB_A`], [`LAB_B`] and [`LAB_C`], made-up UPNs, its data dir `data_dir`. A fence or a live membership source of
/// it knows the lab's group chat once that dir's `chats.toml` records [`ABC_CHAT`].
#[must_use]
pub fn lab_toml(data_dir: &std::path::Path) -> String {
    let accounts =
        [("a", LAB_A), ("b", LAB_B), ("c", LAB_C)]
            .into_iter()
            .fold(String::new(), |mut out, (alias, mri)| {
                let _ = write!(
                    out,
                    "\n[[accounts]]\nalias = \"{alias}\"\nupn = \"bot-{alias}@lab.example.test\"\nmri = \"{mri}\"\n"
                );
                out
            });
    format!("data_dir = {:?}\n{accounts}", data_dir.display().to_string())
}

/// A team channel in a's list: read-only. It is the team's id and its general channel's (`csa/teams.json`).
pub const CHANNEL: &str = "19:channel0001@thread.tacv2";
/// The team's other channel (`csa/teams.json`, and the team's `topics` in `conversations.json`): a, b and c are its
/// members, it holds the posts and replies of `channel-messages.json`, it is in nobody's conversation list (the chat
/// service lists a team's general channel only), and its thread is a `topic`. Read-only.
pub const TEAM_CHANNEL: &str = "19:channel0002@thread.tacv2";
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

/// Matches a request whose path (still percent-encoded) ends with the string held.
struct PathEnds(String);

impl Match for PathEnds {
    fn matches(&self, request: &Request) -> bool {
        request.url.path().ends_with(&self.0)
    }
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

    /// `from` posts `html` (a rich-text message) to `chat`, as one of its clients would: stored as a read gives it back
    /// and queued for the chat's other members, as any post. Its id, or `None` when `from` may not write there.
    #[must_use = "None says the post was refused"]
    pub fn post_as(&self, from: &str, chat: &str, html: &str) -> Option<String> {
        writes::post_text(&mut self.lock(), from, chat, html)
    }

    /// Changes the message of `conversation` that carries `client_message_id` as `change` says, as every later read
    /// gives it (a test's stand-in for an entry a client can't read); its `version` is whatever `change` leaves, so a
    /// sync link already past it doesn't bring it again. `false` when there is no such message.
    pub fn change_message(&self, conversation: &str, client_message_id: &str, change: impl FnOnce(&mut Value)) -> bool {
        let mut world = self.lock();
        let message = world.conversation_mut(conversation).and_then(|conversation| {
            conversation
                .messages
                .iter_mut()
                .find(|message| message["clientmessageid"].as_str() == Some(client_message_id))
        });
        let Some(message) = message else {
            return false;
        };
        change(message);
        true
    }

    /// Adds a copy of the message of `conversation` that carries `client_message_id`, changed as `change` says, as
    /// the conversation's newest entry, the original kept as it is (a test's stand-in for a second entry of one
    /// message, one a client can't read, met by the same read as the readable one). `false` when there is no such
    /// message.
    pub fn copy_message(&self, conversation: &str, client_message_id: &str, change: impl FnOnce(&mut Value)) -> bool {
        let mut world = self.lock();
        let Some(conversation) = world.conversation_mut(conversation) else {
            return false;
        };
        let Some(mut copy) = conversation
            .messages
            .iter()
            .find(|message| message["clientmessageid"].as_str() == Some(client_message_id))
            .cloned()
        else {
            return false;
        };
        change(&mut copy);
        conversation.messages.push(copy);
        true
    }

    /// Answers the next request of `http_method` whose path ends with `path_end` with `status` and no body, before the
    /// world sees it: a test's stand-in for a write the service refuses (the world doesn't change and nobody is told).
    /// Every later request is answered as before.
    pub async fn refuse_next(&self, http_method: &str, path_end: &str, status: u16) {
        Mock::given(method(http_method))
            .and(PathEnds(path_end.to_owned()))
            .respond_with(ResponseTemplate::new(status))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&self.server)
            .await;
    }

    /// The skypetoken the mock's calling service takes as `mri`'s (`X-Skypetoken`), if `mri` is one of its accounts.
    #[must_use]
    pub fn skypetoken(&self, mri: &str) -> Option<String> {
        self.lock()
            .account(mri)
            .map(|account| calling::skypetoken_of(&account.mri))
    }

    /// A caller the mock plays (`from`, no client of its own) rings `to` in their 1:1 call, on every calling endpoint
    /// `to` registered: the call's id.
    #[must_use = "the call's id names it"]
    pub fn ring(&self, from: &str, to: &str) -> String {
        calling::ring(&mut self.lock(), from, &[to.to_owned()], None)
    }

    /// A caller the mock plays (`from`) rings `callees` in the group chat `chat`: the call's id.
    #[must_use = "the call's id names it"]
    pub fn ring_group(&self, from: &str, chat: &str, callees: &[&str]) -> String {
        let callees: Vec<String> = callees.iter().map(|callee| (*callee).to_owned()).collect();
        calling::ring(&mut self.lock(), from, &callees, Some(chat.to_owned()))
    }

    /// Call `id`'s ring ends with `code` and `phrase` (its caller cancelled, it timed out: see [`TIMED_OUT`]). `false`
    /// when there is no such call.
    #[must_use = "false says there was no such call"]
    pub fn end_ring(&self, id: &str, code: i64, phrase: &str) -> bool {
        calling::end_ring(&mut self.lock(), id, code, phrase)
    }

    /// Call `id` is ended by the party the mock plays: everyone in it gets its end. `false` when there is no such call.
    #[must_use = "false says there was no such call"]
    pub fn hang_up_call(&self, id: &str) -> bool {
        calling::hang_up(&mut self.lock(), id)
    }

    /// The calling service's calls so far, oldest first.
    #[must_use]
    pub fn calls(&self) -> Vec<MockCall> {
        self.lock().calling.calls.clone()
    }

    /// The own-availability writes UPS took, in order (see the crate docs).
    #[must_use]
    pub fn own_availability(&self) -> Vec<OwnAvailability> {
        self.lock().availability.clone()
    }

    /// Every request the mock got, in order (to check what a client sent, and how often).
    pub async fn received_requests(&self) -> Vec<Request> {
        self.server.received_requests().await.unwrap_or_default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, world::State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
