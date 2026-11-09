//! Private pipe protocol. No page address or redirect is placed on the command line or in a log.
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, Read as _, Write};

const LIMIT: u64 = 1024 * 1024;
pub(super) fn identity() -> String {
    format!(
        "{}:webview2-v1:{}:test-support={}",
        env!("CARGO_PKG_VERSION"),
        option_env!("NGTEAMS_BUILD_COMMIT").unwrap_or(""),
        cfg!(feature = "test-support")
    )
}

#[derive(Serialize, Deserialize)]
pub(super) struct Start {
    pub identity: String,
    pub authorize: String,
    pub proxy: String,
    pub directory: std::path::PathBuf,
}

#[derive(Serialize, Deserialize)]
pub(super) enum Event {
    Request { method: String, url: String },
    Navigation(String),
    NetworkFailure(String),
    LoadFailed(i32),
    Closed,
    Failed(String),
}

pub(super) fn send(writer: &mut impl Write, value: &impl Serialize) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, value).map_err(io::Error::other)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

pub(super) fn read<T: for<'de> Deserialize<'de>>(reader: &mut impl BufRead) -> io::Result<T> {
    let mut bytes = Vec::new();
    reader.take(LIMIT + 1).read_until(b'\n', &mut bytes)?;
    if bytes.len() as u64 > LIMIT || bytes.pop() != Some(b'\n') {
        return Err(io::Error::other("invalid sign-in worker protocol"));
    }
    serde_json::from_slice(&bytes).map_err(|_| io::Error::other("invalid sign-in worker message"))
}
