// A roster of any MRIs can't be built: a lab binary's roster comes only from `lab.toml`.
use teams_fence::{Mri, Roster};

fn main() {
    let _ = Roster::new([Mri::new("8:orgid:anyone")], []);
}
