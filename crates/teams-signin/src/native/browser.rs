//! `WebView2` lives on this worker's STA. Wiring completes before the first authorize navigation.
//! `WebResourceRequested` filters cover every source (including worker requests). A second response interception
//! installs an additional CSP before any document executes: no socket schemes, workers, plugins or local frames.
//! The document-created script removes the unguarded `WebRTC`/`WebTransport` constructors in every frame before page
//! script can retain them. Chromium's non-proxied UDP, QUIC, DNS prefetch and speculative loads are also disabled.
use super::wire::{self, Event, Start};
use crate::webview_policy::{INIT, response_parameters};
use raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle};
use serde_json::{Value, json};
use std::cell::Cell;
use std::num::NonZeroIsize;
use std::rc::Rc;
use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_WEB_ERROR_STATUS, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
    COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_ALL, GetAvailableCoreWebView2BrowserVersionString, ICoreWebView2,
    ICoreWebView2_4, ICoreWebView2_22, ICoreWebView2Settings3, ICoreWebView2Settings4,
};
use webview2_com::{
    CallDevToolsProtocolMethodCompletedHandler, DevToolsProtocolEventReceivedEventHandler,
    DownloadStartingEventHandler, NavigationCompletedEventHandler, NavigationStartingEventHandler,
    NewWindowRequestedEventHandler, ProcessFailedEventHandler, WebResourceRequestedEventHandler, take_pwstr,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW, GetSystemMetrics,
    MSG, PostQuitMessage, RegisterClassW, SM_CXSCREEN, SM_CYSCREEN, SW_SHOW, ShowWindow, TranslateMessage,
    WINDOW_EX_STYLE, WM_DESTROY, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};
use windows::core::{HSTRING, Interface as _, PCWSTR, PWSTR, Result as ComResult, w};
use wry::dpi::{PhysicalPosition, PhysicalSize};
use wry::{WebView, WebViewBuilder, WebViewBuilderExtWindows as _, WebViewExtWindows as _};

pub(super) fn runtime() -> Result<(), String> {
    let mut version = PWSTR::null();
    // SAFETY: installed runtime query allocates its output; take_pwstr frees it, including on failure.
    let result = unsafe { GetAvailableCoreWebView2BrowserVersionString(PCWSTR::null(), &raw mut version) };
    let version = take_pwstr(version);
    result.map_err(|e| e.to_string())?;
    if version.is_empty() {
        return Err("WebView2 runtime missing".into());
    }
    Ok(())
}

struct Parent(HWND);
impl HasWindowHandle for Parent {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let handle = NonZeroIsize::new(self.0.0 as isize).ok_or(HandleError::Unavailable)?;
        // SAFETY: Parent owns the HWND until after WebView is dropped.
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(Win32WindowHandle::new(handle))) })
    }
}
impl Drop for Parent {
    fn drop(&mut self) {
        // SAFETY: our own top-level window; destruction may already have happened on WM_CLOSE.
        unsafe {
            let _ = DestroyWindow(self.0);
        }
    }
}

// SAFETY: the window procedure owns no borrowed state and delegates unhandled messages to DefWindowProcW.
unsafe extern "system" fn procedure(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if message == WM_DESTROY {
        // SAFETY: called on the window's owning thread.
        unsafe {
            PostQuitMessage(0);
        }
        LRESULT(0)
    } else {
        // SAFETY: unchanged parameters received from Windows.
        unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
    }
}

fn parent() -> ComResult<Parent> {
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{IDI_APPLICATION, LoadIconW};
    // SAFETY: queries this executable's already loaded module, without changing its lifetime.
    let module = unsafe { GetModuleHandleW(None) };
    let icon = module
        .and_then(|module| {
            // SAFETY: Win32 treats MAKEINTRESOURCEW(1) as a resource ID, not a dereferenceable address.
            unsafe { LoadIconW(Some(module.into()), PCWSTR(std::ptr::without_provenance(1))) }
        })
        .or_else(|_| {
            // SAFETY: the stock application icon is an OS-owned shared resource, also used by synthetic harnesses.
            unsafe { LoadIconW(None, IDI_APPLICATION) }
        })
        .unwrap_or_default();
    let class = WNDCLASSW {
        hIcon: icon,
        lpfnWndProc: Some(procedure),
        lpszClassName: w!("NgteamsSignIn"),
        ..Default::default()
    };
    let width = i32::from(super::SIZE.0);
    let height = i32::from(super::SIZE.1);
    // SAFETY: read-only system metrics, in the same coordinate space CreateWindowExW uses for this process.
    let left = (unsafe { GetSystemMetrics(SM_CXSCREEN) } - width).max(0) / 2;
    // SAFETY: as above, for the primary display's height.
    let top = (unsafe { GetSystemMetrics(SM_CYSCREEN) } - height).max(0) / 2;
    // SAFETY: class and its static procedure/name are valid; this is the worker's single GUI thread.
    unsafe {
        RegisterClassW(&raw const class);
    }
    // SAFETY: the registered class and static window procedure remain live until this worker exits.
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class.lpszClassName,
            &HSTRING::from(super::TITLE),
            WS_OVERLAPPEDWINDOW,
            left,
            top,
            width,
            height,
            None,
            None,
            None,
            None,
        )
        .map(Parent)
    }
}

