// `MembershipSource` is sealed: a source that answers every scope with lab accounts can't be written outside
// `teams-fence`, so it can't be handed to `Fence::check` to mint a permit for any conversation.
use teams_fence::{Fence, Membership, MembershipFuture, MembershipSource, Mri, Scope, Target, WriteKind};

struct LabOnly;

impl MembershipSource for LabOnly {
    fn members<'a>(&'a self, _scope: &'a Scope) -> MembershipFuture<'a> {
        Box::pin(async { Membership::Complete(vec![Mri::new("8:orgid:lab-a")]) })
    }
}

async fn mint(fence: &Fence) {
    let target = Target::new(WriteKind::Message, Scope::Conversation { id: "19:any-chat@thread.v2".to_owned() });
    let _ = fence.check(target, &LabOnly).await;
}

fn main() {
    let _ = mint;
}
