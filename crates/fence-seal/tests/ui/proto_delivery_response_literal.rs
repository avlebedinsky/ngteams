// None of `teams-proto`'s call delivery responses can be built from fields of the caller's choosing (an
// acknowledgement for a delivery it doesn't answer, a receipt with another request id): their fields are private. Nor
// can a response the fence permitted, or what decides one for a connection, be made without the fence's decision.
// Every field gets a value of its type (`any()` for the answered delivery, whose type can't be named), so the literals
// type-check and only the privacy check refuses them; nothing else in this file may fail, since rustc checks a struct
// literal's field privacy only once the crate has no other error.
use std::marker::PhantomData;

use teams_proto::{AcceptanceAcknowledgement, InvitationReceipt, Links, PermittedResponse, ResponseDecider};

fn any<T>() -> T {
    unimplemented!()
}

fn main() {
    let _ = InvitationReceipt { answered: any(), client_delay: 0 };
    let _ = AcceptanceAcknowledgement {
        answered: any(),
        chain_id: String::new(),
        message_id: String::new(),
        correlation: String::new(),
        client_delay: 0,
        links: Links::new(),
    };
    let _ = PermittedResponse::<InvitationReceipt> {
        json: String::new(),
        fence: any(),
        connection: String::new(),
        kind: PhantomData,
    };
    let _ = ResponseDecider { fence: any(), connection: String::new() };
}
