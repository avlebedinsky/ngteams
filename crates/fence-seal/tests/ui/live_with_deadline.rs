// A live source's read takes at most `FETCH_TIMEOUT`: a source with a deadline of the caller's choosing
// (`with_deadline`) exists only with `test-support`.
use std::time::Duration;

use teams_fence::LiveMembershipSource;

fn main() {
    let _ = LiveMembershipSource::new().unwrap().with_deadline(Duration::from_secs(1));
}
