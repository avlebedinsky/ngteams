// A personal fence over a log and an account of the caller's making can't be built either: `Fence::personal` exists only
// with both `test-support` and `personal`, for tests.
use teams_fence::{Fence, FenceLog, Mri};

fn main() {
    let log = FenceLog::open(std::path::Path::new("/tmp/fence.log")).unwrap();
    let _ = Fence::personal(Mri::new("8:orgid:anyone"), log);
}
