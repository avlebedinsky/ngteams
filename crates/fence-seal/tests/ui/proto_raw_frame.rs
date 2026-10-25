// A Trouter socket's sink sends only `teams-proto`'s typed frames: raw text, frames it has no type for (no
// `user.activity` yet) and an answer to a delivery built by hand don't compile, and neither does a frame's text made
// outside the sink (a frame built from fields of the caller's choosing is in `proto_frame_literal`). A typed call
// delivery response is no frame either: it goes only through `FrameSink::respond` (or `FrameSink::send_permitted`),
// under the fence's decision
// (`proto_delivery_response`).
use teams_proto::{AcceptanceAcknowledgement, FrameSink, FrameText, FrameWriter, InvitationReceipt};

/// An answer to a delivery (`3:::{"id":…,"status":200,…}`), built by hand.
struct DeliveryAnswer {
    id: u64,
}

async fn send<W: FrameWriter>(
    sink: &mut FrameSink<W>,
    writer: &mut W,
    acknowledgement: AcceptanceAcknowledgement,
    receipt: InvitationReceipt,
) {
    let _ = sink.send(&"5:2+::{\"name\":\"user.activity\",\"args\":[{\"state\":\"active\",\"cv\":\"x\"}]}").await;
    let _ = sink.send(&teams_proto::UserActivity::new()).await;
    let _ = sink.send(&DeliveryAnswer { id: 7 }).await;
    let _ = sink.send(&acknowledgement).await;
    let _ = sink.send(&receipt).await;
    let _ = writer.write(FrameText(String::from("3:::{\"id\":7,\"status\":200}"))).await;
}

fn main() {}
