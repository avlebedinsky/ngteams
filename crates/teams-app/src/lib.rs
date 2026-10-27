//! `teams-app`: the lab's GPUI app for one or more lab accounts in one window, and its credential broker.
//!
//! `teams-app --lab <alias> --secrets-fd <n> [--lab <alias> --secrets-fd <n> ...] [--trace-fd <n>]` is the app
//! ([`app`]): one window over each lab account named (the first one shown first, the rail's avatars above Settings
//! switching between them, every account live at once: its notifications and incoming calls whichever is shown), each
//! account's client built as `teams-headless` builds its client (`teams_core::lab::start`: the fence from
//! `Fence::open_lab_account`, the account's `Session`, the region's chat base, the live member read with the chat
//! transport's credential, the store at `LabPaths::store_file`, Trouter), its tokens only through its own credential
//! broker's socket (the `--secrets-fd` paired with its `--lab`, in order: a `teams_auth::broker::BrokerSecretStore`;
//! the app never opens the Secret Service itself and has no fallback). The accounts' fences share one
//! `teams_fence::CallFloor`: one call or ringing invitation at a time across them. It refuses a `test-support` build
//! and a window anywhere but the UI sandbox ([`guard`]), reports another engine on the first account and exits (on a
//! later account, reports it and runs without that account), writes the view trace to `--trace-fd` ([`trace`]) and
//! prints no token. Before the engines start it installs the diagnostic log (`teams-log`:
//! `<data_dir>/logs/<first alias>/teams-app.log`, the settings' `log_level`, `TEAMS_APP_LOG` overriding it;
//! [`app::install_log`]); one that can't be opened is said once and the app runs without it. Its settings are the
//! first account's file, the other accounts' selected chat and manual status kept in it under their MRIs. When an
//! account's session can't renew its tokens, the window shows that account signed out (run `proto-lab auth <alias>`)
//! and its engine is stopped, so nothing more is sent for it. Its notifications and its tray item go to the session
//! bus it is given (its `DBUS_SESSION_BUS_ADDRESS`, the sandbox's private bus) and nowhere else; closing the window
//! keeps the app in the tray, whose "Quit" quits it, every engine shut down first (`teams_ui::desktop`). Lab accounts
//! sign in through `proto-lab auth`, so the window adds and removes none.
//!
//! The app runs in the UI sandbox, whose session bus is the sandbox's own, so it can't reach the Secret Service where
//! the lab's tokens are. `teams-app secrets --lab <alias> --fd <n>` ([`secrets`]) runs outside the sandbox, with the
//! user's bus, and serves that account's own `teams-lab` items, nothing else, on descriptor `n`, one end of a socket
//! pair; the app gets the other end. An app of several accounts has one broker per account.
//!
//! Its calls (M3 Task 17, [`calling`]): each engine's calling and the window's on the UI sandbox's PipeWire (the only
//! one a lab build may open, opened once for every account), the ringtone on its speaker; without it the app runs
//! without calls.
//!
//! [`cli`] is the command line. M2 Tasks 4, 6 and 14 (`docs/plans/20261021-native-teams-m2-gpui-daily-driver.md`).

pub mod app;
pub mod calling;
pub mod cli;
pub mod guard;
pub mod secrets;
pub mod trace;
