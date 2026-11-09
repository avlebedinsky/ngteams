//! Bounded HTTP transfer with a persistent prefix; only a matching range response may append to it.

use std::fs::{File, OpenOptions};
use std::io::Seek as _;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use crate::cache::{Pins, Progress, located, space_error};
use crate::{Error, error};
use sha2::{Digest as _, Sha256};

pub(crate) fn download(pins: &Pins, path: &Path, progress: &mut impl FnMut(Progress)) -> Result<(), Error> {
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(path)
        .map_err(|e| located(e, path))?;
    let mut received = file.metadata()?.len();
    if received > pins.size {
        file.set_len(0)?;
        received = 0;
    }
    progress(Progress {
        received,
        total: pins.size,
    });
    if received == pins.size {
        return Ok(());
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        // as the app's transport: nothing but HTTPS (the tests' fake server aside), no proxy. Redirects are followed: a
        // release asset is served from another host, and only the pinned hash is trusted.
        .https_only(!cfg!(test))
        .proxy(None)
        .timeout_global(None)
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_per_read(Some(Duration::from_secs(30)))
        .build()
        .into();
    let mut request = agent
        .get(&pins.url)
        .header("Accept-Encoding", "identity")
        .header("User-Agent", concat!("ngteams/", env!("CARGO_PKG_VERSION")));
    if received > 0 {
        request = request.header("Range", format!("bytes={received}-"));
    }
    let mut response = match request.call() {
        Err(ureq::Error::StatusCode(416)) => {
            file.set_len(0)?;
            return Err(error(
                "The web engine server rejected the resume range; retry to download from zero.",
            ));
        }
        result => result?,
    };
    match response.status().as_u16() {
        200 => {
            file.set_len(0)?;
            received = 0;
        }
        206 if received > 0
            && response
                .headers()
                .get("Content-Range")
                .and_then(|value| value.to_str().ok())
                == Some(format!("bytes {received}-{}/{}", pins.size - 1, pins.size).as_str()) => {}
        _ => {
            file.set_len(0)?;
            return Err(error(
                "The web engine server returned an invalid range response; retry to download from zero.",
            ));
        }
    }
    let mut reader = response.body_mut().with_config().limit(pins.size + 1).reader();
    copy_body(&mut reader, &mut file, pins.size, received, progress)?;
    file.sync_all().map_err(|e| space_error(e.into(), pins.size))?;
    Ok(())
}

pub(crate) fn copy_body(
    reader: &mut impl Read,
    writer: &mut impl Write,
    total: u64,
    mut received: u64,
    progress: &mut impl FnMut(Progress),
) -> Result<(), Error> {
    let mut buffer = vec![0; 64 * 1024];
    loop {
        // Read at most one byte past the pin. Never write that byte to disk.
        let capacity = usize::try_from((total - received + 1).min(buffer.len() as u64))
            .map_err(|_| error("Invalid web engine size."))?;
        let count = reader.read(&mut buffer[..capacity])?;
        if count == 0 {
            break;
        }
        if count as u64 > total - received {
            return Err(error("The web engine download exceeds its pinned size."));
        }
        writer
            .write_all(&buffer[..count])
            .map_err(|e| space_error(e.into(), total - received))?;
        received += count as u64;
        progress(Progress { received, total });
    }
    if received != total {
        return Err(error(format!(
            "The web engine download is truncated ({received} of {total} bytes)."
        )));
    }
    Ok(())
}

pub(crate) fn verify(pins: &Pins, file: &mut File) -> Result<(), Error> {
    file.rewind()?;
    if file.metadata()?.len() != pins.size {
        return Err(error("The web engine archive does not match its pinned size."));
    }
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    let matches = hash
        .finalize()
        .iter()
        .enumerate()
        .all(|(index, byte)| u8::from_str_radix(&pins.sha256[index * 2..index * 2 + 2], 16).ok() == Some(*byte));
    if !matches {
        // Caller removes only its download cache, never a user's offline archive.
        return Err(error("The web engine archive failed SHA-256 verification."));
    }
    Ok(())
}
