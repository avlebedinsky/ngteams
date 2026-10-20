//! `proto-lab`: the protocol lab harness CLI.
//!
//! `login` and `status` arrive with the armed fence (M0 Task 8); see
//! `docs/plans/20261019-native-teams-m0-lab-and-spikes.md`. `export` turns a capture into a sanitized
//! reference copy outside the repository, read to write a synthetic fixture.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use proto_lab::bundles::import::{self, Imported};
use proto_lab::bundles::index::{self, Kind};
use proto_lab::commands::chrome::{self, ChromeBuild, Installed};
use proto_lab::commands::export;
use proto_lab::config::{ConfigError, LabConfig, LabLocations, LabPaths};
use proto_lab::sanitize::Identities;

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
    /// The Teams web bundles, unpacked from the MSIX for static analysis (never executed).
    Bundles {
        #[command(subcommand)]
        command: BundlesCommand,
    },
    /// Sanitize a capture into a reference copy under the lab's reference/ dir, outside any repository, to read when
    /// writing a synthetic fixture; nothing is written unless every check and the fixture guard pass it.
    Export {
        /// The capture: its directory (or its events.jsonl), as a path or relative to the lab's captures dir
        /// (`<scenario>/<ts>`).
        capture: PathBuf,
        /// The copy's path under the reference dir, such as `auth/authz`; `.jsonl` is added without an extension, and
        /// a `.json` name gets one JSON array.
        name: String,
    },
}

#[derive(Subcommand)]
enum ChromeCommand {
    /// Download the pinned build into the lab's chrome dir and verify its checksum.
    Install,
    /// Print the browser binary of the pinned build.
    Path,
}

#[derive(Subcommand)]
enum BundlesCommand {
    /// Unpack desktop-assets and client-bindings from a Teams MSIX into the lab's bundles/<version>/ and index them.
    Import {
        /// The Windows MSIX package.
        msix: PathBuf,
    },
    /// Search the index of an imported bundle for urls, hosts, routes and methods.
    Search {
        /// A regular expression (case-insensitive) matched against each value and a method's paired url.
        pattern: String,
        /// Only entries of this kind.
        #[arg(long, value_enum)]
        kind: Option<Kind>,
        /// The bundle version to search; the newest imported one by default.
        #[arg(long)]
        version: Option<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if teams_fence::test_support_enabled() {
        // cargo unified a dev-dependency's feature into this build (`cargo test`, `--all-targets`, `--all-features`):
        // its fence API would take any roster and any member list, so this binary is for the test harness only
        eprintln!("proto-lab: built with teams-fence's test-support feature; rebuild with `cargo build -p proto-lab`");
        return ExitCode::FAILURE;
    }
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
    // these commands need only the lab directories, so they run without a lab.toml too
    let locations = LabLocations::from_env()?;
    let paths = locations.lab_paths()?;
    match cli.command {
        Command::Chrome { command } => run_chrome(command, &paths).await,
        Command::Bundles { command } => run_bundles(command, &paths).await,
        Command::Export { capture, name } => run_export(&locations, &paths, &capture, &name),
    }
}

fn run_export(
    locations: &LabLocations,
    paths: &LabPaths,
    capture: &Path,
    name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // the lab's own accounts get their alias as placeholder label; without a lab.toml every identity gets a number
    let identities = match LabConfig::load(locations) {
        Ok(config) => Identities::from_lab(&config),
        Err(ConfigError::Read { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("no lab.toml: the lab accounts get numbered placeholders too");
            Identities::default()
        }
        Err(error) => return Err(error.into()),
    };
    let capture = if capture.exists() {
        capture.to_owned()
    } else {
        paths.captures_dir().join(capture)
    };
    let reference_dir = export::reference_dir(paths.data_dir());
    let exported = export::export(&capture, name, &reference_dir, &identities)?;
    println!(
        "wrote a sanitized reference copy of {} event(s) to {}; it stays outside the repository: read it to write a \
         synthetic fixture (same shape, made-up values, a \"_synthetic\" marker)",
        exported.events,
        exported.path.display()
    );
    Ok(())
}

async fn run_chrome(command: ChromeCommand, paths: &LabPaths) -> Result<(), Box<dyn std::error::Error>> {
    let chrome_dir = paths.chrome_dir();
    let build = ChromeBuild::pinned();
    match command {
        ChromeCommand::Install => {
            let client = reqwest::Client::builder().build()?;
            match chrome::install(&build, &chrome_dir, &client).await? {
                Installed::Fresh(path) => {
                    println!("installed Chrome for Testing {}: {}", build.version, path.display());
                }
                Installed::Present(path) => println!("already installed: {}", path.display()),
            }
        }
        ChromeCommand::Path => println!("{}", build.executable(&chrome_dir).display()),
    }
    Ok(())
}

async fn run_bundles(command: BundlesCommand, paths: &LabPaths) -> Result<(), Box<dyn std::error::Error>> {
    let bundles_dir = paths.bundles_dir();
    match command {
        BundlesCommand::Import { msix } => {
            let imported = tokio::task::spawn_blocking(move || import::import(&msix, &bundles_dir)).await??;
            match imported {
                Imported::Fresh(bundle) => {
                    let entries = index::Index::read(&bundle.dir.join(index::INDEX_FILE))?.entries().len();
                    println!(
                        "imported {} into {} ({entries} index entries)",
                        bundle.version,
                        bundle.dir.display()
                    );
                }
                Imported::Present(bundle) => {
                    println!("already imported: {} in {}", bundle.version, bundle.dir.display());
                }
            }
        }
        BundlesCommand::Search { pattern, kind, version } => search(&bundles_dir, &pattern, kind, version.as_deref())?,
    }
    Ok(())
}

fn search(
    bundles_dir: &Path,
    pattern: &str,
    kind: Option<Kind>,
    version: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let pattern = index::search_pattern(pattern)?;
    let (version, index) = index::load(bundles_dir, version)?;
    let mut matches = 0usize;
    for entry in index.search(&pattern, kind) {
        println!("{}", index::render(entry));
        matches += 1;
    }
    eprintln!("{matches} match(es) in bundle {version}");
    Ok(())
}
