//! Extract only ordinary files, directories and confined relative symlinks from an authenticated archive.

use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File};
use std::io::{Seek as _, Write as _};
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Component, Path, PathBuf};

use crate::{Error, error};

/// Sum the authenticated tree before allocating any output; every filesystem failure uses this same total.
pub(crate) fn unpacked_size(file: &mut File) -> Result<u64, Error> {
    let mut archive = tar::Archive::new(decoder(file)?);
    let mut needed = 0u64;
    for entry in archive.entries().map_err(invalid_archive)? {
        let entry = entry.map_err(invalid_archive)?;
        if entry.header().entry_type().is_file() {
            needed = needed
                .checked_add(entry.size())
                .ok_or_else(|| error("Web engine archive is too large."))?;
        }
    }
    Ok(needed)
}

fn invalid_archive(e: std::io::Error) -> Error {
    if e.raw_os_error().is_some() {
        e.into()
    } else {
        error(format!("Invalid web engine archive: {e}"))
    }
}

fn decoder(file: &mut File) -> Result<zstd::stream::read::Decoder<'_, std::io::BufReader<&mut File>>, Error> {
    file.rewind()?;
    let mut decoder = zstd::stream::read::Decoder::new(file).map_err(invalid_archive)?;
    decoder.window_log_max(27).map_err(invalid_archive)?;
    Ok(decoder)
}

pub(crate) fn unpack(archive: &mut File, destination: &Path, key: &str) -> Result<(), Error> {
    unpack_with(archive, destination, key, |path| {
        File::create_new(path).map_err(output_error)
    })
}

fn output_error(e: std::io::Error) -> Error {
    if matches!(
        e.kind(),
        std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::NotADirectory
    ) {
        error("The web engine archive contains duplicate or conflicting entries.")
    } else {
        e.into()
    }
}

/// The file opener is injectable so tests exercise an actual extraction write against /dev/full.
pub(crate) fn unpack_with(
    archive: &mut File,
    destination: &Path,
    key: &str,
    mut create_file: impl FnMut(&Path) -> Result<File, Error>,
) -> Result<(), Error> {
    let mut archive = tar::Archive::new(decoder(archive)?);
    let mut links = BTreeMap::new();
    let mut directories = Vec::new();
    for entry in archive.entries().map_err(invalid_archive)? {
        let mut entry = entry.map_err(invalid_archive)?;
        let path = clean_path(&entry.path().map_err(invalid_archive)?)?;
        if path == Path::new(".in-use") {
            return Err(error("The web engine archive contains the reserved runtime lock."));
        }
        let kind = entry.header().entry_type();
        let output = destination.join(&path);
        let mode = entry.header().mode().map_err(invalid_archive)? & 0o777;
        if kind.is_dir() {
            fs::create_dir_all(&output).map_err(output_error)?;
            directories.push((output, mode));
        } else if kind.is_file() && !path.as_os_str().is_empty() {
            fs::create_dir_all(output.parent().ok_or_else(|| error("Missing archive parent."))?)
                .map_err(output_error)?;
            let mut file = create_file(&output)?;
            copy_entry(&mut entry, &mut file)?;
            file.set_permissions(fs::Permissions::from_mode(mode))?;
            file.flush()?;
            file.sync_all()?;
        } else if kind.is_symlink() && !path.as_os_str().is_empty() {
            let target = entry
                .link_name()
                .map_err(invalid_archive)?
                .ok_or_else(|| error("Missing symlink target."))?
                .into_owned();
            if target.is_absolute() || links.insert(path, target).is_some() {
                return Err(error("Invalid web engine archive symlink."));
            }
        } else {
            return Err(error("The web engine archive contains an unsupported entry type."));
        }
    }
    install_links(destination, &links)?;
    validate_manifest(destination, key)?;
    // Apply directory modes last so a read-only directory cannot interrupt extraction.
    for (path, mode) in directories.into_iter().rev() {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

fn copy_entry(reader: &mut impl std::io::Read, writer: &mut impl std::io::Write) -> Result<(), Error> {
    let mut buffer = vec![0; 64 * 1024];
    loop {
        // Keep corrupt input separate from transient output failures when deciding whether to retain downloads.
        let count = reader.read(&mut buffer).map_err(invalid_archive)?;
        if count == 0 {
            return Ok(());
        }
        writer.write_all(&buffer[..count])?;
    }
}

fn clean_path(path: &Path) -> Result<PathBuf, Error> {
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(name) => clean.push(name),
            Component::CurDir => {}
            _ => return Err(error("The web engine archive contains an unsafe path.")),
        }
    }
    Ok(clean)
}

fn install_links(root: &Path, links: &BTreeMap<PathBuf, PathBuf>) -> Result<(), Error> {
    for (path, target) in links {
        if path.ancestors().skip(1).any(|parent| links.contains_key(parent)) {
            return Err(error("An archive symlink is nested under another symlink."));
        }
        confined_target(&path.parent().unwrap_or(Path::new("")).join(target), links)?;
    }
    for (path, target) in links {
        let output = root.join(path);
        fs::create_dir_all(output.parent().ok_or_else(|| error("Missing symlink parent."))?).map_err(output_error)?;
        // Existing files/directories are never replaced; entries below a deferred link fail here too.
        symlink(target, output).map_err(output_error)?;
    }
    Ok(())
}

/// Expand the archive's symlink graph before handling `..`: lexical normalization alone misses chained escapes.
fn confined_target(path: &Path, links: &BTreeMap<PathBuf, PathBuf>) -> Result<(), Error> {
    let mut pending: VecDeque<_> = path.components().map(|part| part.as_os_str().to_owned()).collect();
    let mut resolved = PathBuf::new();
    let mut followed = 0;
    while let Some(part) = pending.pop_front() {
        if part == "." {
            continue;
        }
        if part == ".." {
            if !resolved.pop() {
                return Err(error("An archive symlink escapes the runtime."));
            }
            continue;
        }
        if Path::new(&part).is_absolute() {
            return Err(error("An archive symlink has an absolute target."));
        }
        resolved.push(part);
        if let Some(target) = links.get(&resolved) {
            followed += 1;
            if followed > 40 {
                return Err(error("An archive symlink contains a cycle."));
            }
            resolved.pop();
            for part in target.components().rev() {
                pending.push_front(part.as_os_str().to_owned());
            }
        }
    }
    Ok(())
}

fn validate_manifest(root: &Path, key: &str) -> Result<(), Error> {
    let manifest = fs::read_to_string(root.join("MANIFEST")).map_err(|e| {
        if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidData) {
            error("The web engine archive has no valid MANIFEST.")
        } else {
            e.into()
        }
    })?;
    let value = |name: &str| manifest.lines().find_map(|line| line.strip_prefix(name));
    if value("key=") != Some(key) || value("arch=") != Some(std::env::consts::ARCH) || value("wpe_version=").is_none() {
        return Err(error(
            "The web engine manifest does not match this runtime key and architecture.",
        ));
    }
    Ok(())
}
