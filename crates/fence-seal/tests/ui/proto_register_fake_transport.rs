// A `RegisteredEndpoint` (the one endpoint the account's own availability names, M2 Task 19) comes only from a
// registration the native HTTP transport sent and the registrar took. `Transport` is open, so a transport of another
// crate's making could answer a registration "taken" without sending anything: `register` takes only an
// `EndpointRegistrar`, which is sealed, and outside `test-support` only `HttpTransport` is one.
use std::io;

use teams_proto::{
    Authorized, AuthorizedPostRead, EndpointRegistration, Mutating, OwnState, PostRead, Read, Response, Transport,
    register,
};

struct Answering;

impl Transport for Answering {
    type Error = io::Error;

    async fn read<R: Read>(&self, _: &R) -> Result<Response, io::Error> {
        Ok(Response::new(200, Vec::new()))
    }

    async fn own_state<R: OwnState>(&self, _: &R) -> Result<Response, io::Error> {
        Ok(Response::new(202, Vec::new()))
    }

    async fn post_read<R: PostRead + Send>(&self, _: AuthorizedPostRead<R>) -> Result<Response, io::Error> {
        Ok(Response::new(200, Vec::new()))
    }

    async fn deliver<R: Mutating + Send>(&self, _: Authorized<R>) -> Result<Response, io::Error> {
        Ok(Response::new(200, Vec::new()))
    }
}

async fn forge() {
    let registration = EndpointRegistration::messaging(
        "00000000-0000-4000-8000-000000000001",
        "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint00000A/",
    )
    .unwrap();
    let _ = register(&Answering, &registration).await;
}

fn main() {
    let _ = forge;
}
