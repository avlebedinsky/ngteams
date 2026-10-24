//! What keeps the lab and the personal build apart on disk (`docs/personal-build.md`, "How the lab and personal
//! binaries are kept apart"): where the personal build's directories are, and whether two directories overlap.
//!
//! Compiled in every build: a lab binary refuses a lab data dir that is, contains or lies inside the personal data dir
//! ([`crate::lab::LabConfig::load`], [`crate::lab::LabLocations::lab_paths`]), and the personal build refuses the
//! reverse (`crate::personal`, only with the `personal` feature). Only the paths live here; nothing in this module opens,
//! reads or writes the personal build's data.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

/// The personal build's directory name under `$XDG_DATA_HOME` and `$XDG_CONFIG_HOME`.
pub const PERSONAL_DIR: &str = "teams";

/// Where the personal build keeps its data and its config by default: `$XDG_DATA_HOME/teams` (`~/.local/share/teams`)
/// and `$XDG_CONFIG_HOME/teams` (`~/.config/teams`), by the XDG rules [`crate::lab::LabLocations::resolve`] follows (an
/// empty or relative XDG directory is ignored in favour of the `HOME` default).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersonalLocations {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl PersonalLocations {
    /// From `XDG_CONFIG_HOME`, `XDG_DATA_HOME` and `HOME`; `None` when a fallback to `HOME` is needed and it is unset
    /// or relative.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        Self::resolve(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("XDG_DATA_HOME"),
            std::env::var_os("HOME"),
        )
    }

    /// The XDG rules on explicit values.
    #[must_use]
    pub fn resolve(config_home: Option<OsString>, data_home: Option<OsString>, home: Option<OsString>) -> Option<Self> {
        let home = home.map(PathBuf::from).filter(|home| home.is_absolute());
        let base = |xdg: Option<OsString>, fallback: &str| {
            xdg.map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .or_else(|| home.as_ref().map(|home| home.join(fallback)))
        };
        Some(Self {
            config_dir: base(config_home, ".config")?.join(PERSONAL_DIR),
            data_dir: base(data_home, ".local/share")?.join(PERSONAL_DIR),
        })
    }
}

/// Whether `one` and `other` are the same directory, or one lies inside the other, after resolving symlinks: each path
/// is resolved component by component as the filesystem would ([`resolved`]), so a `..` after a symlink leaves the
/// symlink's target, and a directory not made yet is compared where it would be made.
#[must_use]
pub fn overlapping(one: &Path, other: &Path) -> bool {
    let (one, other) = (resolved(one), resolved(other));
    one.starts_with(&other) || other.starts_with(&one)
}

/// `path` resolved one component at a time: while the path so far exists it is canonicalized after each component
/// (its symlinks followed, as the filesystem does before it takes the next `..`); past the last existing component the
/// rest is appended with `.` dropped and `..` taking one component off.
fn resolved(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => {
                out.push(other.as_os_str());
                if let Ok(canonical) = out.canonicalize() {
                    out = canonical;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn the_personal_dirs_follow_the_xdg_rules() {
        let locations =
            PersonalLocations::resolve(None, Some("relative".into()), Some("/home/someone".into())).unwrap();
        assert_eq!(locations.data_dir, Path::new("/home/someone/.local/share/teams"));
        assert_eq!(locations.config_dir, Path::new("/home/someone/.config/teams"));
        let locations = PersonalLocations::resolve(Some("/c".into()), Some("/d".into()), None).unwrap();
        assert_eq!(locations.data_dir, Path::new("/d/teams"));
        assert_eq!(locations.config_dir, Path::new("/c/teams"));
        assert!(PersonalLocations::resolve(None, None, Some("relative".into())).is_none());
    }

    #[test]
    fn a_dir_overlaps_itself_its_parents_and_its_children_through_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(overlapping(&real, &real));
        assert!(overlapping(&real.join("teams"), &real));
        assert!(overlapping(&real, &real.join("not/yet")));
        assert!(overlapping(&link.join("teams"), &real.join("teams")));
        assert!(overlapping(&real.join("a/../teams"), &link.join("teams/store")));
        assert!(!overlapping(&real.join("teams-lab"), &real.join("teams")));
        assert!(!overlapping(&dir.path().join("other"), &real));
    }

    #[test]
    fn a_parent_after_a_symlink_leaves_the_symlinks_target() {
        let dir = tempfile::tempdir().unwrap();
        let personal = dir.path().join("data/teams");
        std::fs::create_dir_all(personal.join("sub")).unwrap();
        let scratch = dir.path().join("scratch");
        std::fs::create_dir(&scratch).unwrap();
        let link = scratch.join("link");
        std::os::unix::fs::symlink(personal.join("sub"), &link).unwrap();
        // `scratch/link/..` is `data/teams` on the filesystem, not `scratch`
        assert!(overlapping(&link.join("../lab"), &personal));
        assert!(!overlapping(&scratch.join("lab"), &personal));
        assert!(overlapping(&link.join("../not/yet/../lab"), &personal));
    }
}
