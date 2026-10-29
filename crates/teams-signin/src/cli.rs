//! The binary's checks and its answer channel, kept apart from GTK so they are tested on their own.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Write as _};
use std::os::fd::AsFd as _;

use teams_auth::signin::WindowAnswer;
use url::{Host, Url};

/// Why the window refuses a page that isn't local: the guard isn't complete (the plan's Task 24), so it never runs
/// against the tenant until a later decision lifts this. It holds for the whole run, not only the first URL: the guard
/// denies every request off a loopback address (`Guard::local_only`), the navigation policy refuses every navigation
/// off one, and the network session's only way out is a proxy that refuses everything (`crate::egress`), for the
/// redirects `WebKit` follows without asking the guard.
pub const NOT_LIVE: &str = "the sign-in window opens only a local page: its guard is not a complete fence \
     (docs/backlog/webkit-sign-in-guard-redirects.md); sign the lab in with `proto-lab auth`";

/// The one mode there is: `teams-signin --lab`, the lab's window, logging to the lab's `fence.log`.
///
/// # Errors
/// Any other arguments, with the usage.
pub fn parse_args<I: IntoIterator<Item = S>, S: AsRef<OsStr>>(args: I) -> Result<(), String> {
    let args: Vec<S> = args.into_iter().collect();
    match args.as_slice() {
        [flag] if flag.as_ref() == "--lab" => Ok(()),
        _ => Err("usage: teams-signin --lab (the request on stdin, the answer on stdout)".into()),
    }
}

/// Whether `url` is a page on this machine: `http` or `https` on a numeric loopback address. A name, `localhost`
/// included, isn't: it is resolved by DNS, and the network session's proxy bypass is numeric only (`crate::egress`).
#[must_use]
pub fn is_local_page(url: &str) -> bool {
    let Ok(url) = Url::parse(url) else {
        return false;
    };
    let loopback = match url.host() {
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        Some(Host::Domain(_)) | None => false,
    };
    matches!(url.scheme(), "http" | "https") && loopback
}

/// Why a window may not open here: outside the UI sandbox, or with an X display GTK could pick.
#[must_use]
pub fn window_refusal(display: Option<&OsStr>) -> Option<String> {
    if display.is_some_and(|display| !display.is_empty()) {
        return Some("DISPLAY is set: the window opens only on the UI sandbox's Wayland compositor".into());
    }
    ui_sandbox::window_allowed().err()
}

/// Why a build may not run: one with `test-support` (a guard with test rules).
#[must_use]
pub fn test_support_refusal() -> Option<&'static str> {
    cfg!(feature = "test-support").then_some(
        "built with `test-support` (cargo test or --all-features): build it with `cargo build -p teams-signin`",
    )
}

/// Why a lab window may not run: a build with the personal build's features (`teams-signin`'s or `teams-fence`'s
/// `personal`), which a lab binary built in one cargo invocation with `ngteams` gets unified in.
#[must_use]
pub fn personal_refusal() -> Option<&'static str> {
    (cfg!(feature = "personal") || teams_fence::personal_enabled()).then_some(
        "built with the personal build's `personal` feature (unified in by a build with `ngteams`): build it with \
         `cargo build -p teams-signin`",
    )
}

/// Takes stdout for the answer: a private copy of the descriptor (close-on-exec, so no child inherits it) is returned,
/// and stdout itself now writes to stderr, so nothing GTK, `WebKit` or their processes print can reach the answer.
///
/// # Errors
/// When the descriptors can't be duplicated.
pub fn take_stdout() -> io::Result<File> {
    let answer = rustix::io::fcntl_dupfd_cloexec(io::stdout().as_fd(), 3)?;
    rustix::stdio::dup2_stdout(io::stderr().as_fd())?;
    Ok(File::from(answer))
}

/// Writes `answer`'s line to `out`.
///
/// # Errors
/// When it can't be written.
pub fn write_answer(out: &mut File, answer: &WindowAnswer) -> io::Result<()> {
    let line = answer.to_line();
    out.write_all(line.expose().as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()
}
