// A `PostReadPermit` can't be made or copied outside `teams-fence`: it has no constructor and it isn't `Clone`, so the
// only way to one is `Fence::check_post_read`, on a fence only `teams-fence` can open (its literal: the next case).
use teams_fence::{Mri, PostReadPermit};

fn main() {
    let _ = PostReadPermit::new("short_profile", vec![Mri::new("8:orgid:lab-a")]);
}

fn twice(permit: &PostReadPermit) -> PostReadPermit {
    permit.clone()
}
