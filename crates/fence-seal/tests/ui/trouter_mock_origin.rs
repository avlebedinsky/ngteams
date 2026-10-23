// The push channel connects only to its allowlisted Trouter hosts over WSS: a client pointed at an origin of the
// caller's choosing (`with_mock_origin`, plain `ws` to a local mock) exists only with `test-support`.
use std::sync::Arc;

use teams_proto::EndpointRegistrar;
use teams_trouter::{Trouter, TrouterCredentials};

fn mock<T: EndpointRegistrar + Send + 'static, K: TrouterCredentials + 'static>(
    transport: Arc<T>,
    credentials: Arc<K>,
) {
    let _ = Trouter::with_mock_origin(transport, credentials, "ws://127.0.0.1:9");
}

fn main() {}
