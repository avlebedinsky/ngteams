//! `WebView2` supports environment and registry overrides, including flags that disable the proxy. Refuse them
//! rather than silently losing the guard's configuration. This process changes no user or machine policy.
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY, REG_SAM_FLAGS,
    RegCloseKey, RegOpenKeyExW, RegQueryInfoKeyW, RegQueryValueExW,
};
use windows::core::HSTRING;

pub(super) fn check() -> Result<(), String> {
    if std::env::vars_os().any(|(key, _)| key.to_string_lossy().to_ascii_uppercase().starts_with("WEBVIEW2_")) {
        return Err("WebView2 environment overrides are incompatible with guarded sign-in".into());
    }
    check_roots(&[HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE])
}

// Keeping the read-only registry walk separate lets offline tests use a disposable application key as their
// root. They exercise native registry results without creating or changing any Windows/Edge policy.
fn check_roots(roots: &[HKEY]) -> Result<(), String> {
    for &root in roots {
        for view in [KEY_WOW64_32KEY, KEY_WOW64_64KEY] {
            for name in [
                "AdditionalBrowserArguments",
                "BrowserExecutableFolder",
                "UserDataFolder",
            ] {
                let path = format!("SOFTWARE\\Policies\\Microsoft\\Edge\\WebView2\\{name}");
                if populated(root, &path, view)? {
                    return Err("WebView2 policy overrides guarded sign-in".into());
                }
            }
            let path = HSTRING::from("SOFTWARE\\Policies\\Microsoft\\Edge");
            let mut key = HKEY::default();
            // SAFETY: read-only policy query into a fresh output handle.
            let result = unsafe { RegOpenKeyExW(root, &path, None, KEY_READ | view, &raw mut key) };
            if result == ERROR_FILE_NOT_FOUND {
                continue;
            }
            if result != ERROR_SUCCESS {
                return Err("cannot verify WebView2 proxy policy".into());
            }
            let refused = [
                "ProxySettings",
                "ProxyMode",
                "ProxyServer",
                "ProxyBypassList",
                "ProxyPacUrl",
            ]
            .iter()
            .any(|name| {
                // SAFETY: only queries whether the policy value exists; no data is read or logged.
                unsafe { RegQueryValueExW(key, &HSTRING::from(*name), None, None, None, None) != ERROR_FILE_NOT_FOUND }
            });
            // SAFETY: key is the uniquely owned result of the successful open above.
            unsafe {
                let _ = RegCloseKey(key);
            }
            if refused {
                return Err("proxy policy overrides guarded sign-in".into());
            }
        }
    }
    Ok(())
}

fn populated(root: HKEY, path: &str, view: REG_SAM_FLAGS) -> Result<bool, String> {
    let mut key = HKEY::default();
    // SAFETY: read-only open with a writable output.
    let result = unsafe { RegOpenKeyExW(root, &HSTRING::from(path), None, KEY_READ | view, &raw mut key) };
    if result == ERROR_FILE_NOT_FOUND {
        return Ok(false);
    }
    if result != ERROR_SUCCESS {
        return Err("cannot verify WebView2 policy".into());
    }
    let mut values = 0;
    // SAFETY: only the count output is requested. The opened handle remains valid until closed below.
    let result = unsafe {
        RegQueryInfoKeyW(
            key,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(&raw mut values),
            None,
            None,
            None,
            None,
        )
    };
    // SAFETY: key is owned and has no other users.
    unsafe {
        let _ = RegCloseKey(key);
    }
    if result != ERROR_SUCCESS {
        return Err("cannot verify WebView2 policy".into());
    }
    Ok(values != 0)
}

#[cfg(test)]
#[path = "../../tests/support/native_policy.rs"]
mod tests;
