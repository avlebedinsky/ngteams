// A Read-only type is not a Request and cannot be passed to the plain-request/write APIs.
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
    let _ = transport.deliver(&RawPost);
}

fn main() {}