fn query(event: &Event) -> bool {
    let result = wire::send(&mut std::io::stdout().lock(), event)
        .and_then(|()| wire::read::<bool>(&mut std::io::stdin().lock()));
    result.unwrap_or(false)
}

pub(super) fn run(start: &Start) -> Result<(), &'static str> {
    super::policy::check().map_err(
        |_| "WebView2 overrides prevent protected sign-in; remove custom WebView2 and proxy overrides, then try again",
    )?;
    // SAFETY: the worker's main thread has no previous COM apartment; all native objects stay on it.
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok() }
        .map_err(|_| "Windows could not initialize the sign-in window")?;
    teams_platform::windows_desktop::set_app_id(&start.app_id)
        .map_err(|_| "Windows could not set the sign-in identity")?;
    let parent = parent().map_err(|_| "Windows could not open the sign-in window")?;
    let mut context = wry::WebContext::new(Some(start.directory.join("browser")));
    let view = WebViewBuilder::new_with_web_context(&mut context)
        .with_incognito(true)
        .with_devtools(false)
        .with_autoplay(false)
        .with_clipboard(false)
        .with_browser_accelerator_keys(false)
        .with_hotkeys_zoom(false)
        .with_additional_browser_args(format!(
            "--proxy-server={} --proxy-bypass-list=<-loopback> --disable-quic --dns-prefetch-disable \
             --disable-background-networking --disable-component-update --no-pings \
             --force-webrtc-ip-handling-policy=disable_non_proxied_udp \
             --disable-features=Prerender2,SpeculationRulesPrefetch,PreconnectToSearch,NetworkPrediction,WebTransport \
             --disable-blink-features=SharedWorker,BackgroundFetch,PushMessaging",
            start.proxy
        ))
        .with_initialization_script_for_main_only(INIT, false)
        .with_permission_handler(|_| wry::PermissionResponse::Deny)
        .with_drag_drop_handler(|_| true)
        .build_as_child(&parent)
        .map_err(|_| "WebView2 could not open; install or repair the WebView2 Runtime, then try again")?;
    let failed = Rc::new(Cell::new(None));
    wire_view(&view, &failed)
        .map_err(|_| "WebView2 could not protect sign-in requests; update its Runtime and try again")?;
    let webview = view.webview();
    wire_responses(&webview, &failed)
        .map_err(|_| "WebView2 could not protect sign-in responses; update its Runtime and try again")?;
    super::refusals::install(&webview)
        .map_err(|_| "WebView2 could not restrict sign-in permissions; update its Runtime and try again")?;
    network_diagnostics(&webview)
        .map_err(|_| "WebView2 could not monitor the sign-in connection; update its Runtime and try again")?;
    devtools_now(&webview, "Network.enable", &json!({}))
        .map_err(|_| "WebView2 could not monitor the sign-in connection; update its Runtime and try again")?;
    devtools_now(
        &webview,
        "Fetch.enable",
        &json!({"patterns":[{"urlPattern":"*", "requestStage":"Response"}]}),
    )
    .map_err(|_| "WebView2 could not enable protected sign-in responses; update its Runtime and try again")?;
    view.load_url(&start.authorize)
        .map_err(|_| "WebView2 could not open the sign-in page; close it and try again")?;
    pump(&parent, &view)?;
    drop(view);
    if let Some(reason) = failed.get() {
        return Err(reason);
    }
    wire::send(&mut std::io::stdout().lock(), &Event::Closed)
        .map_err(|_| "the sign-in window lost its connection to ngteams")
}

