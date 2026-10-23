// The endpoint the account's own availability names (M2 Task 19) is a `RegisteredEndpoint`, which only
// `teams_proto::register` gives, once a registration the account's transport sent was taken: no other crate can write
// one out by hand, so no other id can be named.
use teams_proto::RegisteredEndpoint;

fn main() {
    let _ = RegisteredEndpoint {
        id: "00000000-0000-4000-8000-000000000001".to_owned(),
    };
}
