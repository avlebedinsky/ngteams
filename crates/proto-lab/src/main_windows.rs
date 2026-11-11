//! The Windows lab tool deliberately exposes only browser installation, login and native provisioning.
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

use clap::{Parser, Subcommand};
use proto_lab::accounts::{AccountsFile, accounts_file};
use proto_lab::commands::auth;
use proto_lab::config::{Alias, LabConfig, LabLocations};
use teams_auth::secrets::KeyringSecretStore;

#[derive(Parser)]
#[command(name = "proto-lab", about = "Windows lab sign-in and native token provisioning")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check the installed Microsoft-signed Edge browser. Does not sign in.
    Browser,
    /// Provision a roster account directly through Microsoft authorization, submitting the password at most once.
    Login {
        alias: Alias,
        #[arg(long)]
        native_alias: Option<String>,
        #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u64).range(1..=900))]
        timeout: u64,
    },
    /// Complete native provisioning from an already verified account's stored refresh token, with no browser.
    Refresh {
        alias: Alias,
        #[arg(long)]
        native_alias: Option<String>,
    },
    /// Read all chat pages and actual participants before starting a signed-in engine.
    Reach {
        alias: Alias,
        #[arg(long)]
        native_alias: Option<String>,
        /// Explicit targets of the intended run; visible channels remain inventory only.
        #[arg(long = "chat")]
        chats: Vec<String>,
    },
    /// Publish verified tokens into ngteams-lab in this interactive console user's Credential Manager.
    Auth {
        alias: Alias,
        /// The already prepared native runner alias (defaults to the roster alias).
        #[arg(long)]
        native_alias: Option<String>,
        #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u64).range(1..=900))]
        timeout: u64,
    },
}

pub fn main() -> ExitCode {
    if proto_lab::test_support_enabled() {
        eprintln!("proto-lab: built with proto-lab's test-support feature; rebuild with cargo build -p proto-lab");
        return ExitCode::FAILURE;
    }
    let cli = Cli::parse();
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| Box::new(error) as Box<dyn std::error::Error>);
    let outcome = result.and_then(|runtime| {
        runtime.block_on(async {
            tokio::select! {
                result = run(cli.command) => result,
                _ = tokio::signal::ctrl_c() => Err("lab command interrupted".into()),
            }
        })
    });
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("proto-lab: {}", failure(error.as_ref()));
            ExitCode::FAILURE
        }
    }
}

async fn run(command: Command) -> Result<(), Box<dyn std::error::Error>> {
    let locations = LabLocations::from_env()?;
    teams_platform::windows_sandbox::window_allowed_here()?;
    teams_platform::private::create_dir_all(&locations.data_dir)?;
    teams_platform::private::restrict_dir(&locations.data_dir)?;
    let _live = proto_lab::private::exclusive(&locations.data_dir.join("live.lock"))?;
    match command {
        Command::Browser => {
            println!(
                "installed lab browser: {}",
                proto_lab::browser::verify_installed_edge().await?.display()
            );
        }
        Command::Login {
            alias,
            native_alias,
            timeout,
        } => provision(&locations, alias, native_alias.as_deref(), timeout, true).await?,
        Command::Auth {
            alias,
            native_alias,
            timeout,
        } => provision(&locations, alias, native_alias.as_deref(), timeout, false).await?,
        Command::Refresh { alias, native_alias } => {
            let config = LabConfig::load(&locations)?;
            let target = auth::NativeTarget::windows_alias(native_alias.as_deref().unwrap_or(alias.as_str()))?;
            let store = KeyringSecretStore::new(auth::KEYRING_APPLICATION).await?;
            let skype = auth::refresh_native(&config, alias, &store, &target).await?;
            println!(
                "native credentials provisioned; skypetoken expiry: {:?}",
                skype.expires_at
            );
        }
        Command::Reach {
            alias,
            native_alias,
            chats,
        } => {
            let config = LabConfig::load(&locations)?;
            let target = auth::NativeTarget::windows_alias(native_alias.as_deref().unwrap_or(alias.as_str()))?;
            proto_lab::commands::reach_windows::audit(&config, alias, &target, &chats).await?;
        }
    }
    Ok(())
}