fn pump(parent: &Parent, view: &WebView) -> Result<(), &'static str> {
    let mut shape = None;
    resize(parent, view, &mut shape)?;
    // SAFETY: our top-level window belongs to this GUI thread.
    unsafe {
        let _ = ShowWindow(parent.0, SW_SHOW);
    }
    let mut message = MSG::default();
    loop {
        // SAFETY: message is a writable output, and this worker owns its message loop.
        let result = unsafe { GetMessageW(&raw mut message, None, 0, 0) }.0;
        if result == 0 {
            return Ok(());
        }
        if result < 0 {
            return Err("Windows could not read sign-in window events");
        }
        resize(parent, view, &mut shape)?;
        // SAFETY: unchanged MSG received from GetMessageW on this thread.
        unsafe {
            let _ = TranslateMessage(&raw const message);
        }
        // SAFETY: as above; the procedure owns no borrowed state.
        unsafe {
            DispatchMessageW(&raw const message);
        }
    }
}

fn resize(parent: &Parent, view: &WebView, shape: &mut Option<(i32, i32)>) -> Result<(), &'static str> {
    let mut rect = RECT::default();
    // SAFETY: our window belongs to this thread; rect is a writable output. A closing window may be gone already.
    if unsafe { GetClientRect(parent.0, &raw mut rect) }.is_err() {
        return Ok(());
    }
    let next = (rect.right - rect.left, rect.bottom - rect.top);
    if *shape != Some(next) && next.0 > 0 && next.1 > 0 {
        // Wry owns a child HWND as well as the controller. Resize both, only when the parent's size changes;
        // controller.SetBounds alone leaves the child clipped to its default 200x200 client area.
        view.set_bounds(wry::Rect {
            position: PhysicalPosition::new(0, 0).into(),
            size: PhysicalSize::new(next.0, next.1).into(),
        })
        .map_err(|_| "Windows could not resize the sign-in window")?;
        *shape = Some(next);
    }
    Ok(())
}

fn wire_view(view: &WebView, failed: &Rc<Cell<Option<&'static str>>>) -> ComResult<()> {
    let webview = view.webview();
    settings(&webview)?;
    navigation(&webview)?;
    requests(&webview, &view.environment())?;
    failures(&webview, failed)
}

fn settings(webview: &ICoreWebView2) -> ComResult<()> {
    // SAFETY: all interfaces belong to this STA, and remain live through configuration before navigation.
    let settings = unsafe { webview.Settings() }?;
    // SAFETY: the settings interface was just obtained on this thread.
    unsafe { settings.SetAreHostObjectsAllowed(false) }?;
    // SAFETY: as above; the page has no host IPC bindings.
    unsafe { settings.SetIsWebMessageEnabled(false) }?;
    // SAFETY: as above.
    unsafe { settings.SetAreDefaultContextMenusEnabled(false) }?;
    // SAFETY: as above.
    unsafe { settings.SetAreDefaultScriptDialogsEnabled(false) }?;
    let keys = settings.cast::<ICoreWebView2Settings3>()?;
    // SAFETY: settings is owned by this webview's STA.
    unsafe { keys.SetAreBrowserAcceleratorKeysEnabled(false) }?;
    let autofill = settings.cast::<ICoreWebView2Settings4>()?;
    // SAFETY: as above. No user browser profile or saved credentials are used.
    unsafe { autofill.SetIsGeneralAutofillEnabled(false) }?;
    // SAFETY: as above.
    unsafe { autofill.SetIsPasswordAutosaveEnabled(false) }
}

fn text(read: impl FnOnce(*mut PWSTR) -> ComResult<()>) -> ComResult<String> {
    let mut output = PWSTR::null();
    let result = read(&raw mut output);
    let text = take_pwstr(output);
    result.map(|()| text)
}

