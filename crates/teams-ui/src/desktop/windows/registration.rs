//! Unpackaged toast identity, registered per user. Only application metadata is stored here.
use std::{io, io::Write as _, path::Path};
use windows_sys::Win32::{
    Foundation::ERROR_SUCCESS,
    System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey, RegCreateKeyExW,
        RegSetValueExW,
    },
};

pub(super) fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

pub(super) struct Registered {
    temporary_shortcut: Option<std::path::PathBuf>,
}
impl Drop for Registered {
    fn drop(&mut self) {
        if let Some(path) = &self.temporary_shortcut {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub(super) fn register(app_id: &str, name: &str) -> io::Result<Registered> {
    if app_id.is_empty()
        || app_id.len() > 128
        || !app_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid toast application identity",
        ));
    }
    let home = std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "LOCALAPPDATA is not absolute"))?;
    let directory = home.join("ngteams-desktop").join(app_id);
    teams_platform::private::create_dir_all(&directory)?;
    let icon = directory.join("icon.png");
    let png = include_bytes!("../../../../../packaging/icons/hicolor/64x64/apps/com.nickalie.ngteams.png");
    let mut file = teams_platform::private::open(&icon, teams_platform::private::Open::ReadWrite)?;
    file.set_len(0)?;
    file.write_all(png)?;
    write_registration(app_id, name, &icon)?;
    shortcut(app_id, name)
}

fn write_registration(app_id: &str, name: &str, icon: &Path) -> io::Result<()> {
    let path = wide(&format!(r"Software\Classes\AppUserModelId\{app_id}"));
    let mut key: HKEY = std::ptr::null_mut();
    // SAFETY: terminated strings and a writable out-pointer. The returned key is closed below.
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            path.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            std::ptr::null(),
            &raw mut key,
            std::ptr::null_mut(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status.cast_signed()));
    }
    let result =
        set_string(key, "DisplayName", name).and_then(|()| set_string(key, "IconUri", &icon.to_string_lossy()));
    // SAFETY: this is the one owned key returned by RegCreateKeyExW.
    unsafe {
        RegCloseKey(key);
    }
    result
}
fn set_string(key: HKEY, name: &str, value: &str) -> io::Result<()> {
    let (name, value) = (wide(name), wide(value));
    let bytes = u32::try_from(value.len() * 2).map_err(io::Error::other)?;
    // SAFETY: open key and terminated UTF-16 buffers alive for this call; length includes NUL.
    let status = unsafe { RegSetValueExW(key, name.as_ptr(), 0, REG_SZ, value.as_ptr().cast(), bytes) };
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(status.cast_signed()))
    }
}

// Legacy desktop toast delivery requires an actual Start-menu shortcut carrying the matching application ID.
// The installer supplies it for installed builds; this also keeps a moved/portable executable's target current.
fn shortcut(app_id: &str, name: &str) -> io::Result<Registered> {
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile};
    use windows::Win32::UI::Shell::{IShellLinkW, PropertiesSystem::IPropertyStore, ShellLink};
    use windows::core::{HSTRING, Interface as _};
    let base = std::env::var_os("APPDATA")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "APPDATA is not absolute"))?;
    let directory = base.join("Microsoft/Windows/Start Menu/Programs");
    std::fs::create_dir_all(&directory)?;
    let temporary = app_id != "com.nickalie.ngteams";
    let filename = if temporary {
        format!("{app_id}.{}.lnk", std::process::id())
    } else {
        "ngteams.lnk".to_owned()
    };
    let path = directory.join(filename);
    // SAFETY: initialized apartment on the owning notification thread; no aggregation.
    let link: IShellLinkW =
        unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) }.map_err(io::Error::other)?;
    let exe = HSTRING::from(std::env::current_exe()?.as_os_str());
    // SAFETY: live COM object and valid, terminated executable path.
    unsafe { link.SetPath(&exe) }.map_err(io::Error::other)?;
    // Non-personal shortcuts exist only to register their notification endpoint, never to start a sending client.
    let args = match app_id {
        "com.nickalie.ngteams.readonly" => "--read-only",
        "com.nickalie.ngteams.lab" => "--lab",
        _ => "",
    };
    // SAFETY: a fixed argument string, copied by the shell link; lab/read-only launches fail closed without a runner.
    unsafe { link.SetArguments(&HSTRING::from(args)) }.map_err(io::Error::other)?;
    // SAFETY: app metadata only, copied by the shell link.
    unsafe { link.SetDescription(&HSTRING::from(name)) }.map_err(io::Error::other)?;
    // SAFETY: the executable's embedded application icon (the shell has a stock fallback in a test fixture).
    unsafe { link.SetIconLocation(&exe, 0) }.map_err(io::Error::other)?;
    let store: IPropertyStore = link.cast().map_err(io::Error::other)?;
    let id = app_id_value(app_id).map_err(io::Error::other)?;
    // Canonical SDK PROPERTYKEY for System.AppUserModel.ID; avoids enabling unrelated EnhancedStorage bindings.
    let key = windows::Win32::Foundation::PROPERTYKEY {
        fmtid: windows::core::GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
        pid: 5,
    };
    // SAFETY: live property store, canonical key and an owned variant whose value is copied by SetValue.
    unsafe { store.SetValue(&raw const key, &raw const id) }.map_err(io::Error::other)?;
    // SAFETY: commits this link's property store before saving the link.
    unsafe { store.Commit() }.map_err(io::Error::other)?;
    let file: IPersistFile = link.cast().map_err(io::Error::other)?;
    // SAFETY: live link persistence interface, valid absolute path; Windows copies the string for the call.
    unsafe { file.Save(&HSTRING::from(path.as_os_str()), true) }.map_err(io::Error::other)?;
    Ok(Registered {
        temporary_shortcut: temporary.then_some(path),
    })
}

fn app_id_value(app_id: &str) -> windows::core::Result<windows::Win32::System::Com::StructuredStorage::PROPVARIANT> {
    use windows::Win32::System::{
        Com::StructuredStorage::{PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0},
        Variant::VT_LPWSTR,
    };
    use windows::{Win32::UI::Shell::SHStrDupW, core::HSTRING};
    // SAFETY: a terminated string; the returned CoTaskMem allocation is owned by the PROPVARIANT below.
    let text = unsafe { SHStrDupW(&HSTRING::from(app_id)) }?;
    Ok(PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_LPWSTR,
                Anonymous: PROPVARIANT_0_0_0 { pwszVal: text },
                ..Default::default()
            }),
        },
    })
}
