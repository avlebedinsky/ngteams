//! The call session in the engine (M3 Task 14), on the mock and its loopback media peer: each lab account's engine
//! with its own tracker, messaging and calling sockets, its calls' media on the file backend through the
//! mock's loopback TURN server (`support`).
//!
//! - `flows`: a call connects and a tone crosses both ways (the test call, a → b native on both ends, the invitation
//!   of a caller the mock plays, a running group call joined); a renegotiation goes either way; mute; a call outlives
//!   its calling socket's reconnect.
//! - `failures`: the native signaling feeds the call tracker; what a failure does (Task 14's decision with
//!   Codex); the engine stopping hangs up.
//! - `share`: a 1:1 call's screen shared either way, and the far end's shown.
//! - `health`: a running call's health read from its media (the Call health pane's), and none once it ended.
//!
//! Every id, token and MRI here is the mocks' made-up one.

// test code throughout (its helpers unwrap and panic like the tests do)
#![cfg(test)]

mod captions;
mod diagnostics;
mod failures;
mod flows;
mod group;
mod health;
mod in_call;
mod meeting_state;
mod recording;
mod share;
mod support;
mod video;
