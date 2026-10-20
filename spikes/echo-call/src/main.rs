//! `echo-call`: places the echo call from lab account A, as far as it can go (see `echo_call::call`). Today it stops
//! before the fence: the echo service has no MRI in `lab.toml` yet (`--callee` with an MRI `lab.toml` doesn't approve
//! is denied by the fence). Nothing is sent to Microsoft until the signaling is confirmed by Task 18's capture; no
//! incoming call is ever answered.

use std::process::ExitCode;

use clap::Parser;
use echo_call::call::{CallError, authorize, echo_call, skypetoken, start};
use teams_auth::secrets::KeyringSecretStore;
use teams_fence::lab::{Alias, LabConfig, LabLocations};
use teams_fence::{Fence, Mri};

/// The Secret Service application `proto-lab auth` stores the lab's tokens under.
const KEYRING_APPLICATION: &str = "teams-lab";

/// Place the Teams echo call from a lab account (spike 3b).
#[derive(Parser)]
struct Cli {
    /// The lab account that calls.
    #[arg(long, default_value = "a")]
    caller: String,
    /// The callee's MRI instead of lab.toml's echo service. The fence still decides: only lab.toml approves.
    #[arg(long)]
    callee: Option<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if teams_fence::test_support_enabled() {
        // cargo unified a dev-dependency's feature into this build (`cargo test`, `--all-targets`, `--all-features`)
        eprintln!("echo-call: built with teams-fence's test-support feature; rebuild with `cargo build -p echo-call`");
        return ExitCode::FAILURE;
    }
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("echo-call: can't start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("echo-call: {message}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<(), String> {
    let caller: Alias = cli.caller.parse().map_err(|e| format!("{e}"))?;
    let locations = LabLocations::from_env().map_err(|e| e.to_string())?;
    let config = LabConfig::load(&locations).map_err(|e| e.to_string())?;
    let fence = Fence::open_lab().map_err(|e| e.to_string())?;
    let call = echo_call(&config, caller, cli.callee.map(Mri::new)).map_err(|e| e.to_string())?;
    let call = authorize(&fence, call).await.map_err(|error| match error {
        CallError::Denied(_) => format!("{error} (the call needs {caller} and an approved echo service)"),
        error => error.to_string(),
    })?;
    println!("fence: the call is allowed");
    // the keyring is opened only once the fence allowed the call
    let store = KeyringSecretStore::new(KEYRING_APPLICATION)
        .await
        .map_err(|e| e.to_string())?;
    let token = skypetoken(&store, &config.account(caller).upn)
        .await
        .map_err(|e| e.to_string())?;
    start(&call, &token).map_err(|e| e.to_string())
}
