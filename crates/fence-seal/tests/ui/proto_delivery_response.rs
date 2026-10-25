// A call delivery response is typed and sealed: no other crate declares one (here a response of its own text).
// `FrameSink::respond` sends one only once the fence permitted its exact JSON for the sink's connection. That none of
// `teams-proto`'s can be built from fields of the caller's choosing is `proto_delivery_response_literal.rs`.
use teams_proto::DeliveryResponse;

struct Forged;

impl DeliveryResponse for Forged {
    const KIND: &'static str = "call_forged";
}

fn main() {}
