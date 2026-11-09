//! Which WPE `WebKit` a worker runs on, and what that takes on the worker's command alone: the app and whatever else
//! it starts never see a runtime's library and module paths, and no second process can delete a runtime in use (a
//! shared lock on its `.in-use` file, inherited across the worker's exec).

use std::path::PathBuf;
use std::process::Command;

/// The engine installation selected before a worker starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineSource {
    /// The system's engine: the loader's own search path finds it.
    System,
    /// A complete, verified runtime installation in this directory.
    Runtime(PathBuf),
}

/// Runtime lock inherited by the worker alone; it restores close-on-exec before starting helpers.
#[doc(hidden)]
pub const RUNTIME_LOCK_ENV: &str = "TEAMS_WPE_RUNTIME_LOCK_FD";

impl EngineSource {
    pub(crate) fn configure(&self, command: &mut Command) {
        let Self::Runtime(root) = self else {
            // as the system probe: only the installed library search path may select the engine
            command.env_remove("LD_LIBRARY_PATH");
            command.env_remove(RUNTIME_LOCK_ENV);
            return;
        };
        for (name, relative) in [
            ("LD_LIBRARY_PATH", "lib"),
            ("GIO_MODULE_DIR", "lib/gio/modules"),
            ("GST_PLUGIN_SYSTEM_PATH_1_0", "lib/gstreamer-1.0"),
            ("GST_PLUGIN_SCANNER", "libexec/gstreamer-1.0/gst-plugin-scanner"),
            ("GST_REGISTRY_1_0", "gstreamer-registry.bin"),
            ("WEBKIT_INJECTED_BUNDLE_PATH", "lib/wpe-webkit-2.0/injected-bundle"),
        ] {
            command.env(name, root.join(relative));
        }
        for name in ["GIO_EXTRA_MODULES", "GST_PLUGIN_PATH", "GST_PLUGIN_PATH_1_0"] {
            command.env_remove(name);
        }
    }

    /// Takes the runtime's shared lock before the exec and has the worker keep the same open file description. A take
    /// that would block means a cleanup already claimed the directory: the start fails.
    pub(crate) fn lock_runtime(&self, command: &mut Command) -> std::io::Result<()> {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::process::CommandExt as _;
        let Self::Runtime(root) = self else { return Ok(()) };
        let lock = std::fs::File::open(root.join(".in-use"))?;
        lock.try_lock_shared().map_err(std::io::Error::from)?;
        validate_runtime_lock(root, &lock)?;
        command.env(RUNTIME_LOCK_ENV, lock.as_raw_fd().to_string());
        let inherit_lock = move || {
            // SAFETY: fcntl only changes the child's flags on this open descriptor.
            if unsafe { libc::fcntl(lock.as_raw_fd(), libc::F_SETFD, 0) } < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        };
        // SAFETY: only the async-signal-safe fcntl runs after the fork. Capturing the file keeps it open through the
        // spawn; clearing close-on-exec only in the child hands its shared lock to the running worker.
        unsafe { command.pre_exec(inherit_lock) };
        Ok(())
    }
}

/// A cleanup may have unlinked this file before the lock was taken, and the same key been reinstalled at the path: the
/// inode tells.
fn validate_runtime_lock(root: &std::path::Path, lock: &std::fs::File) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt as _;
    let held = lock.metadata()?;
    let installed = std::fs::symlink_metadata(root.join(".in-use"))?;
    if held.dev() != installed.dev() || held.ino() != installed.ino() {
        return Err(std::io::Error::other("the web engine runtime changed before launch"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::process::Stdio;

    #[test]
    fn a_runtime_sets_the_workers_paths_and_clears_what_would_mix_in_the_systems() {
        let mut command = Command::new("/bin/true");
        command.env("GST_PLUGIN_PATH", "/elsewhere");
        EngineSource::Runtime("/r".into()).configure(&mut command);
        let envs: Vec<_> = command.get_envs().collect();
        let value = |name: &str| {
            envs.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.map(|value| value.to_string_lossy().into_owned()))
        };
        assert_eq!(value("LD_LIBRARY_PATH"), Some(Some("/r/lib".into())));
        assert_eq!(value("GIO_MODULE_DIR"), Some(Some("/r/lib/gio/modules".into())));
        assert_eq!(value("GST_PLUGIN_PATH"), Some(None));
        assert_eq!(
            value("WEBKIT_INJECTED_BUNDLE_PATH"),
            Some(Some("/r/lib/wpe-webkit-2.0/injected-bundle".into()))
        );
    }

    #[test]
    fn the_system_source_clears_the_loaders_path_and_takes_no_lock() {
        let mut command = Command::new("/bin/true");
        command.env("LD_LIBRARY_PATH", "/elsewhere");
        EngineSource::System.configure(&mut command);
        assert!(EngineSource::System.lock_runtime(&mut command).is_ok());
        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == "LD_LIBRARY_PATH" && value.is_none())
        );
    }

    #[test]
    fn the_worker_inherits_the_shared_lock_until_it_exits() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(".in-use");
        File::create_new(&path).unwrap();
        let mut command = Command::new("/bin/cat");
        command.env_clear().stdin(Stdio::piped()).stdout(Stdio::null());
        EngineSource::Runtime(root.path().to_owned())
            .lock_runtime(&mut command)
            .unwrap();
        let mut child = command.spawn().unwrap();
        // the parent no longer owns any handle of the shared lock
        drop(command);
        let other = File::options().read(true).write(true).open(path).unwrap();
        let busy = other.try_lock().is_err();
        drop(child.stdin.take());
        assert!(child.wait().unwrap().success());
        assert!(busy, "the exec'd worker must hold the runtime's shared lock");
        assert!(other.try_lock().is_ok());
    }

    #[test]
    fn an_unlinked_lock_cannot_protect_a_reinstalled_runtime_at_the_same_path() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(".in-use");
        let stale = File::create_new(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let current = File::create_new(&path).unwrap();
        assert!(validate_runtime_lock(root.path(), &stale).is_err());
        assert!(validate_runtime_lock(root.path(), &current).is_ok());
    }

    #[test]
    fn a_runtime_a_cleanup_holds_cannot_start() {
        let root = tempfile::tempdir().unwrap();
        let lock = File::create_new(root.path().join(".in-use")).unwrap();
        lock.lock().unwrap();
        let mut command = Command::new("/bin/true");
        assert!(
            EngineSource::Runtime(root.path().to_owned())
                .lock_runtime(&mut command)
                .is_err()
        );
    }
}
