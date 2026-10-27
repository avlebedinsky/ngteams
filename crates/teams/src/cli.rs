//! The command line: `teams` (the app for the account `account.toml` holds; before the first sign-in, or when the
//! account must sign in again, its window asks for the account first), `teams --account <UPN>` (a shortcut past that
//! screen: the account to sign in, through the sign-in window at once), `teams --forget` (signs out and forgets the account: its `teams-personal` items,
//! `account.toml` and the saved region, refused while Teams runs for it; the store stays, to delete by hand).

use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "teams",
    about = "Microsoft Teams for your own account, under the personal build's fence. Prints no token."
)]
pub struct Cli {
    /// The work account to sign in (its UPN), skipping the window that asks for it: before a sign-in named one it takes
    /// the place of an account given earlier, as the window's does; once a sign-in named one it may only repeat it.
    #[arg(long, value_name = "UPN")]
    pub account: Option<String>,
    /// Sign out and forget the account: its Secret Service items, `account.toml` and the saved region. Refused while
    /// Teams runs for it (quit it first). The store stays.
    #[arg(long, conflicts_with = "account")]
    pub forget: bool,
}
