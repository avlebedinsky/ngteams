// A fence's own account (the one account whose incoming calls it offers) comes only from `lab.toml`,
// through `Fence::open_lab_account`: `Fence::with_own_account`, which takes any MRI, exists only with `test-support`.
use teams_fence::{Fence, Mri};

fn main() {
    let fence = Fence::open_lab().unwrap();
    let _ = fence.with_own_account(Mri::new("8:orgid:anyone"));
}