fn navigation(webview: &ICoreWebView2) -> ComResult<()> {
    let mut token = 0;
    let nav = NavigationStartingEventHandler::create(Box::new(|_, args| {
        let Some(args) = args else {
            return Ok(());
        };
        // SAFETY: args belongs to this event, and text supplies a writable, zeroed out pointer.
        let uri = text(|out| unsafe { args.Uri(out) })?;
        // SAFETY: the decision is made synchronously before this callback returns.
        unsafe { args.SetCancel(!query(&Event::Navigation(uri))) }
    }));
    // SAFETY: webview and handlers stay on their owning STA; registrations last for the view's lifetime.
    unsafe { webview.add_NavigationStarting(&nav, &raw mut token) }?;
    // SAFETY: same handler also protects every frame navigation.
    unsafe { webview.add_FrameNavigationStarting(&nav, &raw mut token) }?;
    let popup = NewWindowRequestedEventHandler::create(Box::new(|_, args| {
        if let Some(args) = args {
            // SAFETY: marking the live event handled prevents the default external window.
            unsafe { args.SetHandled(true) }?;
        }
        Ok(())
    }));
    // SAFETY: registration on the view's owning thread.
    unsafe { webview.add_NewWindowRequested(&popup, &raw mut token) }
}

fn requests(
    webview: &ICoreWebView2,
    environment: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Environment,
) -> ComResult<()> {
    let environment = environment.clone();
    let handler = WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
        let Some(args) = args else {
            return Ok(());
        };
        // SAFETY: the event owns the request through this synchronous callback.
        let request = unsafe { args.Request() }?;
        // SAFETY: request is live on this STA; text owns the writable out pointer.
        let method = text(|out| unsafe { request.Method(out) })?;
        // SAFETY: as above.
        let url = text(|out| unsafe { request.Uri(out) })?;
        if !query(&Event::Request { method, url }) {
            // SAFETY: the environment is the one that created this view; strings are static, no response stream.
            let response = unsafe {
                environment.CreateWebResourceResponse(None, 403, w!("Forbidden"), w!("Content-Length: 0\r\n"))
            }?;
            // SAFETY: the event still holds the request, so this replacement prevents its network load.
            unsafe { args.SetResponse(&response) }?;
        }
        Ok(())
    }));
    let mut token = 0;
    // SAFETY: registration precedes the first navigation on this STA.
    unsafe { webview.add_WebResourceRequested(&handler, &raw mut token) }?;
    let filtering = webview.cast::<ICoreWebView2_22>()?;
    // SAFETY: the required interface supports all-source filtering. A missing interface fails setup closed.
    unsafe {
        filtering.AddWebResourceRequestedFilterWithRequestSourceKinds(
            w!("*"),
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
            COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_ALL,
        )
    }
}

fn failures(webview: &ICoreWebView2, failed: &Rc<Cell<Option<&'static str>>>) -> ComResult<()> {
    let process = process_handler(Rc::clone(failed));
    let mut token = 0;
    // SAFETY: registration on the view's owning STA.
    unsafe { webview.add_ProcessFailed(&process, &raw mut token) }?;
    let download = download_handler();
    let loaded = loaded_handler();
    // SAFETY: registration on the owning STA before navigation.
    unsafe { webview.add_NavigationCompleted(&loaded, &raw mut token) }?;
    let downloads = webview.cast::<ICoreWebView2_4>()?;
    // SAFETY: registration on the same STA, before navigation.
    unsafe { downloads.add_DownloadStarting(&download, &raw mut token) }
}

fn devtools_now(webview: &ICoreWebView2, method: &str, parameters: &Value) -> Result<(), String> {
    let webview = webview.clone();
    let (method, parameters) = (HSTRING::from(method), HSTRING::from(parameters.to_string()));
    CallDevToolsProtocolMethodCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| {
            // SAFETY: this STA owns the webview, and the callback helper pumps its messages until completion.
            unsafe { webview.CallDevToolsProtocolMethod(&method, &parameters, &handler) }.map_err(Into::into)
        }),
        Box::new(|result, _| result),
    )
    .map_err(|e| e.to_string())
}

fn stop(failed: &Cell<Option<&'static str>>, reason: &'static str) {
    failed.set(Some(reason));
    // SAFETY: called only on the worker's GUI thread, whose message loop receives the quit.
    unsafe {
        PostQuitMessage(0);
    }
}

