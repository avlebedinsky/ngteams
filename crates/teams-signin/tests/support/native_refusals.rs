//! Fake native event objects record cancellation through the real generated COM event-handler ABI.
use super::*;
use std::cell::Cell;
use std::rc::Rc;
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use windows::Win32::Foundation::{E_FAIL, E_NOTIMPL};
use windows::core::{BOOL, PWSTR, Ref, implement};

#[expect(
    clippy::ref_as_ptr,
    clippy::inline_always,
    reason = "windows-rs generated COM ABI implementations"
)]
mod com {
    use super::*;

    #[implement(
        ICoreWebView2BasicAuthenticationRequestedEventArgs,
        ICoreWebView2ClientCertificateRequestedEventArgs,
        ICoreWebView2LaunchingExternalUriSchemeEventArgs
    )]
    pub(super) struct Challenge {
        pub(super) cancelled: Rc<Cell<bool>>,
        pub(super) handled: Rc<Cell<bool>>,
        pub(super) fail: bool,
    }

    macro_rules! unused {
        ($($name:ident($($arg:ident: $ty:ty),*) -> $result:ty);* $(;)?) => {
            $(fn $name(&self, $($arg: $ty),*) -> Result<$result> { Err(E_NOTIMPL.into()) })*
        };
    }
    macro_rules! cancellation {
        () => {
            fn SetCancel(&self, value: BOOL) -> Result<()> {
                if self.fail {
                    return Err(E_FAIL.into());
                }
                self.cancelled.set(value.as_bool());
                Ok(())
            }
        };
    }

    impl ICoreWebView2BasicAuthenticationRequestedEventArgs_Impl for Challenge_Impl {
        unused! {
            Uri(_value: *mut PWSTR) -> ();
            Challenge(_value: *mut PWSTR) -> ();
            Response() -> ICoreWebView2BasicAuthenticationResponse;
            Cancel(_value: *mut BOOL) -> ();
            GetDeferral() -> ICoreWebView2Deferral;
        }
        cancellation!();
    }
    impl ICoreWebView2ClientCertificateRequestedEventArgs_Impl for Challenge_Impl {
        unused! {
            Host(_value: *mut PWSTR) -> ();
            Port(_value: *mut i32) -> ();
            IsProxy(_value: *mut BOOL) -> ();
            AllowedCertificateAuthorities() -> ICoreWebView2StringCollection;
            MutuallyTrustedCertificates() -> ICoreWebView2ClientCertificateCollection;
            SelectedCertificate() -> ICoreWebView2ClientCertificate;
            SetSelectedCertificate(_value: Ref<ICoreWebView2ClientCertificate>) -> ();
            Cancel(_value: *mut BOOL) -> ();
            Handled(_value: *mut BOOL) -> ();
            GetDeferral() -> ICoreWebView2Deferral;
        }
        cancellation!();
        fn SetHandled(&self, value: BOOL) -> Result<()> {
            self.handled.set(value.as_bool());
            Ok(())
        }
    }
    impl ICoreWebView2LaunchingExternalUriSchemeEventArgs_Impl for Challenge_Impl {
        unused! {
            Uri(_value: *mut PWSTR) -> ();
            InitiatingOrigin(_value: *mut PWSTR) -> ();
            IsUserInitiated(_value: *mut BOOL) -> ();
            Cancel(_value: *mut BOOL) -> ();
            GetDeferral() -> ICoreWebView2Deferral;
        }
        cancellation!();
    }
}

#[test]
fn native_permission_events_cancel_without_reading_ambient_credentials_or_launching_an_app() {
    for fail in [false, true] {
        let cancelled = Rc::new(Cell::new(false));
        let handled = Rc::new(Cell::new(false));
        let args: ICoreWebView2ClientCertificateRequestedEventArgs = com::Challenge {
            cancelled: Rc::clone(&cancelled),
            handled: Rc::clone(&handled),
            fail,
        }
        .into();
        let certificate = certificate_handler();
        let auth = authentication_handler();
        let external = external_handler();
        // SAFETY: handler and fake event object belong to this thread; the fake records cancellation only.
        assert_ne!(unsafe { certificate.Invoke(None, &args) }.is_ok(), fail);
        assert_eq!(cancelled.get(), !fail);
        assert_eq!(handled.get(), !fail);
        cancelled.set(false);
        let basic = args
            .cast::<ICoreWebView2BasicAuthenticationRequestedEventArgs>()
            .unwrap();
        // SAFETY: the fake native authentication object only records the handler's cancellation.
        assert_ne!(unsafe { auth.Invoke(None, &basic) }.is_ok(), fail);
        assert_eq!(cancelled.get(), !fail);
        cancelled.set(false);
        let scheme = args.cast::<ICoreWebView2LaunchingExternalUriSchemeEventArgs>().unwrap();
        // SAFETY: this event object records cancellation and launches no external application.
        assert_ne!(unsafe { external.Invoke(None, &scheme) }.is_ok(), fail);
        assert_eq!(cancelled.get(), !fail);
        // SAFETY: each generated handler explicitly accepts missing native event arguments.
        unsafe { certificate.Invoke(None, None) }.unwrap();
        // SAFETY: same null-argument contract on the authentication handler.
        unsafe { auth.Invoke(None, None) }.unwrap();
        // SAFETY: same null-argument contract on the external-scheme handler.
        unsafe { external.Invoke(None, None) }.unwrap();
    }
}
