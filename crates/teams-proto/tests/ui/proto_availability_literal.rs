// The account's own availability is made only by `SetEndpointAvailability::available` (or `ForceAvailability::set` and
// `reset`), from a `RegisteredEndpoint`: no other crate can write one out by hand with an account or a status of its
// choosing (M2 Task 19).
use teams_proto::Mri;
use teams_proto::{ForceAvailability, RegisteredEndpoint, SetEndpointAvailability};

fn forge(endpoint: RegisteredEndpoint) {
    let _ = SetEndpointAvailability {
        account: Mri::new("8:orgid:anyone"),
        endpoint: endpoint.clone(),
    };
    let _ = ForceAvailability {
        account: Mri::new("8:orgid:anyone"),
        endpoint,
        status: None,
    };
}

fn main() {
    let _: fn(RegisteredEndpoint) = forge;
}
