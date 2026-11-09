//! Embed the native Windows executable's icon; no resource tool runs for other targets.
#[path = "../../build/windows-icon.rs"]
mod windows_icon;

fn main() -> std::process::ExitCode {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // MSVC hits LNK1140's debug-record limit when the full GUI/media/AI image is linked, even with line tables
        // only. Keep the normal feature set and all runtime diagnostics; omit this image's PDB at the final link.
        // Dependency/test debug information and the Linux executable retain their normal settings.
        println!("cargo::rustc-link-arg-bin=ngteams=/DEBUG:NONE");
    }
    windows_icon::build("ngteams")
}
