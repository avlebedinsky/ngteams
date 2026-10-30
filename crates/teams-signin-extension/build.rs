//! Finds WPE WebKit's web process extension library (pkg-config `wpe-web-process-extension-2.0`, through
//! scripts/pkg-config.sh: the system's, else the vendored package of scripts/fetch-wpe.sh) for the module's link: the
//! module calls three of its functions (`webkit_web_page_get_id`, `webkit_uri_request_get_*`), which the web process
//! that loads it has already loaded.

fn main() {
    println!("cargo::rerun-if-env-changed=PKG_CONFIG_PATH");
    println!("cargo::rerun-if-env-changed=PKG_CONFIG");
    if let Err(error) = pkg_config::Config::new()
        .atleast_version("2.54")
        .probe("wpe-web-process-extension-2.0")
    {
        println!(
            "cargo::error=teams-signin-extension needs WPE WebKit 2.54 or newer ({}): install the distribution's \
             wpewebkit or run scripts/fetch-wpe.sh (README.md, WPE WebKit)",
            error.to_string().replace('\n', " ")
        );
    }
}
