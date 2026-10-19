// A fence from a config of the caller's making can't be built: `Fence::open` exists only with `test-support`, and a lab
// binary's fence comes from `Fence::open_lab`, which reads `lab.toml` where the environment puts it.
use std::path::Path;

use teams_fence::Fence;
use teams_fence::lab::LabConfig;

fn main() {
    let text = "[[accounts]]\nalias = \"a\"\nupn = \"a@lab.example.com\"\nmri = \"8:orgid:anyone\"\n";
    let config = LabConfig::parse(text, Path::new("/tmp/lab")).unwrap();
    let _ = Fence::open(&config);
}
