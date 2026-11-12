//! Subprocess account state is always disposable. Keep native OS/DLL discovery, never a real profile or app state.
use std::path::Path;
use std::process::Command;

pub fn isolate(child: &mut Command, root: &Path, sandbox: bool) {
    let home = root.join("home");
    let temp = root.join("tmp");
    std::fs::create_dir_all(&temp).unwrap();
    child.env("HOME", &home).env("TEMP", &temp).env("TMP", &temp);
    #[cfg(windows)]
    {
        child.env("USERPROFILE", &home).env("LOCALAPPDATA", root.join("local"));
        for key in [
            "SystemRoot",
            "SystemDrive",
            "ComSpec",
            "ProgramFiles",
            "PATH",
            "LLVM_PROFILE_FILE",
        ] {
            if let Some(value) = std::env::var_os(key) {
                child.env(key, value);
            }
        }
        if sandbox {
            let directory = root.join("sandbox");
            std::fs::create_dir_all(&directory).unwrap();
            child
                .env("NGTEAMS_WINDOWS_VM_SANDBOX", "win11")
                .env("UI_SANDBOX_DIR", directory);
        }
    }
    #[cfg(unix)]
    let _ = sandbox;
}
