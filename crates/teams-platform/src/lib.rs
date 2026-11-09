//! Native OS boundaries. Sensitive files use Unix modes or protected Windows ACLs, including at creation.

pub mod private;

#[cfg(windows)]
pub mod windows_security;
