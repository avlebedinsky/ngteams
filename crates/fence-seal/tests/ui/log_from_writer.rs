// A log that throws its entries away can't be built: a lab binary's log is the lab's `fence.log`.
use std::io;

use teams_fence::FenceLog;

fn main() {
    let _ = FenceLog::from_writer(io::sink());
}
