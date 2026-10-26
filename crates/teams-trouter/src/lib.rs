//! The native client's push channel (M1 Task 16): Trouter, socket.io 0.9 over WSS, as `docs/protocol/trouter.md`
//! records it from M1 Task 14's captures.
//!
//! ```text
//! let trouter = Trouter::new(transport, session)?;  // the account's HTTP transport, its session
//! let mut push = trouter.spawn();
//! while let Some(event) = push.next().await {
//!     match event {
//!         PushEvent::Connected { .. } => { /* catch up (Task 17) */ }
//!         PushEvent::Delivery(delivery) => { /* apply delivery.event by conversation, id and version */ }
//!         PushEvent::MessageLoss(loss) => { /* catch up, then */ loss.caught_up(); }
//!         _ => {}
//!     }
//! }
//! ```
//!
//! **What it sends.** The socket sends only `teams_proto`'s typed frames ([`teams_proto::OwnFrame`]:
//! `user.authenticate`, the keep-alive ping, the message-loss acknowledgement, the disconnect), each through the
//! connection's [`teams_proto::FrameSink`], never raw text; a lab sink writes none to `fence.log` (a frame isn't a
//! contacting write; the personal build's logs each by its kind). The socket's write half goes to that sink when the
//! socket opens, and nothing else holds it ([`socket`]): there is no other way to send on it. No `user.activity`
//! (deliveries come without it), and **no answer to any delivery** but a call's typed response on the calling stack's
//! socket ([`calls`]): the messaging socket's deliveries stay unanswered, an incoming call's included.
//!
//! **Two stacks, two sockets.** A [`Trouter`] is the messaging stack's socket (chat deliveries); built
//! [`Trouter::for_calls`], it is the calling stack's own (`SkypeSpaces`, its own endpoint): its deliveries read by the
//! fence and handed on as [`PushEvent::Call`], an invitation the fence doesn't offer ignored, a delivery answered only
//! by a typed response the fence permits for that call on that connection ([`calls`]). The personal build's calling
//! socket registers as its own-state entry and logs its frames, as its messaging socket does.
//!
//! **What it requests over HTTP.** The registrar registration of the endpoint for its stack and its renewal
//! ([`registration`]): `teams_proto::EndpointRegistration` (`TeamsCDLWebWorker`) or
//! `teams_proto::CallingRegistration` (`SkypeSpacesWeb`), the account's own state (it reaches nobody), sent through
//! the account's transport by `Transport::send_own` (a plain request logged to `fence.log` as `allowed` first, or the
//! personal build's own-state entry logged by its kind; the `ic3` token from the session).
//!
//! **Where it connects.** `wss://go.trouter.teams.microsoft.com/v4/c`, then the regional `<name>-t` host a
//! `trouter.connected` names: a fixed allowlist ([`socket::allowed`]), the server certificate checked by the system's
//! verifier. Only the `test-support` feature (tests, never a lab binary) points it at a loopback mock
//! ([`Trouter::with_mock_origin`]), the URL built and checked as above first.
//!
//! **What it hands on** ([`PushEvent`]): connects (catch up after each), registrations (and the endpoint each one
//! that went through registered, `teams_proto::RegisteredEndpoint`: the one id the account's own availability may
//! name, M2 Task 19), message-loss reports,
//! deliveries as typed chat events ([`events`]: new messages, typing, edits, deletes, reactions, the read horizon,
//! the chat's updates; anything else `Unknown`, logged by its kind only), ignored deliveries, disconnects and the
//! stop. Nothing here logs a token: tokens are [`teams_auth::secrets::Secret`]s, and errors carry no token, frame or
//! body. A frame's text reaches a log only at `debug`, every frame both ways, as a `teams_log::wire::Frame` record
//! (under `teams_log::wire::FRAME_TARGET`), whose formatting redacts `user.authenticate`'s arguments and every
//! credential field (the `connectparams` signature of `trouter.connected` among them); no other log line carries a
//! frame's text.

pub mod calls;
pub mod credentials;
pub mod events;
pub mod frames;
pub mod registration;
mod session;
pub mod socket;

pub use calls::{CallDelivery, CallResponder, CallResponse, CallStack, RespondFailure};
pub use credentials::TrouterCredentials;
pub use events::{Change, ChatEvent, Delivery};
pub use registration::{RegistrationReport, RegistrationTimes};
pub use session::{BuildError, Down, Ignored, MessageLoss, Options, PushEvent, Stop, Trouter, TrouterHandle};
