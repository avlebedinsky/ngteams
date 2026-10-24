// The endpoint the account's own availability names (M2 Task 19) is a `RegisteredEndpoint`, which only
// `teams_proto::register` gives, once a registration the account's transport sent was taken, recorded for the account
// it was registered for: no other crate can write one out by hand, so no other id (or account) can be named.
use teams_fence::Mri;
use teams_proto::RegisteredEndpoint;

fn main() {
    let _ = RegisteredEndpoint {
        id: "00000000-0000-4000-8000-000000000001".to_owned(),
        account: Some(Mri::new("8:orgid:anyone")),
    };
}
