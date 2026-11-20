//! A copy from Nix runs in an FHS environment whose `/usr`, `/lib64` and `/etc` hold symlinks into the Nix store:
//! the libraries, the loader, fonts and certificates. `WebKit`'s own sandbox mounts those directories but not the
//! store, so without it every one of those links is dangling there and the web processes cannot start.

use std::path::{Path, PathBuf};

const STORE: &str = "/nix/store";
/// Where `NixOS` keeps the system's GPU drivers (`addDriverRunpath`); `libglvnd` from Nix looks there.
const DRIVERS: &str = "/run/opengl-driver";

/// The paths that the web processes' sandbox needs as well, for this process's copy.
pub(super) fn sandbox_paths() -> Vec<PathBuf> {
    std::env::current_exe().map_or_else(|_| Vec::new(), |executable| paths_for(&executable, Path::exists))
}

fn paths_for(executable: &Path, exists: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    if !executable.starts_with(STORE) {
        return Vec::new();
    }
    [STORE, DRIVERS]
        .into_iter()
        .map(PathBuf::from)
        .filter(|path| exists(path))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const IN_STORE: &str = "/nix/store/0000-ngteams-unwrapped/opt/ngteams/usr/bin/ngteams-web";

    #[test]
    fn a_copy_in_the_store_exposes_the_store_and_the_drivers() {
        assert_eq!(
            paths_for(Path::new(IN_STORE), |_| true),
            [PathBuf::from(STORE), PathBuf::from(DRIVERS)]
        );
    }

    #[test]
    fn a_path_that_is_absent_is_not_exposed() {
        assert_eq!(
            paths_for(Path::new(IN_STORE), |path| path == Path::new(STORE)),
            [PathBuf::from(STORE)]
        );
    }

    #[test]
    fn a_copy_outside_the_store_exposes_nothing() {
        assert!(paths_for(Path::new("/usr/bin/ngteams-web"), |_| true).is_empty());
        assert!(paths_for(Path::new("/nix/storefront/ngteams-web"), |_| true).is_empty());
    }

    #[test]
    fn this_test_binary_is_not_in_the_store() {
        assert!(sandbox_paths().is_empty());
    }
}
