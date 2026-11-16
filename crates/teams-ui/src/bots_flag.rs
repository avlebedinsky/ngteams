//! The switch for the bot and app features while they are being built: `NGTEAMS_BOTS=1` shows them, or a
//! [`BotsFlag`] the app or a test set.

use gpui::{App, Global};

/// The variable that shows the bot and app features while they are being built (`NGTEAMS_BOTS=1`).
pub const BOTS_ENV: &str = "NGTEAMS_BOTS";

/// Whether the bot and app features show, when a test or the app decided it; else [`BOTS_ENV`] decides.
#[derive(Clone, Copy, Debug)]
pub struct BotsFlag(pub bool);

impl Global for BotsFlag {}

/// Whether the bot and app features show: [`BotsFlag`] when set, else `NGTEAMS_BOTS=1`.
#[must_use]
pub fn bots_on(cx: &App) -> bool {
    cx.try_global::<BotsFlag>()
        .map_or_else(|| std::env::var(BOTS_ENV).as_deref() == Ok("1"), |flag| flag.0)
}
