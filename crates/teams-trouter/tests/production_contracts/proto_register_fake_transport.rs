use std::future::{self, Future};
use std::io;

use teams_proto::{EndpointRegistration, Read, Request, Response, Transport, register};

struct Answering;

impl Transport for Answering {
    type Error = io::Error;

    fn read<R: Read>(&self, _: &R) -> impl Future<Output = Result<Response, io::Error>> + Send {
        future::ready(Ok(Response::new(200, Vec::new())))
    }

    fn send<R: Request>(&self, _: &R) -> impl Future<Output = Result<Response, io::Error>> + Send {
        future::ready(Ok(Response::new(202, Vec::new())))
    }

    fn deliver<R: Request + Send>(&self, _: R) -> impl Future<Output = Result<Response, io::Error>> + Send {
        future::ready(Ok(Response::new(200, Vec::new())))
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
