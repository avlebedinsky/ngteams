//! Finds the WPE `WebKit` the -sys crates link (pkg-config `wpe-webkit-2.0`, through `scripts/pkg-config.sh`: the
//! system's, else the vendored package of `scripts/fetch-wpe.sh`) and tells the crate where its libraries are:
//!
//! - `TEAMS_WPE_LIBDIR`: the library directory, which the launcher (`src/launcher.rs`) overlays on `/usr/lib` when it
//!   is a vendored prefix, whose helper processes sit at a path compiled into the library;
//! - an rpath to every library directory pkg-config reports, for this crate's own executables (its unit and
//!   integration tests; a link argument does not reach dependents): a vendored prefix lies outside the loader's search
//!   path, and pkg-config leaves the system ones out;
//! - the same directories, `:`-separated, as the `libdirs` metadata a dependent's build script reads
//!   (`DEP_TEAMS_WPE_LIBDIRS`, the `links` key's), to give its own executables the rpath.
//!
//! Linking itself is the -sys crates' business: nothing here adds a library.

fn main() {
    println!("cargo::rerun-if-env-changed=PKG_CONFIG_PATH");
    println!("cargo::rerun-if-env-changed=PKG_CONFIG");
    if std::env::var_os("DOCS_RS").is_some() {
        println!("cargo::rustc-env=TEAMS_WPE_LIBDIR=");
        return;
    }
    let library = pkg_config::Config::new()
        .cargo_metadata(false)
        .env_metadata(false)
        .probe("wpe-webkit-2.0");
    let libdir = pkg_config::get_variable("wpe-webkit-2.0", "libdir");
    match (library, libdir) {
        (Ok(library), Ok(libdir)) => {
            for path in &library.link_paths {
                println!("cargo::rustc-link-arg=-Wl,-rpath,{}", path.display());
            }
            let libdirs: Vec<String> = library
                .link_paths
                .iter()
                .map(|path| path.display().to_string())
                .collect();
            println!("cargo::metadata=libdirs={}", libdirs.join(":"));
            println!("cargo::rustc-env=TEAMS_WPE_LIBDIR={libdir}");
        }
        (Err(err), _) | (_, Err(err)) => println!(
            "cargo::error=teams-wpe needs WPE WebKit 2.54 or newer ({}): install the distribution's wpewebkit or run \
             scripts/fetch-wpe.sh (README.md, WPE WebKit)",
            err.to_string().replace('\n', " ")
        ),
    }
}
