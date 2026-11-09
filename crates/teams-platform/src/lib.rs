//! Native OS boundaries. Sensitive files use Unix modes or protected Windows ACLs, including at creation.

pub mod private;

#[cfg(windows)]
pub mod windows_security;

#[cfg(windows)]
pub mod windows_process;

#[cfg(windows)]
pub mod windows_sandbox;

#[cfg(windows)]
pub mod windows_job;

pub mod command;

#[cfg(windows)]
mod windows_command;

#[cfg(windows)]
pub mod windows_desktop;
