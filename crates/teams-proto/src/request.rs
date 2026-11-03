//! Typed requests: service, method, endpoint and body, sent through the logged transport.

use serde_json::Value;

use crate::endpoint::{Endpoint, Method, Service};

/// A plain request (see the module docs): its service, method, endpoint under the service's base, and JSON body.
pub trait Request {
    fn service(&self) -> Service;
    fn method(&self) -> Method;
    fn endpoint(&self) -> Endpoint;
    /// The JSON body, if it has one.
    fn body(&self) -> Option<Value>;
    /// Additional request headers; credentials are provided by the transport.
    fn headers(&self) -> Vec<(String, String)> {
        Vec::new()
    }
    /// A byte body, when the request sends an image instead of JSON.
    fn raw_body(&self) -> Option<crate::RawBody> {
        None
    }
    /// A calling-service link, checked against that service's base by the transport.
    fn call_url(&self) -> Option<&url::Url> {
        None
    }
    /// Whether this typed request has a wire representation.
    fn routable(&self) -> bool {
        true
    }
    /// A write gets one send and returns the service's status to its caller.
    fn is_write(&self) -> bool {
        false
    }
}
