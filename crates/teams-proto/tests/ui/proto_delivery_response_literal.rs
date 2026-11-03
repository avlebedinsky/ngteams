// Typed call responses keep their answered delivery and envelope fields private.
use teams_proto::{AcceptanceAcknowledgement, InvitationReceipt, Links};

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
}
