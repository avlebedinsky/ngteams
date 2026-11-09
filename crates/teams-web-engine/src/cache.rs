//! Serialized publication, pinned identity and cleanup. Locks are released by the kernel on a crash.

use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Seek as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use crate::{Error, error};
use crate::{archive, complete, runtime_root, transfer, valid_key};

/// Immutable release pins. A mirror can change only the URL, never these integrity checks.
#[derive(Debug, Clone)]
pub struct Pins {
    pub(crate) url: String,
    pub(crate) size: u64,
    pub(crate) sha256: String,
    pub(crate) key: String,
}

/// Download progress, including bytes retained from an interrupted request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub received: u64,
    pub total: u64,
}

impl Pins {
    /// The build's runtime pins, with the optional URL-only mirror override.
    ///
    /// # Errors
    /// Returns an explanation for incomplete or malformed release pins.
    pub fn compiled() -> Result<Option<Self>, Error> {
        Self::from_values(
            option_env!("NGTEAMS_WEB_ENGINE_URL"),
            option_env!("NGTEAMS_WEB_ENGINE_SIZE"),
            option_env!("NGTEAMS_WEB_ENGINE_SHA256"),
            crate::KEY,
            std::env::var("NGTEAMS_WEB_ENGINE_URL").ok().as_deref(),
        )
    }

    /// Pins from their values, as the release build gives them; a test of a caller makes its own this way.
    ///
    /// # Errors
    /// Returns an explanation for incomplete or malformed pins.
    #[doc(hidden)]
    pub fn from_values(
        url: Option<&str>,
        size: Option<&str>,
        hash: Option<&str>,
        key: Option<&str>,
        mirror: Option<&str>,
    ) -> Result<Option<Self>, Error> {
        if [url, size, hash, key].iter().all(Option::is_none) {
            return Ok(None);
        }
        let (Some(url), Some(size), Some(hash), Some(key)) = (url, size, hash, key) else {
            return Err(error(
                "The web engine build pins are incomplete; downloading is unavailable.",
            ));
        };
        let pins = Self {
            url: mirror.unwrap_or(url).to_owned(),
            size: size.parse().map_err(|_| error("Invalid web engine size pin."))?,
            sha256: hash.to_ascii_lowercase(),
            key: key.to_owned(),
        };
        if pins.size == 0
            || pins.size > 1024 * 1024 * 1024
            || hash.len() != 64
            || !hash.bytes().all(|c| c.is_ascii_hexdigit())
            || !valid_key(key)
            || !key.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
        {
            return Err(error("Invalid web engine integrity pins."));
        }
        Ok(Some(pins))
    }

    /// Download (or resume), verify and atomically publish this runtime. No global HTTP timeout.
    /// Concurrent calls serialize and reuse the winner's complete installation.
    ///
    /// # Errors
    /// Returns network, integrity, archive or filesystem errors. Interrupted downloads remain resumable.
    pub fn download(&self, state_dir: &Path, mut progress: impl FnMut(Progress)) -> Result<PathBuf, Error> {
        let root = runtime_root(state_dir);
        let _lock = installer_lock(&root).map_err(|e| space_error(e, self.size))?;
        let destination = root.join(&self.key);
        if complete(&destination) {
            let _ = fs::remove_file(root.join(format!("{}.download", self.key)));
            let _ = cleanup_locked(&root, &self.key);
            progress(Progress {
                received: self.size,
                total: self.size,
            });
            return Ok(destination);
        }
        let archive = root.join(format!("{}.download", self.key));
        transfer::download(self, &archive, &mut progress).map_err(|e| space_error(e, self.size))?;
        let mut file = File::open(&archive)?;
        let mut needed = self.size;
        let result = (|| {
            transfer::verify(self, &mut file)?;
            needed = archive::unpacked_size(&mut file)?;
            self.publish(&root, &mut file)
        })();
        // Filesystem failures retain verified bytes for a retry. Content failures must start fresh.
        if result.is_ok() || matches!(&result, Err(Error::Message(_))) {
            let _ = fs::remove_file(&archive);
        }
        result.map_err(|e| space_error(e, needed))
    }

