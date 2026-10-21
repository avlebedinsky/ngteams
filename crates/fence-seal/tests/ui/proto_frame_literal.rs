// The reviewed Trouter frames are built only by `teams-proto`'s constructors, each of which gives exactly its reviewed
// shape: no other crate can build one from fields of its choosing (here an empty message-loss acknowledgement, which
// `MessageLossAck::new` refuses).
use teams_proto::{Disconnect, MessageLossAck, Ping};

fn main() {
    let _ = Ping { _sealed: () };
    let _ = Disconnect { _sealed: () };
    let _ = MessageLossAck { indicators: Vec::new() };
}
