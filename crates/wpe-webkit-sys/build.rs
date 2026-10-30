// Generated once by gir (`split_build_rs`, see Gir.toml) and kept by hand since: gir regenerates only
// build_version.rs, which this script does not use (the crate binds no version-gated API).
//
// Probes wpe-webkit-2.0 through pkg-config (scripts/pkg-config.sh: the system's WPE WebKit, else the vendored package
// of scripts/fetch-wpe.sh) and, when there is none, says where one comes from. A vendored WPE lies outside the
// loader's search path: `teams-wpe`'s build script gives the executables that link it an rpath.

fn main() {
    if std::env::var("DOCS_RS").is_ok() {
        // prevent linking libraries to avoid documentation failure
        return;
    }

    if let Err(s) = system_deps::Config::new().probe() {
        println!(
            "cargo::error={}: this crate needs WPE WebKit 2.54 or newer: install the distribution's wpewebkit or run \
             scripts/fetch-wpe.sh (README.md, WPE WebKit)",
            s.to_string().replace('\n', " ")
        );
    }
}