    /// Install an offline archive with exactly the same size, hash and extraction checks.
    ///
    /// # Errors
    /// Returns integrity, archive or filesystem errors. Builds without pins cannot trust offline archives.
    pub fn install(&self, state_dir: &Path, archive: &Path) -> Result<PathBuf, Error> {
        let root = runtime_root(state_dir);
        let _lock = installer_lock(&root).map_err(|e| space_error(e, self.size))?;
        // An unlinked, create-new 0600 file has no pathname another archive writer can replace or open.
        let mut snapshot = snapshot(self, &root, archive).map_err(|e| space_error(e, self.size))?;
        transfer::verify(self, &mut snapshot)?;
        let needed = archive::unpacked_size(&mut snapshot)?;
        let result = self.publish(&root, &mut snapshot).map_err(|e| space_error(e, needed));
        if result.is_ok() {
            let _ = fs::remove_file(root.join(format!("{}.download", self.key)));
        }
        result
    }

    fn publish(&self, root: &Path, archive: &mut File) -> Result<PathBuf, Error> {
        let destination = root.join(&self.key);
        if complete(&destination) {
            let _ = cleanup_locked(root, &self.key);
            return Ok(destination);
        }
        let partial = root.join(format!("{}.partial", self.key));
        let result = (|| {
            remove_incomplete(&partial)?;
            fs::create_dir(&partial)?;
            archive::unpack(archive, &partial, &self.key)?;
            File::create_new(partial.join(".in-use"))?.sync_all()?;
            if !complete(&partial) {
                return Err(error("The web engine archive is incomplete."));
            }
            remove_incomplete(&destination)?;
            fs::rename(&partial, &destination)?;
            File::open(root)?.sync_all()?;
            // Cleanup is best effort and never invalidates a successful publication.
            let _ = cleanup_locked(root, &self.key);
            Ok(destination)
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&partial);
        }
        result
    }
}

/// Copy at most one byte past the pin, isolating the bytes to verify from concurrent source writers.
pub(crate) fn snapshot(pins: &Pins, root: &Path, archive: &Path) -> Result<File, Error> {
    let mut snapshot = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o600))
        .tempfile_in(root)?
        .into_file();
    let input = File::open(archive).map_err(|e| located(e, archive))?;
    std::io::copy(&mut input.take(pins.size + 1), &mut snapshot)?;
    snapshot.rewind()?;
    Ok(snapshot)
}

pub(crate) fn installer_lock(root: &Path) -> Result<File, Error> {
    fs::create_dir_all(root).map_err(|e| located(e, root))?;
    let lock_path = root.join(".install.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| located(e, &lock_path))?;
    file.lock()?;
    Ok(file)
}

fn remove_incomplete(path: &Path) -> Result<(), Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path)?,
        Ok(_) => fs::remove_file(path)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

/// A filesystem failure the user has to act on, naming the path. A full disk keeps its raw error, which [`space_error`]
/// turns into how much room is needed; either way it stays an `Error::Io`, which keeps a verified download for a retry.
pub(crate) fn located(error: std::io::Error, path: &Path) -> Error {
    if error.raw_os_error() == Some(libc::ENOSPC) {
        return error.into();
    }
    Error::Io(std::io::Error::new(
        error.kind(),
        format!("{}: {error}", path.display()),
    ))
}

pub(crate) fn space_error(error: Error, needed: u64) -> Error {
    if matches!(&error, Error::Io(e) if e.raw_os_error() == Some(libc::ENOSPC)) {
        crate::error(format!(
            "Not enough disk space for the web engine; at least {needed} bytes of free space are needed."
        ))
    } else {
        error
    }
}

/// Remove other runtime keys only while their `.in-use` lock can be taken exclusively.
/// Busy keys survive even when another app installs a newer runtime.
///
/// # Errors
/// Returns cache filesystem errors; busy or unrecognized directories are retained.
pub fn cleanup(state_dir: &Path, keep_key: &str) -> Result<usize, Error> {
    let root = runtime_root(state_dir);
    let _lock = installer_lock(&root)?;
    cleanup_locked(&root, keep_key)
}

fn cleanup_locked(root: &Path, keep_key: &str) -> Result<usize, Error> {
    let mut removed = 0;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(key) = name.to_str() else { continue };
        let kind = entry.file_type()?;
        if kind.is_file()
            && key
                .strip_suffix(".download")
                .is_some_and(|key| key != keep_key && valid_key(key))
        {
            fs::remove_file(entry.path())?;
            removed += 1;
            continue;
        }
        if key == keep_key || !valid_key(key) || !kind.is_dir() {
            continue;
        }
        let Ok(lock) = OpenOptions::new()
            .read(true)
            .write(true)
            .open(entry.path().join(".in-use"))
        else {
            continue;
        };
        if lock.try_lock().is_ok() {
            fs::remove_dir_all(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}
