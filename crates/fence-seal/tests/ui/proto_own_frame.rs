// `OwnFrame` is sealed to keep frame text inside `teams-proto`: no other crate can declare a frame (here a
// `user.activity`; adding one to `teams-proto` is an ordinary change). That none of its frames can be built outside
// `teams-proto` is in `proto_frame_literal`.
use teams_proto::OwnFrame;

struct Activity;

impl OwnFrame for Activity {
    const KIND: &'static str = "trouter_activity";
}

fn main() {}
