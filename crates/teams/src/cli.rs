//! The command line: `teams` (the app for the accounts `accounts.toml` holds; before the first sign-in, or when an
//! account must sign in again, its window asks), `teams --account <UPN>` (a shortcut past that screen: the account to
//! sign in, added when it is a new one, through the sign-in window at once), `teams --forget <UPN>` (signs out and
//! forgets that account: its `teams-personal` items, its `accounts.toml` entry and its saved region, refused while
//! Teams runs for it; its store stays, to delete by hand).

use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "teams",
    about = "Microsoft Teams for your own accounts, under the personal build's fence. Prints no token."
)]
pub struct Cli {
    /// A work account to sign in (its UPN) before the app starts, skipping the window that asks for it: one of the
    /// accounts already given, or a new one added after them.
    #[arg(long, value_name = "UPN")]
    pub account: Option<String>,
    /// Sign out and forget the account of this UPN: its Secret Service items, its entry in `accounts.toml` and its saved
    /// region; the other accounts stay. Refused while Teams runs for it (quit it first). Its store stays.
    #[arg(long, value_name = "UPN", conflicts_with = "account")]
    pub forget: Option<String>,
}
