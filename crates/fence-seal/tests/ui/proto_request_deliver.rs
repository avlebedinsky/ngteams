// A plain `Request` is open to any crate, so it never stands in for a write: the transport's `deliver` takes only an
// `Authorized` write, and `Authorized::new` only a sealed `Mutating` type of `teams-proto`'s, with the permit for its
// target. A message post written as a plain request goes through `send`, where the fence's classifier refuses it.
use serde_json::Value;
use teams_fence::WritePermit;
use teams_proto::{Authorized, Endpoint, Method, Request, Service, Transport};

struct PostAsRequest;

impl Request for PostAsRequest {
    fn service(&self) -> Service {
        Service::ChatService
    }

    fn method(&self) -> Method {
        Method::Post
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["v1", "users", "ME", "conversations", "19:lab-chat@thread.v2", "messages"])
    }

    fn body(&self) -> Option<Value> {
        None
    }
}

fn deliver<T: Transport>(transport: &T, permit: WritePermit) {
    let _ = transport.deliver(PostAsRequest);
    let _ = Authorized::new(PostAsRequest, permit);
}

fn main() {}
