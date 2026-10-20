// The native transport sends only to its allowlisted hosts: a transport pointed at an origin of the caller's choosing
// (`with_mock_origin`, plain `http` to a local mock) exists only with `test-support`.
use std::sync::Arc;

use teams_fence::Fence;
use teams_proto::{Credentials, HttpTransport};

fn mock<C: Credentials>(credentials: Arc<C>, fence: Arc<Fence>) {
    let base = "https://teams.microsoft.com/api/chatsvc/amer";
    let _ = HttpTransport::with_mock_origin(credentials, fence, base, "http://127.0.0.1:9");
}

fn main() {}
