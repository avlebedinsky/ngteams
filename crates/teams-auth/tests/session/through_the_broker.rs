//! The session's cases through the credential broker: a `BrokerSecretStore` on a socket pair, the broker serving a
//! `MemorySecretStore` for the session's account.

const KIND: crate::Kind = crate::Kind::Broker;

#[expect(
    clippy::duplicate_mod,
    reason = "the same cases run once per store: in_memory.rs loads them too, under another KIND"
)]
#[path = "cases.rs"]
mod cases;
