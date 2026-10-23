// A lab account's fence from a config of the caller's making can't be built: `Fence::open_account` exists only with
// `test-support`; a lab binary's comes from `Fence::open_lab_account`, which reads `lab.toml` where the environment
// puts it.
use std::path::Path;

use teams_fence::Fence;
use teams_fence::lab::{Alias, LabConfig};

fn main() {
    let text = "[[accounts]]\nalias = \"a\"\nupn = \"a@lab.example.com\"\nmri = \"8:orgid:anyone\"\n";
    let config = LabConfig::parse(text, Path::new("/tmp/lab")).unwrap();
    let _ = Fence::open_account(&config, Alias::A);
}
