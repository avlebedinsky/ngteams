//! The protocol lab harness: lab config, browser profiles, the armed browser fence, captures and scenarios. The
//! `proto-lab` binary is the CLI over this library.

pub mod accounts;
pub mod arming;
pub mod browser;
pub mod bundles;
pub mod calling;
pub mod capture;
pub mod cdp;
pub mod classify;
pub mod commands;
pub mod config;
pub mod fence_http;
pub mod fence_ws;
pub mod sanitize;
pub mod scenario;
pub mod session;
