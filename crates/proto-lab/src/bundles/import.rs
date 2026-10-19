//! `proto-lab bundles import <msix>`: unpack the Teams web app from the Windows MSIX into the lab's
//! `bundles/<version>/` and index it.
//!
//! Only `desktop-assets/` (the offline React web app and its serve configs) and `client-bindings/` (the JS bindings
//! to the native media stack) are unpacked; the native binaries stay in the package. Files ending in `.gz` are
//! gunzipped and lose the suffix, everything else is copied. Nothing is executed and no file gets an executable
//! bit: every file is written by us from the entry's bytes, never through an archive extractor that would restore
//! modes or links. The version is the `Version` of the manifest's `<Identity>`.
//!
//! The unpacking goes through `<version>.staging` next to the result, which is renamed into place only once the index
//! is written, so a failed or interrupted import never looks finished. The bundle is Microsoft code: it lives under
//! the lab data dir, never in the repo.

use std::fs;
use std::io::{self, Read as _};
use std::path::{Component, Path, PathBuf};

use flate2::read::MultiGzDecoder;
use zip::ZipArchive;

use super::index::{INDEX_FILE, Index, IndexError, parse_version};

/// The package directories that are unpacked.
pub const UNPACKED_DIRS: [&str; 2] = ["desktop-assets/", "client-bindings/"];
const MANIFEST: &str = "AppxManifest.xml";
/// A manifest bigger than this isn't one.
const MANIFEST_LIMIT: u64 = 4 << 20;

/// An imported bundle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bundle {
    pub version: String,
    pub dir: PathBuf,
}

/// What [`import`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Imported {
    /// Unpacked and indexed now.
    Fresh(Bundle),
    /// This version was already imported; nothing was touched.
    Present(Bundle),
}

/// Why the import failed.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("not a readable MSIX: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("the package has no {MANIFEST} with an <Identity Version=\"…\">")]
    NoVersion,
    #[error("the manifest version {0:?} isn't one to four dot-separated numbers")]
    BadVersion(String),
    #[error("the package entry {0:?} would land outside the bundle dir")]
    UnsafePath(String),
    #[error("can't gunzip the package entry {entry:?}: {source}")]
    Gunzip { entry: String, source: io::Error },
    #[error(transparent)]
    Index(#[from] IndexError),
}

fn io_at(path: &Path) -> impl FnOnce(io::Error) -> ImportError {
    let path = path.to_path_buf();
    move |source| ImportError::Io { path, source }
}

/// Imports the MSIX at `msix` into `bundles_dir/<version>/`. A version already imported (its index is there) is
/// left alone.
///
/// # Errors
/// When the package can't be read, has no valid version, holds an entry that would escape the bundle dir or a `.gz`
/// that isn't gzip, or a file can't be written.
pub fn import(msix: &Path, bundles_dir: &Path) -> Result<Imported, ImportError> {
    let file = fs::File::open(msix).map_err(io_at(msix))?;
    let mut archive = ZipArchive::new(file)?;
    let version = manifest_version(&mut archive)?;
    let dir = bundles_dir.join(&version);
    let bundle = Bundle {
        version: version.clone(),
        dir: dir.clone(),
    };
    if dir.join(INDEX_FILE).is_file() {
        return Ok(Imported::Present(bundle));
    }
    fs::create_dir_all(bundles_dir).map_err(io_at(bundles_dir))?;
    let staging = bundles_dir.join(format!("{version}.staging"));
    remove_dir_if_present(&staging)?;
    if let Err(error) = unpack_and_index(&mut archive, &staging) {
        // the cause of the failure is what matters; a staging dir left behind is removed by the next import
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    remove_dir_if_present(&dir)?;
    fs::rename(&staging, &dir).map_err(io_at(&dir))?;
    Ok(Imported::Fresh(bundle))
}

fn manifest_version<R: io::Read + io::Seek>(archive: &mut ZipArchive<R>) -> Result<String, ImportError> {
    let mut text = String::new();
    match archive.by_name(MANIFEST) {
        Ok(entry) => {
            entry
                .take(MANIFEST_LIMIT)
                .read_to_string(&mut text)
                .map_err(io_at(Path::new(MANIFEST)))?;
        }
        Err(zip::result::ZipError::FileNotFound) => return Err(ImportError::NoVersion),
        Err(error) => return Err(error.into()),
    }
    let version = identity_version(&text).ok_or(ImportError::NoVersion)?;
    if parse_version(version).is_none() {
        return Err(ImportError::BadVersion(version.to_owned()));
    }
    Ok(version.to_owned())
}

/// The `Version="…"` attribute of the first `<Identity …>` tag.
fn identity_version(manifest: &str) -> Option<&str> {
    let mut rest = manifest;
    let tag = loop {
        let at = rest.find("<Identity")?;
        let after = &rest[at + "<Identity".len()..];
        if after.starts_with(|c: char| c.is_whitespace() || c == '/' || c == '>') {
            break &after[..after.find('>')?];
        }
        rest = after;
    };
    let mut attributes = tag;
    loop {
        let at = attributes.find("Version=\"")?;
        let preceded_by_space = attributes[..at].ends_with(char::is_whitespace);
        let value = &attributes[at + "Version=\"".len()..];
        if preceded_by_space {
            return value.split('"').next();
        }
        attributes = value;
    }
}

fn unpack_and_index<R: io::Read + io::Seek>(archive: &mut ZipArchive<R>, staging: &Path) -> Result<(), ImportError> {
    fs::create_dir_all(staging).map_err(io_at(staging))?;
    for number in 0..archive.len() {
        let entry = archive.by_index(number)?;
        let name = entry.name()?.into_owned();
        if entry.is_dir() || !UNPACKED_DIRS.iter().any(|dir| name.starts_with(dir)) {
            continue;
        }
        let relative = safe_relative(&name).ok_or_else(|| ImportError::UnsafePath(name.clone()))?;
        let (relative, gzipped) = match relative.strip_suffix(".gz") {
            Some(stripped) => (stripped, true),
            None => (relative, false),
        };
        let target = staging.join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(io_at(parent))?;
        }
        let mut out = fs::File::create(&target).map_err(io_at(&target))?;
        if gzipped {
            io::copy(&mut MultiGzDecoder::new(entry), &mut out)
                .map_err(|source| ImportError::Gunzip { entry: name, source })?;
        } else {
            let mut entry = entry;
            io::copy(&mut entry, &mut out).map_err(io_at(&target))?;
        }
    }
    let index = Index::build(staging)?;
    index.write(&staging.join(INDEX_FILE))?;
    Ok(())
}

/// The entry name as a relative path of plain segments, or `None` if it has `..`, `.`, empty segments, a
/// backslash, a drive or a leading `/`.
fn safe_relative(name: &str) -> Option<&str> {
    let plain = !name.contains('\\')
        && !name.contains(':')
        && name
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
        && Path::new(name)
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    plain.then_some(name)
}

fn remove_dir_if_present(path: &Path) -> Result<(), ImportError> {
    match fs::remove_dir_all(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(io_at(path)(error)),
        _ => Ok(()),
    }
}
