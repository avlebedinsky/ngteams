//! The task-dispatch skill's live status page (`.claude/skills/task-dispatch/SKILL.md`): one card per task of the
//! dispatcher's ledger, with what its agent is doing now, its phase and an estimate of what is left, served on
//! `127.0.0.1` only. The page polls `/status.json` every 2 s.
//!
//! ```text
//! dispatch-status --ledger <path> [--port 47231] [--subagents <dir>] [--exit-with <pid>]
//! ```
//!
//! Agents are found by name in `--subagents` (by default `~/.claude/projects/*/$CLAUDE_CODE_SESSION_ID/subagents`, looked
//! for on every poll until Claude Code makes it with the session's first agent)
//! unless the ledger gives a task's `transcript`. The server exits once the process `--exit-with` names (by default
//! `$CLAUDE_PID`, the Claude Code session that started it) is gone.

mod describe;
mod ledger;
mod phase;
mod redact;
mod signals;
mod status;
mod time;
mod transcript;

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use crate::status::{Agents, Status};

/// The page itself.
const PAGE: &str = include_str!("../assets/index.html");

/// The port the skill opens the page on.
const DEFAULT_PORT: u16 = 47231;

/// How long one request may take to arrive or be written.
const IO_TIMEOUT: Duration = Duration::from_secs(2);

/// The longest request head read.
const HEAD_MAX: usize = 16 * 1024;

struct Options {
    ledger: PathBuf,
    port: u16,
    agents: Agents,
    exit_with: Option<u32>,
}

fn main() -> ExitCode {
    let options = match options(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("dispatch-status: {error}");
            eprintln!("usage: dispatch-status --ledger <path> [--port <n>] [--subagents <dir>] [--exit-with <pid>]");
            return ExitCode::from(2);
        }
    };
    let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, options.port)) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("dispatch-status: can't listen on 127.0.0.1:{}: {error}", options.port);
            return ExitCode::FAILURE;
        }
    };
    if let Some(pid) = options.exit_with {
        std::thread::spawn(move || exit_with(pid));
    }
    println!(
        "dispatch-status: http://127.0.0.1:{}/ (ledger {}, agents in {})",
        options.port,
        options.ledger.display(),
        options.agents.describe(),
    );
    let mut status = Status::new(options.ledger, options.agents);
    for stream in listener.incoming().flatten() {
        // A client that goes away mid-request costs nothing but that request.
        let _ = serve(stream, &mut status, options.port);
    }
    ExitCode::SUCCESS
}

fn options(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut ledger = None;
    let mut port = DEFAULT_PORT;
    let mut subagents = None;
    let mut exit_with = std::env::var("CLAUDE_PID").ok().and_then(|pid| pid.parse().ok());
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--ledger" => ledger = Some(PathBuf::from(value()?)),
            "--port" => port = value()?.parse().map_err(|_| "--port takes a port number".to_owned())?,
            "--subagents" => subagents = Some(PathBuf::from(value()?)),
            "--exit-with" => exit_with = Some(value()?.parse().map_err(|_| "--exit-with takes a pid".to_owned())?),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let ledger = ledger.ok_or("--ledger is required")?;
    Ok(Options {
        ledger,
        port,
        agents: match subagents {
            Some(dir) => Agents::Dir(dir),
            None => match (std::env::var("CLAUDE_CODE_SESSION_ID"), std::env::var("HOME")) {
                (Ok(session), Ok(home)) if !session.is_empty() && !home.is_empty() => Agents::Session {
                    projects: PathBuf::from(home).join(".claude/projects"),
                    session,
                },
                _ => Agents::Nowhere,
            },
        },
        exit_with,
    })
}

/// Exits once process `pid` is gone, checking every few seconds.
fn exit_with(pid: u32) {
    let proc = PathBuf::from(format!("/proc/{pid}"));
    while proc.exists() {
        std::thread::sleep(Duration::from_secs(3));
    }
    println!("dispatch-status: process {pid} ended, stopping");
    std::process::exit(0);
}

fn serve(stream: TcpStream, status: &mut Status, port: u16) -> std::io::Result<()> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let mut reader = BufReader::new(stream);
    let mut head = Vec::new();
    loop {
        let before = head.len();
        if reader.read_until(b'\n', &mut head)? == 0 || head.len() > HEAD_MAX {
            return Ok(());
        }
        if head[before..] == *b"\r\n" || head[before..] == *b"\n" {
            break;
        }
    }
    let head = String::from_utf8_lossy(&head);
    let (code, kind, body) = respond(&head, status, port);
    let mut stream = reader.into_inner();
    write!(
        stream,
        "HTTP/1.1 {code}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(&body)?;
    stream.flush()
}

/// The status line, content type and body for a request head.
fn respond(head: &str, status: &mut Status, port: u16) -> (&'static str, &'static str, Vec<u8>) {
    const TEXT: &str = "text/plain; charset=utf-8";
    let mut words = head.lines().next().unwrap_or_default().split_whitespace();
    let (method, target) = (words.next().unwrap_or_default(), words.next().unwrap_or_default());
    if method != "GET" {
        return ("405 Method Not Allowed", TEXT, b"GET only\n".to_vec());
    }
    // Only a page loaded from this server itself: a page elsewhere that rebinds a name to 127.0.0.1 sends its own Host.
    if !local_host(head, port) {
        return ("403 Forbidden", TEXT, b"wrong host\n".to_vec());
    }
    match target.split('?').next().unwrap_or_default() {
        "/" | "/index.html" => ("200 OK", "text/html; charset=utf-8", PAGE.as_bytes().to_vec()),
        "/status.json" => {
            let body = status.snapshot(time::now_ms()).to_string().into_bytes();
            ("200 OK", "application/json", body)
        }
        _ => ("404 Not Found", TEXT, b"not found\n".to_vec()),
    }
}

/// Whether the request's `Host` is `127.0.0.1:<port>` or `localhost:<port>`.
fn local_host(head: &str, port: u16) -> bool {
    head.lines().skip(1).any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            let value = value.trim();
            name.eq_ignore_ascii_case("host")
                && (value == format!("127.0.0.1:{port}") || value == format!("localhost:{port}"))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::local_host;

    #[test]
    fn takes_only_its_own_host() {
        assert!(local_host("GET / HTTP/1.1\r\nHost: 127.0.0.1:47231\r\n", 47231));
        assert!(local_host("GET / HTTP/1.1\r\nhost: localhost:47231\r\n", 47231));
        assert!(!local_host("GET / HTTP/1.1\r\nHost: evil.example:47231\r\n", 47231));
        assert!(!local_host("GET / HTTP/1.1\r\n", 47231));
    }
}
