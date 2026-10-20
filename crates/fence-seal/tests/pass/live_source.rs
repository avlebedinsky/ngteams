// Without `test-support`, a lab binary that writes can build the live source: `LiveMembershipSource` implements the
// sealed `MembershipSource`, fits the `Arc<dyn MembershipSource + Send>` the browser fence takes, takes a credential
// only as a `ChatCredential` parsed from a chat service request, and without one answers `Partial(NoCredential)`
// before reading anything.
use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use teams_fence::{ChatCredential, LiveMembershipSource, Membership, MembershipSource, PartialReason, Scope};

fn main() {
    let live = Arc::new(LiveMembershipSource::new().unwrap());
    let members: Arc<dyn MembershipSource + Send> = live.clone();
    let scope = Scope::Conversation {
        id: "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces".to_owned(),
    };
    let mut fetch = pin!(members.members(&scope));
    let answer = fetch.as_mut().poll(&mut Context::from_waker(Waker::noop()));
    assert_eq!(answer, Poll::Ready(Membership::Partial(PartialReason::NoCredential)));

    let token = "skypetoken=aaaaaaaaaa.bbbbbbbbbb.cccccccccc";
    let url = "https://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/conversations";
    let credential = ChatCredential::from_request(url, [("Authentication", token)]).unwrap();
    live.set_credential(credential);
    assert!(live.has_credential());
    assert!(!teams_fence::test_support_enabled(), "this case must build teams-fence without test-support");
}
