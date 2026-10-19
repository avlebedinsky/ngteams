// A mutating call without a `WritePermit` doesn't compile: `Client::write` takes the permit as an argument.
use teams_proto::{Client, SendMessage, Transport};

fn send<T: Transport>(client: &Client<T>) {
    let _ = client.write(SendMessage::new("19:lab-chat@thread.v2", "hello"));
}

fn main() {}
