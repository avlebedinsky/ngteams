use std::sync::Arc;

use teams_proto::{Credentials, HttpTransport};

fn mock<C: Credentials>(credentials: Arc<C>) {
    let _ = HttpTransport::with_mock_origin(
        credentials,
        "https://teams.microsoft.com/api/chatsvc/amer",
        "http://127.0.0.1:9",
    );
}

fn main() {}
