//! Teams internal API requests. Every mutating request takes a `WritePermit` that only `teams-fence` can issue.
//!
//! A request others could notice implements [`Mutating`] and names its one target. [`Client::write`] takes the
//! request together with a [`WritePermit`](teams_fence::WritePermit), so a mutating call without a permit doesn't
//! compile, and a permit can't be made outside `teams-fence`. At runtime [`Authorized::new`] checks that the permit was
//! issued for the request's own target before anything reaches the [`Transport`].

mod client;
mod write;

pub use client::{Client, Transport, WriteError};
pub use write::{AddToCall, Authorized, Mutating, SendMessage, StartCall, TEST_CALL_SCENARIO, TargetMismatch};
