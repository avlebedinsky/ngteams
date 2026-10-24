// A read by `POST` of the personal build's sealed list reaches the native transport's `post_read` only inside an
// `AuthorizedPostRead`, which only `AuthorizedPostRead::new` makes, from a permit: a lookup handed over bare doesn't
// compile, and the test-only probe doesn't exist without `test-support` (the pairing's literal: the next case).
use teams_fence::Mri;
use teams_proto::{Credentials, HttpTransport, ShortProfileLookup};

fn send<C: Credentials>(transport: &HttpTransport<C>) {
    let lookup = ShortProfileLookup::new([Mri::new("8:orgid:lab-a")]).unwrap();
    let _ = transport.post_read(lookup.clone());
    let _ = teams_proto::ProbePostRead::new(lookup);
}

fn main() {}
