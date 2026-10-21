// Without `test-support`, a native client's live member read takes its credential from a `CredentialSource` asked on
// every read (the native transport is one, over the account's session); a source that has no token gives no
// credential, and the read is `Partial(NoCredential)` before anything is sent.
use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use teams_fence::{
    CredentialFuture, CredentialSource, LiveMembershipSource, Membership, MembershipSource, PartialReason, Scope,
};
use teams_proto::{Credentials, HttpTransport};

struct SignedOut;

impl CredentialSource for SignedOut {
    fn credential(&self) -> CredentialFuture<'_> {
        Box::pin(async { None })
    }
}

/// The native transport is a credential source for the live read.
fn _native<C: Credentials + 'static>(transport: Arc<HttpTransport<C>>, live: &LiveMembershipSource) {
    live.set_credential_source(transport);
}

fn main() {
    let live = LiveMembershipSource::new().unwrap();
    live.set_credential_source(Arc::new(SignedOut));
    assert!(live.has_credential());
    let scope = Scope::Conversation {
        id: "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces".to_owned(),
    };
    let mut fetch = pin!(live.members(&scope));
    let answer = fetch.as_mut().poll(&mut Context::from_waker(Waker::noop()));
    assert_eq!(answer, Poll::Ready(Membership::Partial(PartialReason::NoCredential)));
    assert!(!teams_fence::test_support_enabled(), "this case must build teams-fence without test-support");
}
