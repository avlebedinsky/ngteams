//! The protocol lab harness: lab config, browser profiles, the armed browser fence, captures and scenarios. The
//! `proto-lab` binary is the CLI over this library.

pub mod arming;
pub mod browser;
pub mod cdp;
pub mod classify;
pub mod commands;
pub mod config;
pub mod fence_http;
