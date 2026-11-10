//! A verified update put in the installation's place.

use std::fs;
use std::path::{Path, PathBuf};

use crate::privileged::{self, Prompted};
use crate::{Error, Format, Installation, Version, error};

/// A downloaded and verified update, waiting in its stage; the installation is untouched until [`Prepared::install`].
/// Dropped, its stage goes too.
#[derive(Debug)]
pub struct Prepared {
    pub(crate) installation: Installation,
    /// The stage; kept on disk once the update is in ([`Prepared::install`]).
    pub(crate) work: Option<tempfile::TempDir>,
    /// The unpacked tree, the AppImage or the package.
    pub(crate) candidate: PathBuf,
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
    /// has no prompt, or a step failed; the installation is then as it was.
    pub fn install(&mut self) -> Result<PathBuf, Error> {
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
            Format::Archive => swap(&installation.root, &self.candidate, &stage)?,
            Format::AppImage => fs::rename(&self.candidate, &installation.root).map_err(|error| {
                crate::error(format!(
                    "The AppImage couldn't be replaced ({error}). The installed version is unchanged."
                ))
            })?,
            Format::Deb | Format::Rpm | Format::Pacman => self.install_package()?,
        }
        if let Some(work) = self.work.take() {
            let _ = work.keep();
        }
        Ok(self.installation.executable.clone())
    }

    fn install_package(&self) -> Result<(), Error> {
        let kind = self
            .installation
            .format
            .system_package()
            .ok_or_else(|| error("This installation isn't a system package."))?;
        match privileged::prompt(&self.installation, kind, &self.candidate, &self.identity)? {
            Prompted::Installed => Ok(()),
            Prompted::Cancelled => Err(Error::Cancelled),
            Prompted::Unavailable => Err(error(format!(
                "This desktop has no password prompt for installing packages. Download {} from the releases page \
                 and install it with your package manager.",
                self.installation.format.asset(self.version)
            ))),
        }
    }
}

/// The installed folder `root` moved into the stage `work` and the unpacked `candidate` moved into its place, both by a
/// rename on one filesystem; the previous one put back when the second rename fails.
fn swap(root: &Path, candidate: &Path, work: &Path) -> Result<(), Error> {
    let previous = work.join("previous");
    fs::rename(root, &previous).map_err(|error| {
        crate::error(format!(
            "The installed folder couldn't be moved aside ({error}). The installed version is unchanged."
        ))
    })?;
    if let Err(failed) = fs::rename(candidate, root) {
        if fs::rename(&previous, root).is_ok() {
            return Err(crate::error(format!(
                "The update couldn't be put in place ({failed}). The installed version was put back."
            )));
        }
        let _ = fs::write(work.join(crate::stage::RECOVERY), b"");
        return Err(crate::error(format!(
            "The update couldn't be put in place ({failed}), nor the installed version put back: it is in {}.",
            previous.display()
        )));
    }
    Ok(())
}
