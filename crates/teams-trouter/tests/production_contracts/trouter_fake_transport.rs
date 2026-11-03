use std::sync::Arc;

use teams_proto::Transport;
use teams_trouter::{Trouter, TrouterCredentials};

fn push<T: Transport + Send + 'static, K: TrouterCredentials + 'static>(
    transport: Arc<T>,
    credentials: Arc<K>,
) {
    let _ = Trouter::new(transport, credentials);
}

fn main() {}
