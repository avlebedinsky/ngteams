// An `Authorized` write can't be assembled by hand, bypassing the check that the permit's target is the request's.
use teams_fence::WritePermit;
use teams_proto::{Authorized, SendMessage};

fn pair(request: SendMessage, permit: WritePermit) -> Authorized<SendMessage> {
    Authorized { request, permit }
}

fn main() {}
