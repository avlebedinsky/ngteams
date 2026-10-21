// A `PostReadPermit` can't be built outside `teams-fence`: its fields are private.
use teams_fence::{Mri, PostReadPermit};

fn main() {
    let _ = PostReadPermit { kind: "short_profile", names: vec![Mri::new("8:orgid:lab-a")] };
}
