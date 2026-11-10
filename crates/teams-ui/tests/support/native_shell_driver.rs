//! Bounded native UIA client: Explorer's taskbar/overflow and the fixture process's popup only.
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::RECT,
        System::{
            Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize},
            Variant::VARIANT,
        },
        UI::Accessibility::{
            CUIAutomation8, IUIAutomation6, IUIAutomationElement, TreeScope_Descendants, UIA_NamePropertyId,
        },
    },
    core::Result,
};

pub fn main() -> std::process::ExitCode {
    teams_platform::windows_sandbox::window_allowed_here().expect("authorized driver console");
    // SAFETY: this separate, window-free driver process owns its MTA until it exits.
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.unwrap();
    let result = drive();
    // SAFETY: balances this thread's successful initialization after all owned UIA objects are dropped.
    unsafe { CoUninitialize() };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("native tray driver: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn drive() -> Result<()> {
    let directory = PathBuf::from(std::env::var_os("UI_SANDBOX_DIR").unwrap());
    let fixture: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.join("fixture-process.json")).unwrap()).unwrap();
    let pid = i32::try_from(fixture["id"].as_u64().unwrap()).unwrap();
    restore_after_broadcast(pid);
    // SAFETY: initialized, window-free MTA; client is retained exclusively on this thread.
    let client: IUIAutomation6 = unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER) }?;
    // SAFETY: finite provider deadlines in milliseconds on this owned client.
    unsafe { client.SetConnectionTimeout(500) }?;
    // SAFETY: same owned client, finite transaction deadline.
    unsafe { client.SetTransactionTimeout(1000) }?;
    let taskbar = root(&client, "Shell_TrayWnd")?;
    println!("native taskbar found");
    let label = format!("NGTEAMS SYNTHETIC SHELL {pid}:");
    let icon = if let Ok(icon) = tray_icon(&client, &taskbar, &label) {
        icon
    } else {
        println!("synthetic tray is in overflow");
        if !overflow_visible() {
            let expand = find_hidden(&client, &taskbar)?;
            println!("native overflow toggle found");
            capture(&directory, "expand", &expand)?;
            println!("native overflow opened");
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let found = [
                "TopLevelWindowForOverflowXamlIsland",
                "NotifyIconOverflowWindow",
                "Xaml_WindowedPopupClass",
            ]
            .into_iter()
            .find_map(|class| {
                root(&client, class)
                    .and_then(|root| tray_icon(&client, &root, &label))
                    .ok()
            });
            if let Some(icon) = found {
                break icon;
            }
            assert!(
                Instant::now() < deadline,
                "visible synthetic tray icon in overflow; classes={:?}",
                shell_classes()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    println!("synthetic tray icon found");
    capture(&directory, "tray", &icon)?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Ok(menu) = root(&client, "#32768") {
            // SAFETY: the retained native popup's provider, queried on its owning MTA.
            let owner = unsafe { menu.CurrentProcessId() }?;
            assert_eq!(owner, pid, "only this fixture's menu is driven");
            let open = find(&client, &menu, "Open Teams")?;
            visible(find(&client, &menu, "Quit")?)?;
            capture(&directory, "menu", &menu)?;
            write_rectangle(&directory, "open-item", &open)?;
            return Ok(());
        }
        assert!(Instant::now() < deadline, "owned native tray popup");
        std::thread::sleep(Duration::from_millis(50));
    }
}

// Remove precisely this fixture's real tray item, then send the broadcast synchronously. A queued-only message
// loop cannot restore it. Explorer and every other process are left running.
fn restore_after_broadcast(pid: i32) {
    use windows_sys::Win32::UI::{
        Shell::{NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW},
        WindowsAndMessaging::{FindWindowW, GetWindowThreadProcessId, RegisterWindowMessageW, SendMessageW},
    };
    let wide = |text: &str| text.encode_utf16().chain(std::iter::once(0)).collect::<Vec<_>>();
    // SAFETY: fixed class name of the synthetic client's native tray, no window title is searched.
    let hwnd = unsafe { FindWindowW(wide("NgteamsDesktopTray").as_ptr(), std::ptr::null()) };
    assert!(!hwnd.is_null());
    let mut owner = 0;
    // SAFETY: read-only owner query of that HWND into an initialized output buffer.
    unsafe { GetWindowThreadProcessId(hwnd, &raw mut owner) };
    assert_eq!(
        owner,
        u32::try_from(pid).unwrap(),
        "never modify another process's tray item"
    );
    let data = NOTIFYICONDATAW {
        cbSize: u32::try_from(std::mem::size_of::<NOTIFYICONDATAW>()).unwrap(),
        hWnd: hwnd,
        uID: 1, // NativeTray's single icon, owned by the verified fixture PID above.
        ..Default::default()
    };
    // SAFETY: only the verified synthetic fixture's icon is withdrawn, using its HWND/ID pair.
    assert_ne!(unsafe { Shell_NotifyIconW(NIM_DELETE, &raw const data) }, 0);
    // SAFETY: fixed terminated system broadcast-message name.
    let message = unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) };
    assert_ne!(message, 0);
    // SAFETY: synchronous delivery only to that same verified fixture HWND, no borrowed pointer arguments.
    unsafe { SendMessageW(hwnd, message, 0, 0) };
    println!("synthetic tray withdrawn; synchronous restart broadcast delivered");
}

