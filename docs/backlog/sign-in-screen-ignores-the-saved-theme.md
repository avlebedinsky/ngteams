---
worth: later
where: crates/teams-ui/src/account_prompt.rs, crates/teams-ui/src/theme.rs, crates/ngteams/src/main.rs
added: 2026-11-14
---
# The sign-in screen ignores the saved theme

With `follow_system_theme = false` and `theme = "teams-dark"` in `settings.toml`, a start that opens a sign-in
screen (the first run's, or an own server's account asked for its password) shows it light; the app itself opens
dark once the account is signed in. `account_prompt::open_offering` only follows the window's appearance
(`theme::follow`) and never reads the settings file, which `run_setup` has in hand before it opens the screen.

Hand the saved theme choice to the sign-in screen the way the main window gets it, so the screen follows the
system only when the settings say to. Check every screen `main.rs` opens before an account runs (`ask_account`,
`own_screen`, the password asked again beside running accounts), in Teams Light and a dark theme, in the sandbox.
