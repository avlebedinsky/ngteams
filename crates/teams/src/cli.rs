//! The command line: `teams` (the app for the account `account.toml` holds), `teams --account <UPN>` (the first run:
//! the account to sign in), `teams --forget` (signs out and forgets the account: its `teams-personal` items,
//! `account.toml` and the saved region; the store stays, to delete by hand).

use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "teams",
    about = "Microsoft Teams for your own account, under the personal build's fence. Prints no token."
)]
pub struct Cli {
    /// The work account to sign in (its UPN): needed once, before the first sign-in; afterwards it may only repeat the
    /// account `account.toml` holds.
    #[arg(long, value_name = "UPN")]
    pub account: Option<String>,
    /// Sign out and forget the account: its Secret Service items, `account.toml` and the saved region. The store stays.
    #[arg(long, conflicts_with = "account")]
    pub forget: bool,
}
