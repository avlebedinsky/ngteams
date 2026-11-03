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
