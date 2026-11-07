//! JSON lines: one compact JSON value per line, each line at most [`MAX_LINE`] bytes.
//!
//! Compact JSON escapes every line break inside a string, so a value never spans lines. A line over the cap is an
//! error for both ends: the writer refuses to send it, and the reader stops reading it (the connection is then
//! closed, its stream being mid-line).

use std::io::{self, BufRead, Write};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::protocol::{CtlError, ErrorCode, RequestLine};

/// The longest line either end sends or reads, in bytes, its line break left out.
pub const MAX_LINE: usize = 8 << 20;

/// Why a line couldn't be read or written.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("a line is over {limit} bytes")]
    TooLong { limit: usize },
    #[error("the stream ended inside a line")]
    Truncated,
    #[error("a line isn't UTF-8")]
    Utf8,
    #[error("a line isn't the JSON expected: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// A line that couldn't be read as a request: the id to answer it with (0 when it had none) and the error.
#[derive(Debug, PartialEq, Eq)]
pub struct BadLine {
    pub id: u64,
    pub error: CtlError,
}

/// Reads lines, keeping a line's start across a read that timed out, so the next read finishes it.
#[derive(Debug)]
pub struct LineReader<R> {
    inner: R,
    partial: Vec<u8>,
    limit: usize,
}

impl<R: BufRead> LineReader<R> {
    /// Lines of at most [`MAX_LINE`] bytes.
    pub fn new(inner: R) -> Self {
        Self::with_limit(inner, MAX_LINE)
    }

    /// Lines of at most `limit` bytes.
    pub fn with_limit(inner: R, limit: usize) -> Self {
        Self {
            inner,
            partial: Vec::new(),
            limit,
        }
    }

    /// The reader the lines come from.
    pub fn get_ref(&self) -> &R {
        &self.inner
    }

    /// The next line, without its line break; `None` at the end of the stream.
    ///
    /// # Errors
    /// [`CodecError::TooLong`] past the limit, [`CodecError::Truncated`] when the stream ends inside a line,
    /// [`CodecError::Utf8`], or the read's own error (a timeout keeps what was read of the line).
    pub fn read_line(&mut self) -> Result<Option<String>, CodecError> {
        loop {
            let available = match self.inner.fill_buf() {
                Ok(available) => available,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(CodecError::Io(error)),
            };
            if available.is_empty() {
                return if self.partial.is_empty() {
                    Ok(None)
                } else {
                    self.partial.clear();
                    Err(CodecError::Truncated)
                };
            }
            let end = available.iter().position(|&byte| byte == b'\n');
            let chunk = &available[..end.unwrap_or(available.len())];
            if self.partial.len() + chunk.len() > self.limit {
                self.partial.clear();
                return Err(CodecError::TooLong { limit: self.limit });
            }
            self.partial.extend_from_slice(chunk);
            let used = chunk.len() + usize::from(end.is_some());
            self.inner.consume(used);
            if end.is_some() {
                let line = std::mem::take(&mut self.partial);
                return String::from_utf8(line).map(Some).map_err(|_| CodecError::Utf8);
            }
        }
    }

    /// The next line read as `T`; `None` at the end of the stream.
    ///
    /// # Errors
    /// As [`LineReader::read_line`], or [`CodecError::Json`] when the line isn't a `T`.
    pub fn read<T: DeserializeOwned>(&mut self) -> Result<Option<T>, CodecError> {
        match self.read_line()? {
            Some(line) => Ok(Some(serde_json::from_str(&line)?)),
            None => Ok(None),
        }
    }
}

/// `value` as one line, its line break included.
///
/// # Errors
/// [`CodecError::Json`] when it doesn't serialize, [`CodecError::TooLong`] over [`MAX_LINE`].
pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, CodecError> {
    let mut line = serde_json::to_vec(value)?;
    if line.len() > MAX_LINE {
        return Err(CodecError::TooLong { limit: MAX_LINE });
    }
    line.push(b'\n');
    Ok(line)
}

/// Write `value` as one line and flush it.
///
/// # Errors
/// As [`encode`], or the write's own error.
pub fn write_line<W: Write, T: Serialize>(writer: &mut W, value: &T) -> Result<(), CodecError> {
    let line = encode(value)?;
    writer.write_all(&line)?;
    writer.flush()?;
    Ok(())
}

/// A line read as a request. One that doesn't read is answered [`ErrorCode::BadRequest`], with its id when it has one.
///
/// # Errors
/// A [`BadLine`]: not JSON, no id, an unknown `cmd`, or a field missing or of the wrong type.
pub fn decode_request(line: &str) -> Result<RequestLine, BadLine> {
    let value: serde_json::Value = serde_json::from_str(line).map_err(|error| BadLine {
        id: 0,
        error: CtlError::new(ErrorCode::BadRequest, format!("not JSON: {error}")),
    })?;
    let Some(id) = value.get("id").and_then(serde_json::Value::as_u64) else {
        return Err(BadLine {
            id: 0,
            error: CtlError::new(ErrorCode::BadRequest, "no numeric id"),
        });
    };
    serde_json::from_value(value).map_err(|error| BadLine {
        id,
        error: CtlError::new(ErrorCode::BadRequest, error.to_string()),
    })
}
