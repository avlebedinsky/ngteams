//! Native callback error paths through generated COM interfaces, with no page, tenant or visible window.
use super::*;
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use windows::Win32::Foundation::{E_FAIL, E_NOTIMPL};
use windows::Win32::UI::WindowsAndMessaging::{PM_REMOVE, PeekMessageW, WM_QUIT};
use windows::core::{BOOL, PCWSTR, implement};

#[expect(
    clippy::ref_as_ptr,
    clippy::inline_always,
    reason = "windows-rs generated COM ABI implementations"
)]
mod com {
    use super::*;
    #[implement(ICoreWebView2DownloadStartingEventArgs, ICoreWebView2NavigationCompletedEventArgs)]
    pub(super) struct Event {
        pub(super) cancelled: Rc<Cell<bool>>,
        pub(super) handled: Rc<Cell<bool>>,
        pub(super) success: bool,
        pub(super) status: COREWEBVIEW2_WEB_ERROR_STATUS,
        pub(super) fail: bool,
        pub(super) status_read: Rc<Cell<bool>>,
    }
    impl ICoreWebView2DownloadStartingEventArgs_Impl for Event_Impl {
        fn DownloadOperation(&self) -> ComResult<ICoreWebView2DownloadOperation> {
            Err(E_NOTIMPL.into())
        }
        fn Cancel(&self, _value: *mut BOOL) -> ComResult<()> {
            Err(E_NOTIMPL.into())
        }
        fn ResultFilePath(&self, _value: *mut PWSTR) -> ComResult<()> {
            Err(E_NOTIMPL.into())
        }
        fn SetResultFilePath(&self, _value: &PCWSTR) -> ComResult<()> {
            Err(E_NOTIMPL.into())
        }
        fn Handled(&self, _value: *mut BOOL) -> ComResult<()> {
            Err(E_NOTIMPL.into())
        }
        fn GetDeferral(&self) -> ComResult<ICoreWebView2Deferral> {
            Err(E_NOTIMPL.into())
        }
        fn SetCancel(&self, value: BOOL) -> ComResult<()> {
            if self.fail {
                return Err(E_FAIL.into());
            }
            self.cancelled.set(value.as_bool());
            Ok(())
        }
        fn SetHandled(&self, value: BOOL) -> ComResult<()> {
            self.handled.set(value.as_bool());
            Ok(())
        }
    }
    impl ICoreWebView2NavigationCompletedEventArgs_Impl for Event_Impl {
        fn IsSuccess(&self, value: *mut BOOL) -> ComResult<()> {
            if self.fail {
                return Err(E_FAIL.into());
            }
            // SAFETY: the generated handler supplies a live writable BOOL output to this synchronous call.
            unsafe {
                *value = self.success.into();
            }
            Ok(())
        }
        fn WebErrorStatus(&self, value: *mut COREWEBVIEW2_WEB_ERROR_STATUS) -> ComResult<()> {
            self.status_read.set(true);
            // SAFETY: the generated handler supplies the writable status output for this synchronous call.
            unsafe {
                *value = self.status;
            }
            Ok(())
        }
        fn NavigationId(&self, _value: *mut u64) -> ComResult<()> {
            Err(E_NOTIMPL.into())
        }
    }
}

#[test]
fn downloads_are_cancelled_and_failed_main_frame_loads_query_the_native_status() {
    // the load failures reported: a refused (cancelled) navigation keeps its page and reports none
    for (success, fail, status, reported) in [
        (true, false, COREWEBVIEW2_WEB_ERROR_STATUS_UNKNOWN, None),
        (
            false,
            false,
            COREWEBVIEW2_WEB_ERROR_STATUS_UNKNOWN,
            Some(COREWEBVIEW2_WEB_ERROR_STATUS_UNKNOWN.0),
        ),
        (false, false, COREWEBVIEW2_WEB_ERROR_STATUS_OPERATION_CANCELED, None),
        (true, true, COREWEBVIEW2_WEB_ERROR_STATUS_UNKNOWN, None),
    ] {
        let cancelled = Rc::new(Cell::new(false));
        let handled = Rc::new(Cell::new(false));
        let status_read = Rc::new(Cell::new(false));
        let args: ICoreWebView2DownloadStartingEventArgs = com::Event {
            cancelled: Rc::clone(&cancelled),
            handled: Rc::clone(&handled),
            success,
            status,
            fail,
            status_read: Rc::clone(&status_read),
        }
        .into();
        // SAFETY: the fake event belongs to this thread and only records cancellation and handling.
        assert_ne!(unsafe { download_handler().Invoke(None, &args) }.is_ok(), fail);
        assert_eq!(cancelled.get(), !fail);
        assert_eq!(handled.get(), !fail);
        let completed = args.cast::<ICoreWebView2NavigationCompletedEventArgs>().unwrap();
        let failures = Rc::new(std::cell::RefCell::new(Vec::new()));
        let told = Rc::clone(&failures);
        let loaded = loaded_handler_with(move |status| told.borrow_mut().push(status));
        // SAFETY: the fake completion object supplies only fixed synthetic status outputs.
        assert_ne!(unsafe { loaded.Invoke(None, &completed) }.is_ok(), fail);
        assert_eq!(status_read.get(), !success && !fail);
        assert_eq!(
            *failures.borrow(),
            reported.into_iter().collect::<Vec<_>>(),
            "status {status:?}"
        );
        // SAFETY: the generated handler explicitly accepts missing native event arguments.
        unsafe { download_handler().Invoke(None, None) }.unwrap();
        // SAFETY: same null-argument contract on the navigation handler.
        unsafe { loaded_handler().Invoke(None, None) }.unwrap();
    }
}

#[test]
fn native_process_failure_stops_the_loop_and_invalid_handles_and_text_fail_closed() {
    let failure = Rc::new(Cell::new(None));
    let process = process_handler(Rc::clone(&failure));
    // SAFETY: no event arguments are used, and the handler runs synchronously on the test thread.
    unsafe {
        process.Invoke(None, None).unwrap();
    }
    assert!(failure.get().unwrap().contains("browser process stopped"));
    let mut message = MSG::default();
    // SAFETY: reads/removes only this thread's quit notification; no global desktop input is dispatched.
    assert!(unsafe { PeekMessageW(&raw mut message, None, WM_QUIT, WM_QUIT, PM_REMOVE) }.as_bool());
    stop(&failure, "synthetic protected-response failure");
    assert_eq!(failure.get(), Some("synthetic protected-response failure"));
    // SAFETY: consumes the second quit notification on this same test thread.
    assert!(unsafe { PeekMessageW(&raw mut message, None, WM_QUIT, WM_QUIT, PM_REMOVE) }.as_bool());
    assert!(Parent(HWND::default()).window_handle().is_err());
    assert_eq!(text(|_| Ok(())).unwrap(), "");
    assert_eq!(text(|_| Err(E_FAIL.into())).unwrap_err().code(), E_FAIL);
}
