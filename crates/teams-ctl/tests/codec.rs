//! JSON lines: a line per value, the size cap both ways, and a line's start kept across a read that timed out.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::collections::VecDeque;
use std::io::{self, BufReader, Cursor, Read};

use teams_ctl::codec::{CodecError, LineReader, MAX_LINE, encode, write_line};
use teams_ctl::protocol::{Outcome, Request, RequestLine, Response, ResponseLine};

fn reader(text: &str) -> LineReader<Cursor<Vec<u8>>> {
    LineReader::new(Cursor::new(text.as_bytes().to_vec()))
}

#[test]
fn lines_are_read_one_at_a_time_until_the_end() {
    let mut lines = reader("{\"a\":1}\n\n{\"b\":2}\n");
    assert_eq!(lines.read_line().unwrap().as_deref(), Some("{\"a\":1}"));
    assert_eq!(lines.read_line().unwrap().as_deref(), Some(""));
    assert_eq!(lines.read_line().unwrap().as_deref(), Some("{\"b\":2}"));
    assert_eq!(lines.read_line().unwrap(), None);
    assert_eq!(lines.read_line().unwrap(), None);
}

#[test]
fn a_line_split_over_small_reads_comes_whole() {
    let text = b"{\"id\":1,\"ok\":{\"kind\":\"done\"}}\n".to_vec();
    let mut lines = LineReader::new(BufReader::with_capacity(3, Cursor::new(text)));
    let line: ResponseLine = lines.read().unwrap().unwrap();
    assert_eq!(line.outcome, Outcome::Ok(Response::Done));
    assert!(lines.read::<ResponseLine>().unwrap().is_none());
}

#[test]
fn a_stream_ending_inside_a_line_is_truncated() {
    let mut lines = reader("{\"a\":1}\n{\"b\"");
    assert!(lines.read_line().unwrap().is_some());
    assert!(matches!(lines.read_line(), Err(CodecError::Truncated)));
    assert_eq!(lines.read_line().unwrap(), None);
}

#[test]
fn a_line_over_the_cap_is_refused_as_it_is_read() {
    let mut lines = LineReader::with_limit(Cursor::new(b"12345678\n".to_vec()), 8);
    assert_eq!(lines.read_line().unwrap().as_deref(), Some("12345678"));

    let mut lines = LineReader::with_limit(BufReader::with_capacity(4, Cursor::new(b"123456789\n".to_vec())), 8);
    assert!(matches!(lines.read_line(), Err(CodecError::TooLong { limit: 8 })));
}

#[test]
fn a_line_that_is_not_utf8_or_not_the_json_expected_is_an_error() {
    let mut lines = LineReader::new(Cursor::new(b"\xff\xfe\n".to_vec()));
    assert!(matches!(lines.read_line(), Err(CodecError::Utf8)));

    let mut lines = reader("{\"id\":1}\n");
    assert!(matches!(lines.read::<ResponseLine>(), Err(CodecError::Json(_))));
}

/// Hands out its chunks one read at a time, with a timeout (`WouldBlock`) or an interruption between them.
struct Stutter {
    chunks: VecDeque<io::Result<Vec<u8>>>,
}

impl Read for Stutter {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.chunks.pop_front() {
            None => Ok(0),
            Some(Err(error)) => Err(error),
            Some(Ok(chunk)) => {
                buf[..chunk.len()].copy_from_slice(&chunk);
                Ok(chunk.len())
            }
        }
    }
}

#[test]
fn a_read_that_timed_out_keeps_the_start_of_its_line() {
    let chunks = VecDeque::from([
        Ok(b"{\"id\":4,".to_vec()),
        Err(io::Error::from(io::ErrorKind::WouldBlock)),
        Err(io::Error::from(io::ErrorKind::Interrupted)),
        Ok(b"\"cmd\":\"status\"}\n".to_vec()),
    ]);
    let mut lines = LineReader::new(BufReader::new(Stutter { chunks }));
    match lines.read_line() {
        Err(CodecError::Io(error)) => assert_eq!(error.kind(), io::ErrorKind::WouldBlock),
        other => panic!("{other:?}"),
    }
    let line: RequestLine = lines.read().unwrap().unwrap();
    assert_eq!(line.id, 4);
    assert_eq!(line.request, Request::Status);
    assert!(lines.get_ref().buffer().is_empty());
}

#[test]
fn a_value_is_written_as_one_line() {
    let line = RequestLine {
        id: 1,
        account: None,
        request: Request::Send {
            chat: "19:x@thread.v2".to_owned(),
            text: "one\ntwo".to_owned(),
        },
    };
    let mut out = Vec::new();
    write_line(&mut out, &line).unwrap();
    assert_eq!(out.iter().position(|&byte| byte == b'\n'), Some(out.len() - 1));
    assert_eq!(out.last(), Some(&b'\n'));
    let back: RequestLine = LineReader::new(Cursor::new(out)).read().unwrap().unwrap();
    assert_eq!(back, line);
}

#[test]
fn a_value_over_the_cap_is_not_written() {
    let text = "x".repeat(MAX_LINE);
    assert!(matches!(encode(&text), Err(CodecError::TooLong { limit: MAX_LINE })));
    let mut out = Vec::new();
    assert!(write_line(&mut out, &text).is_err());
    assert!(out.is_empty());
    // just under the cap: two quotes around it
    assert!(encode(&"x".repeat(MAX_LINE - 2)).is_ok());
}

/// A writer whose writes fail.
struct Broken;

impl io::Write for Broken {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::from(io::ErrorKind::BrokenPipe))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_failed_write_is_an_io_error() {
    assert!(matches!(write_line(&mut Broken, &1), Err(CodecError::Io(_))));
    assert!(io::Write::flush(&mut Broken).is_ok());
}

#[test]
fn an_unserializable_value_is_a_json_error() {
    use std::collections::HashMap;
    // a map keyed by something that isn't a string doesn't make JSON
    let map = HashMap::from([((1, 2), 3)]);
    assert!(matches!(encode(&map), Err(CodecError::Json(_))));
}

#[test]
fn errors_say_what_went_wrong() {
    assert_eq!(CodecError::TooLong { limit: 8 }.to_string(), "a line is over 8 bytes");
    assert_eq!(CodecError::Truncated.to_string(), "the stream ended inside a line");
    assert_eq!(CodecError::Utf8.to_string(), "a line isn't UTF-8");
    let reader = LineReader::new(BufReader::new(Cursor::new(Vec::new())));
    assert!(format!("{reader:?}").contains("LineReader"));
}
