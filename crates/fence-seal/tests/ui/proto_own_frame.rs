// `OwnFrame` is sealed: the reviewed list of Trouter frames is the set of `teams-proto`'s own types, so no other crate
// can add one to it (here a `user.activity` declared an own frame). That none of the list's frames can be built
// outside `teams-proto` is in `proto_frame_literal`.
use teams_proto::OwnFrame;

struct Activity;

impl OwnFrame for Activity {
    const KIND: &'static str = "trouter_activity";
}

fn main() {}
