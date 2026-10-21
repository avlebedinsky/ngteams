// The live source knows the lab's group chat only from the fence it serves (`LiveMembershipSource::for_fence`, the
// policy `Fence::open_lab` read from `lab.toml`): a source given a lab config of the caller's choosing (`with_lab`), or
// a deadline of its choosing (`with_deadline`), exists only with `test-support`.
use std::time::Duration;

use teams_fence::LiveMembershipSource;
use teams_fence::lab::LabConfig;

fn main() {
    let config = LabConfig::parse("", std::path::Path::new("/lab")).unwrap();
    let source = LiveMembershipSource::new().unwrap();
    let _ = source.with_lab(&config);
    let _ = LiveMembershipSource::new().unwrap().with_deadline(Duration::from_secs(1));
}
