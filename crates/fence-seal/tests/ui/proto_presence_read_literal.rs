// A `PresenceRead` (UPS's presence read, an entry of the sealed read-by-`POST` list, M2 Task 17) is made only by
// `PresenceRead::new`, which names whom it asks about; no other crate can write one out by hand. (An
// `AuthorizedPostRead` can't be written out by hand either: `proto_post_read_literal`.)
use teams_fence::Mri;
use teams_proto::PresenceRead;

fn main() {
    let _ = PresenceRead {
        names: vec![Mri::new("8:orgid:someone")],
    };
}
