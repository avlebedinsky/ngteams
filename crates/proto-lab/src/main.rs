//! `proto-lab`: the protocol lab harness CLI. See `docs/plans/20261019-native-teams-m0-lab-and-spikes.md`.
//! `export` turns a capture into a sanitized fixture; `run` runs a scenario.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use proto_lab::browser::Display;
use proto_lab::bundles::import::{self, Imported};
use proto_lab::bundles::index::{self, Kind};
use proto_lab::commands::chrome::{self, ChromeBuild, Installed};
use proto_lab::commands::export;
use proto_lab::commands::login::{self, LoginOptions};
use proto_lab::commands::status::{self, AccountStatus};
use proto_lab::config::{Alias, ConfigError, LabConfig, LabLocations, LabPaths, MriRecord};
use proto_lab::sanitize::Identities;
use proto_lab::scenario::{self, RunOptions, ScenarioName};

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
    /// Sign a lab account into the Teams web client in its profile, headless, under the armed fence, and record its MRI
    /// in lab.toml.
    Login {
        /// The lab account: a, b or c.
        alias: Alias,
        /// Seconds the sign-in and the web client's load may take.
        #[arg(long, default_value_t = 300)]
        timeout: u64,
    },
    /// Whether each lab account's session still works: `ok` or `login needed`, per account. Signs nothing in.
    Status {
        /// The accounts to check; all three by default.
        aliases: Vec<Alias>,
        /// Seconds the web client may take to load, per account.
        #[arg(long, default_value_t = 120)]
        timeout: u64,
    },
    /// Sanitize a capture into a fixture under tests/fixtures; nothing is written unless the fixture guard passes it.
    Export {
        /// The capture: its directory (or its events.jsonl), as a path or relative to the lab's captures dir
        /// (`<scenario>/<ts>`).
        capture: PathBuf,
        /// The fixture's path under tests/fixtures, such as `auth/authz`; `.jsonl` is added without an extension,
        /// and a `.json` name gets one JSON array.
        fixture_name: String,
        /// The fixtures dir; by default the `tests/fixtures` of the cargo workspace around the current directory (the
        /// repository's, never a member crate's own).
        #[arg(long)]
        fixtures_dir: Option<PathBuf>,
    },
    /// Run a scenario in the lab profiles under the armed fence; exits non-zero when its goal isn't reached, the fence
    /// was breached, or the fence denied anything the scenario doesn't expect.
    Run {
        /// The scenario.
        #[arg(value_enum)]
        scenario: ScenarioName,
        /// No browser window (agents always pass it); without it the browsers are visible, for a human watching.
        #[arg(long)]
        headless: bool,
        /// Seconds the web client may take to load, per account.
        #[arg(long, default_value_t = 180)]
        timeout: u64,
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
    match cli.command {
        Command::Login { alias, timeout } => run_login(alias, timeout).await,
        Command::Status { aliases, timeout } => run_status(aliases, timeout).await,
        // these need only the lab directories, so they run without a lab.toml too
        Command::Chrome { command } => run_chrome(command, &LabLocations::from_env()?.lab_paths()?).await,
        Command::Bundles { command } => run_bundles(command, &LabLocations::from_env()?.lab_paths()?).await,
        Command::Run {
            scenario,
            headless,
            timeout,
        } => run_scenario(scenario, headless, timeout).await,
        Command::Export {
            capture,
            fixture_name,
            fixtures_dir,
        } => {
            let locations = LabLocations::from_env()?;
            let paths = locations.lab_paths()?;
            run_export(&locations, &paths, &capture, &fixture_name, fixtures_dir)
        }
    }
}

async fn run_login(alias: Alias, timeout: u64) -> Result<(), Box<dyn std::error::Error>> {
    let options = LoginOptions {
        timeout: Duration::from_secs(timeout),
    };
    let report = login::login(alias, options).await?;
    let how = if report.signed_in {
        "signed in"
    } else {
        "already signed in"
    };
    let mri = match report.mri {
        MriRecord::Recorded => "MRI recorded in lab.toml",
        MriRecord::AlreadyRecorded => "MRI already in lab.toml",
    };
    let teams = match report.refused {
        None => "the web client loaded".to_owned(),
        Some(status) => format!("but Teams refused the account (authz answered {status}; no Teams license?)"),
    };
    println!("{alias}: {how}, {teams}; {mri}");
    eprintln!("{alias}: {} (fail-closed: expected)", report.fence.summary());
    match report.refused {
        None => Ok(()),
        Some(_) => Err(format!("{alias}: Teams refused the account").into()),
    }
}

async fn run_status(aliases: Vec<Alias>, timeout: u64) -> Result<(), Box<dyn std::error::Error>> {
    let config = LabConfig::load(&LabLocations::from_env()?)?;
    let aliases = if aliases.is_empty() {
        Alias::ALL.to_vec()
    } else {
        aliases
    };
    let timeout = Duration::from_secs(timeout);
    let mut all_ok = true;
    for alias in aliases {
        let (state, fence) = status::check(config.paths(), alias, timeout).await?;
        println!("{alias}: {state}");
        eprintln!("{alias}: {} (fail-closed: expected)", fence.summary());
        if fence.breached() {
            return Err(format!("{alias}: the fence was breached").into());
        }
        all_ok &= state == AccountStatus::Ok;
    }
    if all_ok {
        Ok(())
    } else {
        Err("not every account is ok".into())
    }
}

async fn run_scenario(scenario: ScenarioName, headless: bool, timeout: u64) -> Result<(), Box<dyn std::error::Error>> {
    let paths = LabLocations::from_env()?.lab_paths()?;
    let options = RunOptions {
        display: if headless { Display::Headless } else { Display::Headed },
        load_timeout: Duration::from_secs(timeout),
    };
    let report = scenario::run(scenario, &paths, options).await?;
    for note in &report.notes {
        println!("{note}");
    }
    for failure in &report.failures {
        eprintln!("{scenario}: {failure}");
    }
    if report.failed() {
        Err(format!("{scenario} failed").into())
    } else {
        println!("{scenario}: ok");
        Ok(())
    }
}

fn run_export(
    locations: &LabLocations,
    paths: &LabPaths,
    capture: &Path,
    fixture_name: &str,
    fixtures_dir: Option<PathBuf>,
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
    let fixtures_dir = match fixtures_dir {
        Some(dir) => dir,
        None => export::find_fixtures_dir(&std::env::current_dir()?)?,
    };
    let exported = export::export(&capture, fixture_name, &fixtures_dir, &identities)?;
    println!("exported {} event(s) to {}", exported.events, exported.path.display());
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
