//! The worker's launch and the socket protocol both sides speak (`teams-wpe` serves it, module docs of its `worker`).

mod client;
#[doc(hidden)]
pub mod message;
#[doc(hidden)]
pub mod wire;

#[doc(hidden)]
pub use client::Client;
pub use client::{pin_worker, pin_worker_path, probe_system};

/// The worker binary, beside the app's.
pub const WORKER_BINARY: &str = "ngteams-web";
/// The variable that makes a process the worker: the number of the socket descriptor it inherited.
pub const WORKER_FD_ENV: &str = "TEAMS_WPE_WORKER_FD";
/// The number of the descriptor of the image the worker was started from (`/proc/self/fd/<n>`), closed at once.
pub const WORKER_IMAGE_ENV: &str = "TEAMS_WPE_WORKER_IMAGE_FD";

/// The identity compiled into both sides by one build (the release's commit, when it names one): the handshake refuses
/// a worker from any other build.
#[must_use]
pub fn build_identity() -> String {
    format!(
        "{}:{}",
        env!("CARGO_PKG_VERSION"),
        option_env!("NGTEAMS_BUILD_COMMIT").unwrap_or("")
    )
}
