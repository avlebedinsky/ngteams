//! Shared executable resource build, using the same icon as Linux. Resource 1 is GPUI's window/taskbar icon.
use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub fn build(binary: &str) -> ExitCode {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return ExitCode::SUCCESS;
    }
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
    let directory = manifest.join("../../packaging/windows");
    if let Err(error) = compile(binary, &directory) {
        println!("cargo::error=Windows executable resource: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn compile(binary: &str, directory: &Path) -> Result<(), String> {
    let resource = directory.join("ngteams.rc");
    println!("cargo::rerun-if-changed={}", resource.display());
    println!("cargo::rerun-if-changed={}", directory.join("ngteams.ico").display());
    embed_resource::compile_for(&resource, [binary], embed_resource::ParamsIncludeDirs([directory]))
        .manifest_required()
        .map_err(|error| error.to_string())
}
