//! A call's signaling (M3 Task 13): the state machine of the account's calls, sans-IO. It builds each request a call
//! makes as a sealed `teams-proto` write and **hands it on; it sends nothing itself**. The engine owns it, feeds it,
//! and carries out what it asks (`docs/protocol/calling.md`, `docs/protocol/calling-echo.md`).
//!
//! ```text
//! let mut calls = Calls::new(identity);
//! calls.set_push_url(push_url)?;                  // the calling socket's `trouter.connected`
//! let call = calls.place(&placement, offer, now)?; // or a delivered invitation, answered by `calls.answer(…)`
//! while let Some(effect) = calls.poll() {
//!     match effect {
//!         Effect::Send { id, write } => write.submit_to(id, &mut sink), // fence, outbox; `calls.on_outcome(id, …)`
//!         Effect::Respond { delivery, reply } => { /* the delivery's own responder; `on_response_failed` */ }
//!         Effect::Event(event) => { /* tell the account */ }
//!     }
//! }
//! ```
//!
//! **In**: the account's actions ([`Calls::place`], [`Calls::join`], [`Calls::answer`], [`Calls::decline`],
//! [`Calls::hang_up`]), each call delivery as the fence read it ([`Calls::on_delivery`]: the calling socket's
//! `teams_fence::Delivery` and `Observed`), each write's outcome under its [`WriteId`] ([`Calls::on_outcome`]), a
//! delivery response that didn't go ([`Calls::on_response_failed`]) and the time ([`Calls::tick`]). **Out**
//! ([`Calls::poll`]): [`Effect::Send`] (a [`CallWrite`], handed to the engine's [`WriteSink`] as its own sealed type,
//! checked by the fence and sent in order), [`Effect::Respond`] (a typed delivery response, for the delivery's own
//! responder) and [`Effect::Event`].
//!
//! **The calls, as captured:**
//! - **Placed**: the start (a chat's call or the test call), its answer the conversation; the acceptance, delivered at
//!   the start's own `acceptance` callback, carries the answer to the account's offer and is acknowledged by its
//!   delivery response (by request only when that response failed); then the account's endpoint metadata and, for the
//!   test call once that was taken, the `add` of the echo service. An acceptance delivered before the start's answer
//!   is kept and applied once the answer came. A hang-up while it rings is a cancel (`leave` with how long it rang),
//!   later a `leave`.
//! - **Joined**: a running group call an invitation named ([`Calls::joinable`]: forgotten once its conversation's end
//!   is delivered or a join to it fails), its conversation joined with the account's offer; then as placed.
//! - **Invited**: an invitation the fence offered (`Observed::Offered`) gets its receipt and the `attach` at once,
//!   with no action of the account's; once the `attach` is answered it rings here (`progress`,
//!   [`CallEvent::Incoming`]). Nothing answers it but [`Calls::answer`] (endpoint state, then, once that was taken,
//!   `accept`) and nothing declines it but [`Calls::decline`], or [`Calls::hang_up`] while it is being answered and
//!   its `accept` hasn't gone out yet (until then the account isn't in the call). An invitation the fence doesn't offer never reaches the
//!   machine, and one that comes while a call is up isn't offered: nothing is sent for it.
//! - **Ends**: any delivery at the call's own `call/end` or `conversationEnd` callback ends the call (or the ring, an
//!   offered invitation's: [`CallEvent::RingEnded`]), whatever its body; what the body says is kept as delivered
//!   ([`RemoteEnd`]), no reason read into a code no capture named. A step that fails (not sent, or not answered `2xx`)
//!   ends the call, with a `leave` when the account was in it; nothing else of it is ever sent again (a write that
//!   waits for the failed one, or that wasn't handed out yet, never goes). A decline or a `leave` that wasn't taken
//!   ends it [`End::Failed`] too.
//! - **Keep-alives**: the interval the acceptance or the `accept`'s answer asks for (`callKeepAliveInterval`) is kept
//!   and its deadline told ([`CallEvent::KeepAliveDue`]), but no request goes: no capture shows what the keep-alive
//!   is yet.
//! - **Not sent**: the account's own participant properties (its AI voice consent), which the web client posts once it
//!   answered: nothing of the call waits for them. Media renegotiation (the camera) comes with the call session.
//!
//! The fence's call registry stays the authority on what may be sent: this crate keeps only what it needs to build
//! the next request.

mod agent;
mod build;
mod call;
mod machine;
mod read;
mod write;

pub use machine::{
    CallError, CallEvent, CallId, CallState, Calls, DeliveryKey, DeliveryReply, Effect, End, Identity, Incoming,
    Outcome, Placement,
};
pub use read::RemoteEnd;
pub use write::{CallWrite, WriteId, WriteSink};