fn wire_responses(webview: &ICoreWebView2, failed: &Rc<Cell<Option<&'static str>>>) -> ComResult<()> {
    let target = webview.clone();
    let failed = Rc::clone(failed);
    // SAFETY: the receiver belongs to this view and is registered on its STA.
    let receiver = unsafe { webview.GetDevToolsProtocolEventReceiver(w!("Fetch.requestPaused")) }?;
    let handler = DevToolsProtocolEventReceivedEventHandler::create(Box::new(move |_, args| {
        let Some(args) = args else {
            return Ok(());
        };
        // SAFETY: args belongs to the live event; text owns its writable output pointer.
        let parameters = text(|out| unsafe { args.ParameterObjectAsJson(out) })?;
        let parsed = serde_json::from_str::<Value>(&parameters).ok();
        let Some((method, parameters)) = parsed.as_ref().and_then(response_parameters) else {
            stop(
                &failed,
                "WebView2 returned an invalid protected response. Close sign-in and try again.",
            );
            return Ok(());
        };
        let failure = Rc::clone(&failed);
        let done = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |result, _| {
            if result.is_err() {
                stop(
                    &failure,
                    "WebView2 could not resume a protected response. Close sign-in and try again.",
                );
            }
            Ok(())
        }));
        // SAFETY: the paused response belongs to this view; failure closes the complete worker tree.
        unsafe {
            target.CallDevToolsProtocolMethod(&HSTRING::from(method), &HSTRING::from(parameters.to_string()), &done)
        }
    }));
    let mut token = 0;
    // SAFETY: receiver and handler are on the view's owning STA, registered before the first network load.
    unsafe { receiver.add_DevToolsProtocolEventReceived(&handler, &raw mut token) }
}

fn network_diagnostics(webview: &ICoreWebView2) -> ComResult<()> {
    // SAFETY: receiver belongs to this view and is used only on its owning STA.
    let receiver = unsafe { webview.GetDevToolsProtocolEventReceiver(w!("Network.loadingFailed")) }?;
    let handler = DevToolsProtocolEventReceivedEventHandler::create(Box::new(|_, args| {
        let Some(args) = args else {
            return Ok(());
        };
        // SAFETY: text supplies a writable output; the event remains live through the read.
        let parameters = text(|out| unsafe { args.ParameterObjectAsJson(out) })?;
        let value = serde_json::from_str::<Value>(&parameters).ok();
        let code = value
            .as_ref()
            .and_then(|v| v.get("errorText"))
            .and_then(Value::as_str)
            .unwrap_or("");
        // Only Chromium's fixed error code, never a URI, request, response or headers.
        let code = if code.starts_with("net::ERR_")
            && code[8..]
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        {
            code
        } else {
            "unknown network error"
        };
        let _ = wire::send(&mut std::io::stdout().lock(), &Event::NetworkFailure(code.into()));
        Ok(())
    }));
    let mut token = 0;
    // SAFETY: registration on this receiver's owning STA before navigation.
    unsafe { receiver.add_DevToolsProtocolEventReceived(&handler, &raw mut token) }
}

fn process_handler(
    failure: Rc<Cell<Option<&'static str>>>,
) -> webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2ProcessFailedEventHandler {
    ProcessFailedEventHandler::create(Box::new(move |_, _| {
        failure.set(Some(
            "The WebView2 browser process stopped during sign-in. Close it and try again.",
        ));
        // SAFETY: callbacks run on the worker's GUI thread, whose message loop receives the quit.
        unsafe {
            PostQuitMessage(0);
        }
        Ok(())
    }))
}

fn download_handler() -> webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2DownloadStartingEventHandler {
    DownloadStartingEventHandler::create(Box::new(|_, args| {
        if let Some(args) = args {
            // SAFETY: the event is still live and no download may proceed.
            unsafe { args.SetCancel(true) }?;
            // SAFETY: prevent the default download UI as well.
            unsafe { args.SetHandled(true) }?;
        }
        Ok(())
    }))
}

fn loaded_handler() -> webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2NavigationCompletedEventHandler {
    NavigationCompletedEventHandler::create(Box::new(|_, args| {
        let Some(args) = args else {
            return Ok(());
        };
        let mut success = windows::core::BOOL::default();
        // SAFETY: the event's live arguments supply the final main-frame load status.
        unsafe { args.IsSuccess(&raw mut success) }?;
        if !success.as_bool() {
            let mut status = COREWEBVIEW2_WEB_ERROR_STATUS::default();
            // SAFETY: status is a writable output for the live event.
            unsafe { args.WebErrorStatus(&raw mut status) }?;
            let _ = wire::send(&mut std::io::stdout().lock(), &Event::LoadFailed(status.0));
        }
        Ok(())
    }))
}

#[cfg(test)]
#[path = "../../tests/support/native_browser.rs"]
mod tests;
