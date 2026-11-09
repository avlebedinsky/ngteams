//! What the sign-in window's guard (`teams-signin`) and its web process extension (`teams-signin-extension`, which links
//! `WebKit`) share, in a crate of its own so the app's binary needs no `WebKit` to link.

pub mod protocol;

/// The name the module exports for `WebKit`: `teams-signin` checks the file it hands `WebKit` carries it.
pub const ENTRY_POINT: &str = "webkit_web_process_extension_initialize_with_user_data";