fn tray_icon(client: &IUIAutomation6, root: &IUIAutomationElement, label: &str) -> Result<IUIAutomationElement> {
    use windows::Win32::UI::Accessibility::PropertyConditionFlags_MatchSubstring;
    // SAFETY: the name contains this run's synthetic label and PID; no real application content is selected.
    let condition = unsafe {
        client.CreatePropertyConditionEx(
            UIA_NamePropertyId,
            &VARIANT::from(label),
            PropertyConditionFlags_MatchSubstring,
        )
    }?;
    // SAFETY: only the retained taskbar/overflow subtree is searched, with finite provider deadlines.
    let icon = unsafe { root.FindFirst(TreeScope_Descendants, &condition) }?;
    visible(icon)
}

fn root(client: &IUIAutomation6, name: &str) -> Result<IUIAutomationElement> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: fixed shell class, no window title or application descendants are searched.
    let hwnd = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW(wide.as_ptr(), std::ptr::null()) };
    // SAFETY: a numeric window handle returned by the OS, queried without changing it.
    if hwnd.is_null() || unsafe { windows_sys::Win32::UI::WindowsAndMessaging::IsWindowVisible(hwnd) } == 0 {
        return Err(windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL));
    }
    // SAFETY: retained provider for precisely that shell HWND, with this client's finite provider deadlines.
    unsafe { client.ElementFromHandle(windows::Win32::Foundation::HWND(hwnd)) }
}

