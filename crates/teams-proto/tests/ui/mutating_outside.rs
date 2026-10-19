// `Mutating` is sealed: a request type from another crate can't implement it, so it can't report one target to the
// permit check and write somewhere else.
use teams_fence::{Scope, Target, WriteKind};
use teams_proto::Mutating;

struct Elsewhere;

impl Mutating for Elsewhere {
    fn target(&self) -> Target {
        Target::new(WriteKind::Message, Scope::Conversation { id: "19:lab-chat@thread.v2".to_owned() })
    }
}

fn main() {}
