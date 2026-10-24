//! Plain requests: anything the native client sends that isn't a read (`GET`) or a contacting write, in any method,
//! with any body. The trait is open: any crate may add one, with no capture or review, because the transport decides
//! whether it may go out by what it is, not by who made it.
//!
//! [`Transport::send`](crate::Transport::send) builds the request's URL from its [`Service`]'s base and asks
//! [`teams_fence::contacting`], the lab rule's one classifier: a request that reaches nobody
//! ([`teams_fence::Contact::None`]: the account's own session, settings and endpoints, a read sent as a `POST`, a
//! profile lookup or a presence read whoever it names) is sent, and logged to `fence.log` as `allowed` before it goes
//! when it is mutating; a write ([`teams_fence::Contact::Write`]) or a mutating request on a contacting service that no
//! row reads ([`teams_fence::Contact::Unreadable`]) is refused before anything is sent. A contacting write goes out
//! only as a sealed [`Mutating`](crate::Mutating) type under its permit ([`crate::Client::write`]).

use serde_json::Value;

use crate::endpoint::{Endpoint, Method, Service};

/// A plain request (see the module docs): its service, method, endpoint under the service's base, and JSON body.
pub trait Request {
    fn service(&self) -> Service;
    fn method(&self) -> Method;
    fn endpoint(&self) -> Endpoint;
    /// The JSON body, if it has one.
    fn body(&self) -> Option<Value>;
}
