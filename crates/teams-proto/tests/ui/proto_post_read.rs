// `PostRead` is sealed: the reviewed read-by-`POST` list is the set of `teams-proto`'s own types, so no other crate can
// declare a request a read by `POST` with a body of its own choosing.
use teams_proto::Mri;
use teams_proto::PostRead;

struct Search {
    names: Vec<Mri>,
}

impl PostRead for Search {
    const KIND: &'static str = "short_profile";

    fn names(&self) -> &[Mri] {
        &self.names
    }
}

fn main() {}
