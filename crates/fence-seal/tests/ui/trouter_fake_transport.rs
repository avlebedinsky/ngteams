// The push channel hands on the endpoint its registration registered (`PushEvent::Registered`, M2 Task 19), so it
// registers only through an `EndpointRegistrar`: a push channel over a transport of another crate's making, whose
// answers could say a registration was taken without sending it, can't be built outside `test-support`.
use std::sync::Arc;

use teams_fence::Fence;
use teams_proto::Transport;
use teams_trouter::{Trouter, TrouterCredentials};

fn push<T: Transport + Send + 'static, K: TrouterCredentials + 'static>(
    transport: Arc<T>,
    credentials: Arc<K>,
    fence: Arc<Fence>,
) {
    let _ = Trouter::new(transport, credentials, fence);
}

fn main() {}