fn overflow_visible() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, IsWindowVisible};
    ["TopLevelWindowForOverflowXamlIsland", "NotifyIconOverflowWindow"]
        .into_iter()
        .any(|name| {
            let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            // SAFETY: fixed shell class name, no window title or application content is searched.
            let hwnd = unsafe { FindWindowW(wide.as_ptr(), std::ptr::null()) };
            // SAFETY: numeric OS handle, read-only visibility query.
            !hwnd.is_null() && unsafe { IsWindowVisible(hwnd) } != 0
        })
}
fn find(client: &IUIAutomation6, root: &IUIAutomationElement, name: &str) -> Result<IUIAutomationElement> {
    // SAFETY: owned name condition, searched only inside a retained shell/owned popup subtree.
    let condition = unsafe { client.CreatePropertyCondition(UIA_NamePropertyId, &VARIANT::from(name)) }?;
    // SAFETY: no desktop-wide descendant scan; provider deadlines apply to this owned client.
    unsafe { root.FindFirst(TreeScope_Descendants, &condition) }
}
fn visible(element: IUIAutomationElement) -> Result<IUIAutomationElement> {
    // SAFETY: read-only visibility query of a retained element on its owning MTA.
    if unsafe { element.CurrentIsOffscreen() }?.as_bool() {
        Err(windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL))
    } else {
        Ok(element)
    }
}
fn rectangle(element: &IUIAutomationElement) -> Result<RECT> {
    // SAFETY: retained element on its owning MTA.
    unsafe { element.CurrentBoundingRectangle() }
}
fn write_rectangle(directory: &std::path::Path, name: &str, element: &IUIAutomationElement) -> Result<()> {
    let bounds = rectangle(element)?;
    assert!(bounds.right > bounds.left && bounds.bottom > bounds.top);
    let value =
        serde_json::json!({"left": bounds.left, "top": bounds.top, "right": bounds.right, "bottom": bounds.bottom});
    std::fs::write(directory.join(format!("{name}.rectangle.json")), value.to_string()).unwrap();
    Ok(())
}
fn capture(directory: &std::path::Path, name: &str, element: &IUIAutomationElement) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut stable = None;
    loop {
        let bounds = rectangle(element)?;
        let key = (bounds.left, bounds.top, bounds.right, bounds.bottom);
        if visible(element.clone()).is_ok() && bounds.right > bounds.left && bounds.bottom > bounds.top {
            if let Some((previous, since)) = stable
                && previous == key
            {
                if Instant::now().duration_since(since) >= Duration::from_millis(250) {
                    break;
                }
            } else {
                stable = Some((key, Instant::now()));
            }
        } else {
            stable = None;
        }
        assert!(Instant::now() < deadline, "settled synthetic shell rectangle {name}");
        std::thread::sleep(Duration::from_millis(50));
    }
    write_rectangle(directory, name, element)?;
    let deadline = Instant::now() + Duration::from_secs(3);
    while !directory.join(format!("{name}.captured")).exists() {
        assert!(Instant::now() < deadline, "synthetic shell capture {name}");
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

// Native class names only, never titles or application content, when the expected shell container is absent.
fn shell_classes() -> Vec<String> {
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM},
        UI::WindowsAndMessaging::{EnumWindows, GetClassNameW},
    };
    unsafe extern "system" fn collect(hwnd: HWND, parameter: LPARAM) -> i32 {
        let mut name = [0_u16; 256];
        // SAFETY: initialized writable class-name buffer, never a window title.
        let length = unsafe { GetClassNameW(hwnd, name.as_mut_ptr(), 256) };
        let length = usize::try_from(length).unwrap_or(0);
        // SAFETY: the exclusive output vector stays alive until synchronous EnumWindows returns.
        let output = unsafe { &mut *(parameter as *mut Vec<String>) };
        output.push(String::from_utf16_lossy(&name[..length]));
        1
    }
    let mut output = Vec::<String>::new();
    // SAFETY: synchronous callback uses only the borrowed vector while it remains exclusively alive here.
    unsafe { EnumWindows(Some(collect), (&raw mut output) as LPARAM) };
    output
}

fn find_hidden(client: &IUIAutomation6, taskbar: &IUIAutomationElement) -> Result<IUIAutomationElement> {
    use windows::Win32::UI::Accessibility::{
        PropertyConditionFlags, PropertyConditionFlags_IgnoreCase, PropertyConditionFlags_MatchSubstring,
    };
    // SAFETY: fixed English shell label, insensitive to the OS's tooltip capitalization.
    let condition = unsafe {
        client.CreatePropertyConditionEx(
            UIA_NamePropertyId,
            &VARIANT::from("hidden"),
            PropertyConditionFlags(PropertyConditionFlags_IgnoreCase.0 | PropertyConditionFlags_MatchSubstring.0),
        )
    }?;
    // SAFETY: searches only Explorer's retained taskbar subtree with finite provider deadlines.
    unsafe { taskbar.FindFirst(TreeScope_Descendants, &condition) }
}