async fn provision(
    locations: &LabLocations,
    alias: Alias,
    native_alias: Option<&str>,
    timeout: u64,
    password: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = LabConfig::load(locations)?;
    let target = auth::NativeTarget::windows_alias(native_alias.unwrap_or(alias.as_str()))?;
    let native_file = target.root().join("accounts.toml");
    let store = KeyringSecretStore::new(auth::KEYRING_APPLICATION).await?;
    let options = auth::AuthOptions {
        timeout: Duration::from_secs(timeout),
        skypetoken: true,
        other_audiences: false,
    };
    let report = if password {
        let accounts = AccountsFile::load(&accounts_file(locations), |name| std::env::var(name).ok())?;
        let credentials = accounts.for_alias(alias, &config.account(alias).upn)?;
        auth::authorize_password(&config, alias, &store, &options, target, credentials).await?
    } else {
        auth::authorize_target(&config, alias, &store, &options, target).await?
    };
    show_report(&report)?;
    let native: toml::Table = toml::from_str(&std::fs::read_to_string(native_file)?)?;
    let oid = native
        .get("account")
        .and_then(toml::Value::as_array)
        .and_then(|entries| entries.first())
        .and_then(|entry| entry.get("oid"))
        .and_then(toml::Value::as_str)
        .ok_or("verified identity was not saved")?;
    proto_lab::config::record_account_mri(locations, alias, &format!("8:orgid:{oid}"))?;
    Ok(())
}

fn show_report(report: &auth::AuthReport) -> Result<(), Box<dyn std::error::Error>> {
    for line in report.lines(SystemTime::now()) {
        println!("{line}");
    }
    if !matches!(report.skypetoken, Some(Ok(_))) {
        return Err("no native skypetoken".into());
    }
    Ok(())
}

fn failure(error: &(dyn std::error::Error + 'static)) -> String {
    use proto_lab::commands::login::LoginError;
    use teams_auth::token::TokenError;
    if let Some(reason) = error.downcast_ref::<proto_lab::commands::reach_windows::Refused>() {
        return reason.to_string();
    }
    if let Some(reason) = error.downcast_ref::<teams_proto::chat::ChatReadError>() {
        return format!(
            "chat reach GET failed; HTTP {:?}, service code {:?}",
            reason.status(),
            reason.service_code()
        );
    }
    if let Some(reason) = error.downcast_ref::<std::io::Error>() {
        return format!("private lab input/output failed ({:?})", reason.kind());
    }
    if error.is::<teams_proto::BuildError>() {
        return "native HTTP service-base validation failed".into();
    }
    match error.downcast_ref::<auth::AuthError>() {
        Some(auth::AuthError::Login(LoginError::SignIn(stop))) => stop.to_string(),
        Some(auth::AuthError::Login(LoginError::Timeout { .. })) => "sign-in deadline expired".into(),
        Some(auth::AuthError::Login(LoginError::UnexpectedHost(_))) => "unexpected authorization host; stopped".into(),
        Some(auth::AuthError::Token(TokenError::ConditionalAccess(reason))) => {
            format!("Conditional Access refused the grant; AADSTS codes {:?}", reason.codes)
        }
        Some(auth::AuthError::Token(TokenError::InteractionRequired(reason))) => {
            format!("Microsoft requires MFA or consent; AADSTS codes {:?}", reason.codes)
        }
        Some(auth::AuthError::InteractionNeeded) => "browser session needs sign-in interaction".into(),
        Some(auth::AuthError::Session(_)) => "browser session setup or shutdown failed".into(),
        Some(auth::AuthError::Provision(reason)) => format!("native account publication failed ({:?})", reason.kind()),
        Some(auth::AuthError::Token(_)) => "token exchange or account validation failed".into(),
        Some(auth::AuthError::Redirect(_)) => "authorization redirect refused".into(),
        _ => "lab command failed (private input, browser, service or output); raw error data withheld".into(),
    }
}

#[cfg(test)]
#[path = "../tests/support/cli_windows.rs"]
mod tests;
