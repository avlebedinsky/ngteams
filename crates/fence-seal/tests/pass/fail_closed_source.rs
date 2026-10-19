// Without `test-support`, a lab binary still has a membership source to arm the browser fence with: `FailClosed`
// implements the sealed `MembershipSource`, fits the `Arc<dyn MembershipSource + Send>` the browser fence takes, can
// be handed to `Fence::check`, and answers every scope with `Partial(NoSource)`.
use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use teams_fence::{FailClosed, Fence, Membership, MembershipSource, PartialReason, Scope, Target, WriteKind};

async fn check(fence: &Fence, members: &(dyn MembershipSource + Send)) {
    let target = Target::new(WriteKind::Message, Scope::Conversation { id: "19:any-chat@thread.v2".to_owned() });
    let _ = fence.check(target, members).await;
}

fn main() {
    let _ = check;
    let members: Arc<dyn MembershipSource + Send> = Arc::new(FailClosed);
    let scope = Scope::Conversation { id: "19:any-chat@thread.v2".to_owned() };
    let mut fetch = pin!(members.members(&scope));
    let answer = fetch.as_mut().poll(&mut Context::from_waker(Waker::noop()));
    assert_eq!(answer, Poll::Ready(Membership::Partial(PartialReason::NoSource)));
    assert!(!teams_fence::test_support_enabled(), "this case must build teams-fence without test-support");
}
