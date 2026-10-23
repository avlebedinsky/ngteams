// A transport sends a `Read` (always a `GET`), a plain `Request` (which it hands to the fence's classifier first) or an
// `Authorized` write, nothing else: a type that is only a `Read`, here a post shaped as a read, doesn't compile as a
// plain request or as a write, and neither does the test-only own-state probe without `test-support`.
use teams_proto::{Endpoint, Read, Service, Transport};

struct RawPost;

impl Read for RawPost {
    fn service(&self) -> Service {
        Service::ChatService
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["v1", "users", "ME", "conversations", "19:lab-chat@thread.v2", "messages"])
    }
}

fn post<T: Transport>(transport: &T) {
    let _ = transport.send(&RawPost);
    let _ = transport.deliver(RawPost);
    let _ = teams_proto::ProbeOwnState::new(Service::ChatService, Endpoint::new(["v1"]), None);
}

fn main() {}
