// A `WritePermit` can't be built outside `teams-fence`: its field is private.
use teams_fence::{Scope, Target, WriteKind, WritePermit};

fn main() {
    let target = Target::new(WriteKind::Message, Scope::Conversation { id: "19:lab-chat@thread.v2".to_owned() });
    let _ = WritePermit { target };
}
