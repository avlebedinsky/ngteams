// A mutating call without a `WritePermit` doesn't compile: `Client::write` takes the permit as an argument.
use teams_proto::{Client, SendMessage, Transport};

fn send<T: Transport>(client: &Client<T>, request: SendMessage) {
    let _ = client.write(request);
}

fn main() {}
