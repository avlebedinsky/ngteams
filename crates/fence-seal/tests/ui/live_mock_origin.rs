// The live source reads from the chat service hosts it allows and nowhere else: a source pointed at an origin of the
// caller's choosing (`with_mock_origin`, a local mock) exists only with `test-support`.
use teams_fence::LiveMembershipSource;

fn main() {
    let _ = LiveMembershipSource::with_mock_origin("http://127.0.0.1:9");
}
