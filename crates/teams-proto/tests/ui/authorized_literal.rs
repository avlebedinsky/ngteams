// An `Authorized` write can't be assembled by hand, bypassing the check that the permit's target is the request's.
use teams_fence::WritePermit;
use teams_proto::{Authorized, SendMessage};

fn pair(permit: WritePermit) -> Authorized<SendMessage> {
    Authorized { request: SendMessage::new("19:other-chat@thread.v2", "hello"), permit }
}

fn main() {}
