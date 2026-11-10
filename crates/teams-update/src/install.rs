//! A verified update put in the installation's place.

use std::fs;
use std::path::{Path, PathBuf};

use crate::privileged::{self, Prompted};
use crate::{Error, Format, Installation, Version, error};

/// A downloaded and verified update, waiting in its stage; the installation is untouched until [`Prepared::install`].
/// Dropped, its stage goes too, unless it holds a previous installation that needs recovery.
#[derive(Debug)]
pub struct Prepared {
    pub(crate) installation: Installation,
    /// The stage; kept on disk once the update is in ([`Prepared::install`]).
    pub(crate) work: Option<tempfile::TempDir>,
    /// The unpacked tree, the AppImage or the package.
    pub(crate) candidate: PathBuf,
    /// The digest verified against the release's SHA256SUMS, never re-derived from the staged file.
    pub(crate) sha256: String,
    /// The installation's identity when the download started.
    pub(crate) identity: Vec<u8>,
    pub(crate) version: Version,
}

impl Prepared {
    /// The release this update installs.
    #[must_use]
    pub fn version(&self) -> Version {
        self.version
    }

    /// Puts the update in the installation's place and gives back the program a restart runs. A system package is
    /// installed by the package manager behind the desktop's password prompt, which this call waits for.
    ///
    /// The stage stays (it holds an archive's previous version after its swap) until a later start's
    /// [`Installation::cleanup_stale`], so the files of the running process stay where they are until it has quit.
    ///
    /// # Errors
    /// The installation changed since the download started, the prompt was closed ([`Error::Cancelled`]), the desktop
    /// has no prompt, or a step failed. If rollback also fails, the previous installation and recovery marker
    /// remain in the stage named in the error for manual recovery.
    pub fn install(&mut self) -> Result<PathBuf, Error> {
        self.install_with_rename(|from, to| fs::rename(from, to))
    }

    /// The archive's filesystem operations, injectable to pin failures of both the swap and rollback.
    pub(crate) fn install_with_rename(
        &mut self,
        mut rename: impl FnMut(&Path, &Path) -> std::io::Result<()>,
    ) -> Result<PathBuf, Error> {
        let installation = &self.installation;
        installation.validate()?;
        if installation.identity()? != self.identity {
            return Err(error(
                "ngteams was updated by something else after this download started. The next check offers what's \
                 newer.",
            ));
        }
        let stage = self
            .work
            .as_ref()
            .map(|work| work.path().to_path_buf())
            .ok_or_else(|| error("This update is installed already."))?;
        match installation.format {
            Format::Archive => {
                if let Err(error) = swap(&installation.root, &self.candidate, &stage, &mut rename) {
                    // A failed rollback leaves the only previous install here. Detach before returning:
                    // dropping Prepared must never erase it, including when reporting the error in the UI.
                    if stage.join("previous").exists() {
                        self.keep_stage();
                    }
                    return Err(error);
                }
            }
            Format::AppImage => fs::rename(&self.candidate, &installation.root).map_err(|error| {
                crate::error(format!(
                    "The AppImage couldn't be replaced ({error}). The installed version is unchanged."
                ))
            })?,
            Format::Deb | Format::Rpm | Format::Pacman => self.install_package()?,
        }
        self.keep_stage();
        Ok(self.installation.executable.clone())
    }

    fn keep_stage(&mut self) {
        if let Some(work) = self.work.take() {
            let _ = work.keep();
        }
    }

    fn install_package(&self) -> Result<(), Error> {
        let kind = self
            .installation
            .format
            .system_package()
            .ok_or_else(|| error("This installation isn't a system package."))?;
        match privileged::prompt(&self.installation, kind, &self.candidate, &self.sha256, &self.identity)? {
            Prompted::Installed => Ok(()),
            Prompted::Cancelled => Err(Error::Cancelled),
            Prompted::Unavailable => Err(error(format!(
                "This desktop has no password prompt for installing packages. Download {} from {} \
                 and install it with your package manager.",
                self.installation.format.asset(self.version),
                crate::RELEASES_URL
            ))),
        }
    }
}

/// The installed folder `root` moved into the stage `work` and the unpacked `candidate` moved into its place, both by a
/// rename on one filesystem; the previous one put back when the second rename fails.
fn swap(
    root: &Path,
    candidate: &Path,
    work: &Path,
    rename: &mut impl FnMut(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), Error> {
    let previous = work.join("previous");
    // Establish recovery protection before moving the only installed copy. If the marker can't be
    // written, nothing moves; cleanup can never erase a previous install after a double failure.
    let recovery = work.join(crate::stage::RECOVERY);
    fs::write(&recovery, b"")?;
    rename(root, &previous).map_err(|error| {
        crate::error(format!(
            "The installed folder couldn't be moved aside ({error}). The installed version is unchanged."
        ))
    })?;
    if let Err(failed) = rename(candidate, root) {
        if rename(&previous, root).is_ok() {
            return Err(crate::error(format!(
                "The update couldn't be put in place ({failed}). The installed version was put back."
            )));
        }
        return Err(crate::error(format!(
            "The update couldn't be put in place ({failed}), nor the installed version put back: it is in {}.",
            previous.display()
        )));
    }
    // If removing the marker fails, retaining the old stage is safe; the install still succeeded.
    let _ = fs::remove_file(recovery);
    Ok(())
}
