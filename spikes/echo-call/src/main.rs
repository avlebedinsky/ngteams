//! `echo-call`: places the echo call from lab account A, as far as it can go (see `echo_call::call`): the fence allows
//! the test call (A and the `lab.toml` echo service, the only echo there is: no flag names another), the
//! skypetoken is read, and the call start is not sent: the echo would join only through writes on the started call
//! that the fence can't permit yet. Nothing is sent to Microsoft; no incoming call is ever answered.

use std::process::ExitCode;

use clap::Parser;
use echo_call::call::{CallError, authorize, echo_call, skypetoken, start};
use teams_auth::secrets::KeyringSecretStore;
use teams_fence::Fence;
use teams_fence::lab::{Alias, LabConfig, LabLocations, LabPaths};

/// Place the Teams echo call from a lab account (spike 3b).
#[derive(Parser)]
struct Cli {
    /// The lab account that calls.
    #[arg(long, default_value = "a")]
    caller: String,
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
    let call = echo_call(&config, caller).map_err(|e| e.to_string())?;
    let call = authorize(&fence, call).await.map_err(|error| match error {
        CallError::Denied(_) => format!("{error} (the call needs {caller} and an approved echo service)"),
        error => error.to_string(),
    })?;
    println!("fence: the call is allowed");
    // the keyring is opened only once the fence allowed the call
    let store = KeyringSecretStore::new(LabPaths::KEYRING_APPLICATION)
        .await
        .map_err(|e| e.to_string())?;
    let token = skypetoken(&store, &config.account(caller).upn)
        .await
        .map_err(|e| e.to_string())?;
    start(&call, &token).map_err(|e| e.to_string())
}
