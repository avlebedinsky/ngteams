// A `WritePermit` has no public constructor: only `Fence::check` issues one.
use teams_fence::{Scope, Target, WriteKind, WritePermit};

fn main() {
    let target = Target::new(WriteKind::Message, Scope::Conversation { id: "19:lab-chat@thread.v2".to_owned() });
    let _ = WritePermit::new(target);
}
