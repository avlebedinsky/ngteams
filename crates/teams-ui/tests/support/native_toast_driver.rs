//! Native UI Automation client, on its own MTA thread with finite provider deadlines. No desktop-wide searches.
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{E_FAIL, HWND},
        System::{
            Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
            Variant::VARIANT,
        },
        UI::Accessibility::{
            CUIAutomation8, IUIAutomation6, IUIAutomationElement, IUIAutomationInvokePattern,
            PropertyConditionFlags_MatchSubstring, TreeScope_Descendants, UIA_InvokePatternId, UIA_NamePropertyId,
        },
    },
    core::Result,
};
use windows_sys::Win32::{
    Foundation::{HWND as RawHwnd, LPARAM},
    UI::WindowsAndMessaging::{EnumWindows, GetClassNameW, IsWindowVisible},
};

pub(super) async fn click(title: &'static str, action: &'static str) {
    let (reply, result) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = reply.send(drive(title, action));
    });
    result.await.unwrap().expect("native toast activation driver");
}
fn drive(title: &str, action: &str) -> Result<()> {
    let _apartment = super::Apartment::new()?;
    // SAFETY: this fresh MTA thread owns no windows and uses only the native UIA client interfaces.
    let automation: IUIAutomation6 = unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER) }?;
    // SAFETY: valid owned client; bound calls even when another process's accessibility provider is unresponsive.
    unsafe { automation.SetConnectionTimeout(500) }?;
    // SAFETY: the same owned client, using milliseconds.
    unsafe { automation.SetTransactionTimeout(1000) }?;
    let deadline = Instant::now() + Duration::from_secs(12);
    while Instant::now() < deadline {
        for hwnd in notification_windows() {
            if let Ok((target, bounds)) = target(&automation, HWND(hwnd.cast()), title, action) {
                screenshot(&bounds, action);
                // SAFETY: the target was found below our exact synthetic title, never by a global Answer search.
                let pattern: IUIAutomationInvokePattern = unsafe { target.GetCurrentPatternAs(UIA_InvokePatternId) }?;
                // SAFETY: invokes this retained synthetic toast element, not a pointer coordinate.
                return unsafe { pattern.Invoke() };
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(windows::core::Error::from_hresult(E_FAIL))
}
fn target(
    client: &IUIAutomation6,
    hwnd: HWND,
    title: &str,
    action: &str,
) -> Result<(IUIAutomationElement, windows::Win32::Foundation::RECT)> {
    // SAFETY: Windows validates the handle; no window from any application tree is dereferenced directly.
    let root = unsafe { client.ElementFromHandle(hwnd) }?;
    // The shell can expose a single accessible label containing the title and body together.
    // SAFETY: read only the retained shell element's name, classifying fixed OS labels rather than logging it.
    let root_name = unsafe { root.CurrentName() }?.to_string();
    println!(
        "native toast root: {}",
        match root_name.as_str() {
            "New notification" => "notification",
            "Notification Center" => "center",
            "Start" => "start",
            _ => "other",
        }
    );
    // SAFETY: the owned variant is copied into an immutable property condition.
    let condition = unsafe {
        client.CreatePropertyConditionEx(
            UIA_NamePropertyId,
            &VARIANT::from(title),
            PropertyConditionFlags_MatchSubstring,
        )
    }?;
    // SAFETY: search only this shell notification window, requiring our exact synthetic title.
    let mut parent = unsafe { root.FindFirst(TreeScope_Descendants, &condition) }?;
    // SAFETY: retrieves the client's walker, all used on this one thread.
    let walker = unsafe { client.ControlViewWalker() }?;
    for _ in 0..7 {
        // SAFETY: a retained element in the synthetic title's ancestor chain.
        let bounds = unsafe { parent.CurrentBoundingRectangle() }?;
        let width = bounds.right - bounds.left;
        let height = bounds.bottom - bounds.top;
        if (200..=700).contains(&width) && (70..=450).contains(&height) {
            if action == "default" {
                // SAFETY: querying a retained element does not activate it.
                if unsafe { parent.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId) }.is_ok() {
                    return Ok((parent, bounds));
                }
            } else {
                let text = if action == "answer" { "Answer" } else { "Decline" };
                // SAFETY: the condition owns a copy of the fixed label.
                let condition = unsafe { client.CreatePropertyCondition(UIA_NamePropertyId, &VARIANT::from(text)) }?;
                // SAFETY: the search is rooted at an ancestor of the verified synthetic title.
                if let Ok(button) = unsafe { parent.FindFirst(TreeScope_Descendants, &condition) } {
                    return Ok((button, bounds));
                }
            }
        }
        // SAFETY: move only within this retained notification's ancestry; size limits exclude the desktop.
        parent = unsafe { walker.GetParentElement(&parent) }?;
    }
    Err(windows::core::Error::from_hresult(E_FAIL))
}
fn notification_windows() -> Vec<RawHwnd> {
    let mut windows = Vec::new();
    // SAFETY: synchronous enumeration; the mutable vector remains alive until every callback has returned.
    unsafe {
        EnumWindows(Some(collect), (&raw mut windows).cast::<()>() as LPARAM);
    }
    windows
}
unsafe extern "system" fn collect(hwnd: RawHwnd, state: LPARAM) -> i32 {
    let mut class = [0_u16; 256];
    // SAFETY: initialized writable UTF-16 buffer with its exact capacity.
    let size = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), 256) };
    // SAFETY: Windows validates the enumerated handle.
    let visible = unsafe { IsWindowVisible(hwnd) } != 0;
    if visible && String::from_utf16_lossy(&class[..usize::try_from(size).unwrap_or(0)]) == "Windows.UI.Core.CoreWindow"
    {
        // SAFETY: EnumWindows calls synchronously with our original vector pointer and never stores it.
        unsafe { &mut *(state as *mut Vec<RawHwnd>) }.push(hwnd);
    }
    1
}
fn screenshot(bounds: &windows::Win32::Foundation::RECT, action: &str) {
    let root = PathBuf::from(std::env::var_os("UI_SANDBOX_DIR").expect("fixture evidence directory"));
    let path = root.join(format!("{action}.rectangle.json"));
    std::fs::write(
        &path,
        serde_json::json!({"left":bounds.left,"top":bounds.top,"right":bounds.right,"bottom":bounds.bottom})
            .to_string(),
    )
    .unwrap();
    let ack = root.join(format!("{action}.captured"));
    let until = Instant::now() + Duration::from_secs(5);
    while !ack.exists() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        ack.exists(),
        "console helper did not capture the verified synthetic toast"
    );
}
