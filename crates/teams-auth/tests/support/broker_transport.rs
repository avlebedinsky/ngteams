//! Synthetic broker transports, with real Unix sockets or Windows named pipes.
#![cfg(test)]

use std::io;
use teams_auth::broker::BrokerStream;

pub type AsyncStream = teams_auth::broker::AsyncBrokerStream;

pub fn pair_sync() -> io::Result<(BrokerStream, BrokerStream)> {
    teams_auth::broker::private_pair()
}

pub fn pair() -> io::Result<(AsyncStream, BrokerStream)> {
    let (server, client) = pair_sync()?;
    Ok((teams_auth::broker::into_async(server)?, client))
}
