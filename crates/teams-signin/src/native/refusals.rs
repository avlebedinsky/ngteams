//! Page capabilities outside the guarded request path are refused natively, including external protocol launches,
//! client certificates and ambient HTTP authentication. TLS errors never offer a click-through in production.
use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_SERVER_CERTIFICATE_ERROR_ACTION_CANCEL, ICoreWebView2, ICoreWebView2_5, ICoreWebView2_10,
    ICoreWebView2_14, ICoreWebView2_18,
};
use webview2_com::{
    BasicAuthenticationRequestedEventHandler, ClientCertificateRequestedEventHandler,
    LaunchingExternalUriSchemeEventHandler, ServerCertificateErrorDetectedEventHandler,
};
use windows::core::{Interface as _, Result};

pub(super) fn install(webview: &ICoreWebView2) -> Result<()> {
    let mut token = 0;
    let certificate = ClientCertificateRequestedEventHandler::create(Box::new(|_, args| {
        if let Some(args) = args {
            // SAFETY: the event owns its arguments, and cancellation is synchronous.
            unsafe { args.SetCancel(true) }?;
            // SAFETY: suppress the default certificate picker as well.
            unsafe { args.SetHandled(true) }?;
        }
        Ok(())
    }));
    let certificates = webview.cast::<ICoreWebView2_5>()?;
    // SAFETY: all registrations occur on the view's owning STA before navigation.
    unsafe { certificates.add_ClientCertificateRequested(&certificate, &raw mut token) }?;
    let authentication = BasicAuthenticationRequestedEventHandler::create(Box::new(|_, args| {
        if let Some(args) = args {
            // SAFETY: the live challenge is cancelled before any ambient authentication can proceed.
            unsafe { args.SetCancel(true) }?;
        }
        Ok(())
    }));
    let authentication_view = webview.cast::<ICoreWebView2_10>()?;
    // SAFETY: registration on the view's owning STA.
    unsafe { authentication_view.add_BasicAuthenticationRequested(&authentication, &raw mut token) }?;
    let external = LaunchingExternalUriSchemeEventHandler::create(Box::new(|_, args| {
        if let Some(args) = args {
            // SAFETY: the live event is cancelled, so no default application may open.
            unsafe { args.SetCancel(true) }?;
        }
        Ok(())
    }));
    let external_view = webview.cast::<ICoreWebView2_18>()?;
    // SAFETY: registration on the view's owning STA.
    unsafe { external_view.add_LaunchingExternalUriScheme(&external, &raw mut token) }?;
    tls(webview)
}

fn tls(webview: &ICoreWebView2) -> Result<()> {
    let handler = ServerCertificateErrorDetectedEventHandler::create(Box::new(|_, args| {
        let Some(args) = args else {
            return Ok(());
        };
        let action = COREWEBVIEW2_SERVER_CERTIFICATE_ERROR_ACTION_CANCEL;
        #[cfg(feature = "test-support")]
        let action = {
            let mut uri = windows::core::PWSTR::null();
            // SAFETY: the live event owns this URI; uri is a writable output, freed below even on failure.
            let result = unsafe { args.RequestUri(&raw mut uri) };
            let uri = webview2_com::take_pwstr(uri);
            result?;
            // An ephemeral self-signed numerical-loopback fixture only; no trust-store entry. Runnable ngteams
            // refuses every test-support build, and production always cancels certificate errors.
            if super::is_local_page(&uri) {
                webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_SERVER_CERTIFICATE_ERROR_ACTION_ALWAYS_ALLOW
            } else {
                action
            }
        };
        // SAFETY: the live certificate error is answered before this callback returns.
        unsafe { args.SetAction(action) }
    }));
    let certificates = webview.cast::<ICoreWebView2_14>()?;
    let mut token = 0;
    // SAFETY: registration occurs on the owning STA before any network load.
    unsafe { certificates.add_ServerCertificateErrorDetected(&handler, &raw mut token) }
}
