// An `AuthorizedPostRead` can't be built outside `teams-proto` but by `AuthorizedPostRead::new`, which checks the
// permit: its fields are private.
use teams_fence::{Mri, PostReadPermit};
use teams_proto::{AuthorizedPostRead, ShortProfileLookup};

fn pair(permit: PostReadPermit) {
    let request = ShortProfileLookup::new([Mri::new("8:orgid:lab-a")]).unwrap();
    let _ = AuthorizedPostRead { request, permit };
}

fn main() {}
