//! Reads: requests that change nothing anyone could notice, always sent as `GET`.

use crate::endpoint::{Endpoint, Service};

/// A read. It names only its service and its endpoint under that service's base; the transport always sends it as a
/// `GET`, with no body. So any type may implement it: a read can't become a request of another method, can't carry a
/// body and can't name a host.
pub trait Read {
    fn service(&self) -> Service;
    fn endpoint(&self) -> Endpoint;
}
