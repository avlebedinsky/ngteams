//! `proto-lab`: the protocol lab harness CLI.
//!
//! `login` and `status` arrive with the armed fence (M0 Task 8); see
//! `docs/plans/20261019-native-teams-m0-lab-and-spikes.md`.

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use proto_lab::commands::chrome::{self, ChromeBuild, Installed};
use proto_lab::config::{LabConfig, LabLocations};

#[derive(Parser)]
#[command(name = "proto-lab", about = "The Teams protocol lab harness")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// The pinned Chrome for Testing build.
    Chrome {
        #[command(subcommand)]
        command: ChromeCommand,
    },
}

#[derive(Subcommand)]
enum ChromeCommand {
    /// Download the pinned build into the lab's chrome dir and verify its checksum.
    Install,
    /// Print the browser binary of the pinned build.
    Path,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("proto-lab: can't start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("proto-lab: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let config = LabConfig::load(&LabLocations::from_env()?)?;
    let chrome_dir = config.paths().chrome_dir();
    let build = ChromeBuild::pinned();
    match cli.command {
        Command::Chrome {
            command: ChromeCommand::Install,
        } => {
            let client = reqwest::Client::builder().build()?;
            match chrome::install(&build, &chrome_dir, &client).await? {
                Installed::Fresh(path) => {
                    println!("installed Chrome for Testing {}: {}", build.version, path.display());
                }
                Installed::Present(path) => println!("already installed: {}", path.display()),
            }
        }
        Command::Chrome {
            command: ChromeCommand::Path,
        } => println!("{}", build.executable(&chrome_dir).display()),
    }
    Ok(())
}
