// The push channel connects only to its allowlisted Trouter hosts over WSS: a client pointed at an origin of the
// caller's choosing (`with_mock_origin`, plain `ws` to a local mock) exists only with `test-support`.
use std::sync::Arc;

use teams_fence::Fence;
use teams_proto::Transport;
use teams_trouter::{Trouter, TrouterCredentials};

fn mock<T: Transport + Send + 'static, K: TrouterCredentials + 'static>(
    transport: Arc<T>,
    credentials: Arc<K>,
    fence: Arc<Fence>,
) {
    let _ = Trouter::with_mock_origin(transport, credentials, fence, "ws://127.0.0.1:9");
}

fn main() {}
