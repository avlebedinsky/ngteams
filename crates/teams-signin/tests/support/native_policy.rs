//! All writes are below a fresh synthetic application key, never HKCU/HKLM's actual policy paths.
use super::*;
use windows::Win32::System::Registry::{
    KEY_ALL_ACCESS, REG_OPTION_NON_VOLATILE, REG_SZ, RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW,
};

struct Registry {
    root: HKEY,
    path: HSTRING,
}
impl Registry {
    fn new() -> Self {
        let name = tempfile::Builder::new().prefix("ngteams-registry-").tempdir().unwrap();
        let path = HSTRING::from(format!(
            "Software\\NgteamsSynthetic\\{}",
            name.path().file_name().unwrap().to_string_lossy()
        ));
        Self {
            root: key(HKEY_CURRENT_USER, &path),
            path,
        }
    }
    fn set(&self, path: &str, name: &str) {
        let key = key(self.root, &HSTRING::from(path));
        let bytes: Vec<u8> = "synthetic\0".encode_utf16().flat_map(u16::to_le_bytes).collect();
        // SAFETY: the handle is our uniquely owned synthetic application subkey; the byte slice remains live.
        let result = unsafe { RegSetValueExW(key, &HSTRING::from(name), None, REG_SZ, Some(&bytes)) };
        // SAFETY: this function uniquely owns the successfully opened child key.
        unsafe { RegCloseKey(key) }.ok().unwrap();
        result.ok().unwrap();
    }
}
impl Drop for Registry {
    fn drop(&mut self) {
        // SAFETY: this handle belongs only to this test and is not used again.
        unsafe { RegCloseKey(self.root) }.ok().unwrap();
        // SAFETY: deletion names this test's unique application path, never the actual policy tree.
        unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &self.path) }.ok().unwrap();
    }
}
fn key(root: HKEY, path: &HSTRING) -> HKEY {
    let mut key = HKEY::default();
    // SAFETY: output is writable, and tests only pass their own unique application key or its descendants.
    unsafe {
        RegCreateKeyExW(
            root,
            path,
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_ALL_ACCESS,
            None,
            &raw mut key,
            None,
        )
    }
    .ok()
    .unwrap();
    key
}

#[test]
fn absent_and_empty_native_settings_allow_signin_but_any_override_refuses() {
    let registry = Registry::new();
    assert!(check_roots(&[registry.root]).is_ok());
    for name in [
        "AdditionalBrowserArguments",
        "BrowserExecutableFolder",
        "UserDataFolder",
    ] {
        let registry = Registry::new();
        let path = format!("SOFTWARE\\Policies\\Microsoft\\Edge\\WebView2\\{name}");
        let empty = key(registry.root, &HSTRING::from(&path));
        // SAFETY: test-owned child handle.
        unsafe { RegCloseKey(empty) }.ok().unwrap();
        assert!(!populated(registry.root, &path, KEY_WOW64_64KEY).unwrap());
        assert!(check_roots(&[registry.root]).is_ok());
        registry.set(&path, "synthetic-app");
        assert!(populated(registry.root, &path, KEY_WOW64_64KEY).unwrap());
        assert!(check_roots(&[registry.root]).unwrap_err().contains("WebView2 policy"));
    }
    for name in [
        "ProxySettings",
        "ProxyMode",
        "ProxyServer",
        "ProxyBypassList",
        "ProxyPacUrl",
    ] {
        let registry = Registry::new();
        registry.set("SOFTWARE\\Policies\\Microsoft\\Edge", name);
        assert!(check_roots(&[registry.root]).unwrap_err().contains("proxy policy"));
    }
}

#[test]
fn an_unreadable_native_registry_root_fails_closed() {
    let invalid = HKEY(std::ptr::without_provenance_mut(1));
    assert!(check_roots(&[invalid]).unwrap_err().contains("cannot verify"));
    assert!(
        populated(invalid, "synthetic", KEY_WOW64_64KEY)
            .unwrap_err()
            .contains("cannot verify")
    );
}
