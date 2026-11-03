// `OwnState` is sealed: the reviewed own-state list is the set of `teams-proto`'s own types, so no other crate can
// declare an own-state entry with arbitrary wire data.
use serde_json::Value;
use teams_proto::{Endpoint, Method, OwnState, Service};

struct Elsewhere;

impl OwnState for Elsewhere {
    const KIND: &'static str = "elsewhere";

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

fn main() {}
