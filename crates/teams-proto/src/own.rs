//! Own-state requests: the reviewed list of requests that aren't reads but touch only the signed-in account's own
//! state (registering its own endpoint, say), sent without a membership check.
//!
//! The list is the set of types implementing [`OwnState`], and the trait is sealed, so only this crate adds to it.
//! **It starts empty** (M1 Task 5): each entry is added by the task that captured and reviewed it, in its exact shape
//! (method, service, path, body schema), the way `proto-lab`'s browser fence adds its own-state kinds. The transport
//! logs every own-state request to `fence.log` before it sends it, and sends none it couldn't log.

use serde_json::Value;

use crate::endpoint::{Endpoint, Method, Service};

/// A reviewed own-state request (see the module docs). Sealed: no other crate can implement it.
pub trait OwnState: sealed::Sealed {
    /// The entry's name, logged to `fence.log` with every request of it.
    const KIND: &'static str;

    fn service(&self) -> Service;
    fn method(&self) -> Method;
    fn endpoint(&self) -> Endpoint;
    /// The JSON body, if it has one.
    fn body(&self) -> Option<Value>;
}

mod sealed {
    /// The seal on [`OwnState`](super::OwnState): a public trait no other crate can name. Its implementations are the
    /// reviewed list; there are none yet.
    pub trait Sealed {}

    #[cfg(feature = "test-support")]
    impl Sealed for super::ProbeOwnState {}
}

/// An own-state request of the test's choosing, `POST`ed: the transport's tests need one, and the reviewed list is
/// empty. Tests only (`test-support`), which a lab binary refuses to run with.
#[cfg(feature = "test-support")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeOwnState {
    service: Service,
    endpoint: Endpoint,
    body: Option<Value>,
}

#[cfg(feature = "test-support")]
impl ProbeOwnState {
    #[must_use]
    pub fn new(service: Service, endpoint: Endpoint, body: Option<Value>) -> Self {
        Self {
            service,
            endpoint,
            body,
        }
    }
}

#[cfg(feature = "test-support")]
impl OwnState for ProbeOwnState {
    const KIND: &'static str = "test_probe";

    fn service(&self) -> Service {
        self.service
    }

    fn method(&self) -> Method {
        Method::Post
    }

    fn endpoint(&self) -> Endpoint {
        self.endpoint.clone()
    }

    fn body(&self) -> Option<Value> {
        self.body.clone()
    }
}
