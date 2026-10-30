//! Gives `ngteams` (and its tests) an rpath to WPE `WebKit`'s library directories (`teams-wpe`'s `libdirs`): the
//! sign-in window's engine, whose vendored package (scripts/fetch-wpe.sh) lies outside the loader's search path.

fn main() {
    if let Ok(dirs) = std::env::var("DEP_TEAMS_WPE_LIBDIRS") {
        for dir in dirs.split(':').filter(|dir| !dir.is_empty()) {
            println!("cargo::rustc-link-arg=-Wl,-rpath,{dir}");
        }
    }
}
