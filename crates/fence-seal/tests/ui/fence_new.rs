// A fence over a roster and log of the caller's choosing can't be built: `Fence::new` exists only with `test-support`.
use std::path::Path;

use teams_fence::{Fence, FenceLog, Roster};

fn main() {
    let log = FenceLog::open(Path::new("/dev/null")).unwrap();
    let _ = Fence::new(Roster::default(), log);
}
