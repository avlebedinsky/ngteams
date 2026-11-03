//! What the personal binary refuses before anything else (`docs/personal-build.md`, "What agents never build a
//! personal build means once the code exists"):
//!
//! - **a build with `test-support`**: `teams-fence`'s (a fence over any roster and log, mock origins) or
//!   `teams-signin`'s (a guard with test rules); cargo unifies it in from the dev-dependencies of any `cargo test`,
//!   `--all-targets` or `--all-features` build, so every binary the gate leaves refuses to run;
//! - **a build `scripts/install-personal.sh` didn't make**: the install marker, an environment variable only the script
//!   sets, read at compile time ([`INSTALLED`]). A plain `cargo build -p ngteams`, debug or release, gives binaries
//!   that refuse to start. It doesn't stop someone who sets the variable on purpose: agents never do (CLAUDE.md).

/// The install marker: set (to anything but empty) only when `scripts/install-personal.sh` compiled this crate.
pub const INSTALLED: Option<&str> = option_env!("TEAMS_PERSONAL_INSTALL");

/// Why a binary of this build may not run, or `None` when it may.
#[must_use]
pub fn refusal() -> Option<&'static str> {
    if teams_fence::test_support_enabled() || teams_signin::test_support_enabled() {
        return Some(
            "built with test-support (cargo test, --all-targets or --all-features unify it in): the personal build \
             runs only as scripts/install-personal.sh builds it",
        );
    }
    if INSTALLED.is_none_or(str::is_empty) {
        return Some(
            "not built by scripts/install-personal.sh (no install marker): the personal build runs only as that \
             script builds and installs it",
        );
    }
    None
}
