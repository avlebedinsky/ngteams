//! Gives this crate's executables (the lab's `teams-signin` and the tests) an rpath to WPE `WebKit`'s library
//! directories (`teams-wpe`'s `libdirs`): a vendored package (scripts/fetch-wpe.sh) lies outside the loader's search
//! path.

fn main() {
    if let Ok(dirs) = std::env::var("DEP_TEAMS_WPE_LIBDIRS") {
        for dir in dirs.split(':').filter(|dir| !dir.is_empty()) {
            println!("cargo::rustc-link-arg=-Wl,-rpath,{dir}");
        }
    }
}
