//! Thin safe wrappers over the WPE `WebKit` objects the engine drives: each type a `GObject` wrapper, each method one
//! call into the library. All of them live on the engine thread.

use std::ffi::{CString, c_char};
use std::ptr;

use glib::prelude::*;
use glib::translate::{FromGlib, ToGlibPtr, from_glib_full, from_glib_none};

use crate::jsc_sys as jsc;
use crate::spec::{InjectionTime, Rgba, Settings as SettingsFlags, UserScript};
use crate::webkit_sys as webkit;

glib::wrapper! {
    pub(crate) struct WebView(Object<webkit::WebKitWebView, webkit::WebKitWebViewClass>);
    match fn { type_ => || webkit::webkit_web_view_get_type(), }
}

glib::wrapper! {
    pub(crate) struct NetworkSession(Object<webkit::WebKitNetworkSession, webkit::WebKitNetworkSessionClass>);
    match fn { type_ => || webkit::webkit_network_session_get_type(), }
}

glib::wrapper! {
    pub(crate) struct WebsiteDataManager(
        Object<webkit::WebKitWebsiteDataManager, webkit::WebKitWebsiteDataManagerClass>
    );
    match fn { type_ => || webkit::webkit_website_data_manager_get_type(), }
}

glib::wrapper! {
    pub(crate) struct Settings(Object<webkit::WebKitSettings, webkit::WebKitSettingsClass>);
    match fn { type_ => || webkit::webkit_settings_get_type(), }
}

glib::wrapper! {
    pub(crate) struct UserContentManager(
        Object<webkit::WebKitUserContentManager, webkit::WebKitUserContentManagerClass>
    );
    match fn { type_ => || webkit::webkit_user_content_manager_get_type(), }
}

glib::wrapper! {
    pub(crate) struct WebContext(Object<webkit::WebKitWebContext, webkit::WebKitWebContextClass>);
    match fn { type_ => || webkit::webkit_web_context_get_type(), }
}

glib::wrapper! {
    pub(crate) struct BackForwardList(Object<webkit::WebKitBackForwardList, webkit::WebKitBackForwardListClass>);
    match fn { type_ => || webkit::webkit_back_forward_list_get_type(), }
}

glib::wrapper! {
    pub(crate) struct PolicyDecision(Object<webkit::WebKitPolicyDecision, webkit::WebKitPolicyDecisionClass>);
    match fn { type_ => || webkit::webkit_policy_decision_get_type(), }
}

glib::wrapper! {
    pub(crate) struct NavigationPolicyDecision(
        Object<webkit::WebKitNavigationPolicyDecision, webkit::WebKitNavigationPolicyDecisionClass>
    ) @extends PolicyDecision;
    match fn { type_ => || webkit::webkit_navigation_policy_decision_get_type(), }
}

glib::wrapper! {
    pub(crate) struct ResponsePolicyDecision(
        Object<webkit::WebKitResponsePolicyDecision, webkit::WebKitResponsePolicyDecisionClass>
    ) @extends PolicyDecision;
    match fn { type_ => || webkit::webkit_response_policy_decision_get_type(), }
}

glib::wrapper! {
    pub(crate) struct UriRequest(Object<webkit::WebKitURIRequest, webkit::WebKitURIRequestClass>);
    match fn { type_ => || webkit::webkit_uri_request_get_type(), }
}

glib::wrapper! {
    pub(crate) struct UriResponse(Object<webkit::WebKitURIResponse, webkit::WebKitURIResponseClass>);
    match fn { type_ => || webkit::webkit_uri_response_get_type(), }
}

glib::wrapper! {
    pub(crate) struct UriSchemeRequest(Object<webkit::WebKitURISchemeRequest, webkit::WebKitURISchemeRequestClass>);
    match fn { type_ => || webkit::webkit_uri_scheme_request_get_type(), }
}

glib::wrapper! {
    pub(crate) struct Download(Object<webkit::WebKitDownload, webkit::WebKitDownloadClass>);
    match fn { type_ => || webkit::webkit_download_get_type(), }
}

glib::wrapper! {
    pub(crate) struct JscValue(Object<jsc::JSCValue, jsc::JSCValueClass>);
    match fn { type_ => || jsc::jsc_value_get_type(), }
}

glib::wrapper! {
    pub(crate) struct JscContext(Object<jsc::JSCContext, jsc::JSCContextClass>);
    match fn { type_ => || jsc::jsc_context_get_type(), }
}

/// A string `WebKit` returns without transfer, copied.
///
/// # Safety
///
/// `text` is null or a NUL-terminated string that lives for the call.
unsafe fn borrowed_string(text: *const c_char) -> Option<String> {
    // SAFETY: the caller guarantees a NUL-terminated string or null; `from_glib_none` copies it.
    unsafe { from_glib_none(text) }
}

/// `text` as a C string; an interior NUL ends it (`WebKit` would read only that far anyway).
pub(crate) fn c_string(text: &str) -> CString {
    let end = text.find('\0').unwrap_or(text.len());
    CString::new(&text[..end]).unwrap_or_default()
}

fn optional_c_string(text: Option<&str>) -> Option<CString> {
    text.map(c_string)
}

fn as_ptr(text: Option<&CString>) -> *const c_char {
    text.map_or(ptr::null(), |text| text.as_ptr())
}

// MARK: - web view

impl WebView {
    pub(crate) fn raw(&self) -> *mut webkit::WebKitWebView {
        self.to_glib_none().0
    }

    pub(crate) fn load_uri(&self, uri: &str) {
        let uri = c_string(uri);
        // SAFETY: a live web view and a NUL-terminated string.
        unsafe { webkit::webkit_web_view_load_uri(self.raw(), uri.as_ptr()) };
    }

    pub(crate) fn load_html(&self, html: &str, base_uri: Option<&str>) {
        let html = c_string(html);
        let base = optional_c_string(base_uri);
        // SAFETY: a live web view and NUL-terminated strings (the base may be null).
        unsafe { webkit::webkit_web_view_load_html(self.raw(), html.as_ptr(), as_ptr(base.as_ref())) };
    }

    pub(crate) fn reload(&self) {
        // SAFETY: a live web view.
        unsafe { webkit::webkit_web_view_reload(self.raw()) };
    }

    pub(crate) fn stop_loading(&self) {
        // SAFETY: a live web view.
        unsafe { webkit::webkit_web_view_stop_loading(self.raw()) };
    }

    /// Ends the web process the view renders through (`web-process-terminated` follows, by API); the next load
    /// starts a new one.
    pub(crate) fn terminate_web_process(&self) {
        // SAFETY: a live web view.
        unsafe { webkit::webkit_web_view_terminate_web_process(self.raw()) };
    }

    /// The id of the view's page, as a web process extension's `webkit_web_page_get_id` names it.
    pub(crate) fn page_id(&self) -> u64 {
        // SAFETY: a live web view.
        unsafe { webkit::webkit_web_view_get_page_id(self.raw()) }
    }

    pub(crate) fn go_back(&self) {
        // SAFETY: a live web view.
        unsafe { webkit::webkit_web_view_go_back(self.raw()) };
    }

    pub(crate) fn go_forward(&self) {
        // SAFETY: a live web view.
        unsafe { webkit::webkit_web_view_go_forward(self.raw()) };
    }

    pub(crate) fn can_go_back(&self) -> bool {
        // SAFETY: a live web view.
        let can = unsafe { webkit::webkit_web_view_can_go_back(self.raw()) };
        can != glib::ffi::GFALSE
    }

    pub(crate) fn can_go_forward(&self) -> bool {
        // SAFETY: a live web view.
        let can = unsafe { webkit::webkit_web_view_can_go_forward(self.raw()) };
        can != glib::ffi::GFALSE
    }

    pub(crate) fn uri(&self) -> Option<String> {
        // SAFETY: a live web view; the string is owned by it and copied before any other call.
        let raw = unsafe { webkit::webkit_web_view_get_uri(self.raw()) };
        // SAFETY: `raw` comes straight from that call, owned as its documentation says.
        unsafe { borrowed_string(raw) }
    }

    pub(crate) fn title(&self) -> Option<String> {
        // SAFETY: a live web view; the string is owned by it and copied before any other call.
        let raw = unsafe { webkit::webkit_web_view_get_title(self.raw()) };
        // SAFETY: `raw` comes straight from that call, owned as its documentation says.
        unsafe { borrowed_string(raw) }
    }

    pub(crate) fn set_zoom_level(&self, zoom: f64) {
        // SAFETY: a live web view.
        unsafe { webkit::webkit_web_view_set_zoom_level(self.raw(), zoom) };
    }

    pub(crate) fn set_background_color(&self, color: Rgba) {
        let mut color = webkit::WebKitColor {
            red: color.red,
            green: color.green,
            blue: color.blue,
            alpha: color.alpha,
        };
        // SAFETY: a live web view and a color it copies.
        unsafe { webkit::webkit_web_view_set_background_color(self.raw(), &raw mut color) };
    }

    /// The `WPEView` the web view renders through.
    pub(crate) fn wpe_view(&self) -> *mut crate::platform_sys::WPEView {
        // SAFETY: a live web view; the view is owned by it.
        unsafe { webkit::webkit_web_view_get_wpe_view(self.raw()) }
    }

    pub(crate) fn back_forward_list(&self) -> Option<BackForwardList> {
        // SAFETY: a live web view; `from_glib_none` takes a reference to the list it owns.
        let raw = unsafe { webkit::webkit_web_view_get_back_forward_list(self.raw()) };
        // SAFETY: `raw` comes straight from that call, owned as its documentation says.
        unsafe { from_glib_none(raw) }
    }

    /// Runs `script` in `world` (the page's own when `None`); `done` gets the value or the exception.
    pub(crate) fn evaluate_javascript(
        &self,
        script: &str,
        world: Option<&str>,
        done: impl FnOnce(Result<Option<JscValue>, glib::Error>) + 'static,
    ) {
        type Done = Box<dyn FnOnce(Result<Option<JscValue>, glib::Error>)>;
        unsafe extern "C" fn finished(
            source: *mut glib::gobject_ffi::GObject,
            result: *mut gio::ffi::GAsyncResult,
            data: glib::ffi::gpointer,
        ) {
            // SAFETY: `data` is the box leaked below, given back exactly once here.
            let done = unsafe { Box::from_raw(data.cast::<Done>()) };
            let mut error = ptr::null_mut();
            // SAFETY: `source` is the web view the call ran on and `result` its result; the value is ours.
            let value =
                unsafe { webkit::webkit_web_view_evaluate_javascript_finish(source.cast(), result, &raw mut error) };
            if error.is_null() {
                // SAFETY: a new reference to a JSCValue (or null), which we take over.
                (done)(Ok(unsafe { from_glib_full(value) }));
            } else {
                // SAFETY: the call set `error`, which we take over.
                (done)(Err(unsafe { from_glib_full(error) }));
            }
        }
        let script = c_string(script);
        let world = optional_c_string(world);
        let done: Box<Done> = Box::new(Box::new(done));
        // SAFETY: a live web view, NUL-terminated strings the call copies, and a callback that takes the box back.
        unsafe {
            webkit::webkit_web_view_evaluate_javascript(
                self.raw(),
                script.as_ptr(),
                -1,
                as_ptr(world.as_ref()),
                ptr::null(),
                ptr::null_mut(),
                Some(finished),
                Box::into_raw(done).cast(),
            );
        };
    }
}

impl BackForwardList {
    /// Calls `changed` whenever the history changes.
    pub(crate) fn connect_changed(&self, changed: impl Fn() + 'static) {
        self.connect_local("changed", false, move |_| {
            changed();
            None
        });
    }
}

// MARK: - settings and user content

impl Settings {
    pub(crate) fn new(flags: SettingsFlags) -> Self {
        // SAFETY: a constructor returning a new reference, which we take over.
        let raw = unsafe { webkit::webkit_settings_new() };
        // SAFETY: `raw` comes straight from that call, owned as its documentation says.
        let settings: Self = unsafe { from_glib_full(raw) };
        let raw = settings.to_glib_none().0;
        let on = |flag: SettingsFlags| glib::ffi::gboolean::from(flags.contains(flag));
        // SAFETY: a live settings object.
        unsafe { webkit::webkit_settings_set_enable_javascript_markup(raw, on(SettingsFlags::JAVASCRIPT_MARKUP)) };
        // SAFETY: as above.
        unsafe {
            webkit::webkit_settings_set_javascript_can_open_windows_automatically(
                raw,
                on(SettingsFlags::OPEN_WINDOWS_AUTOMATICALLY),
            );
        };
        // SAFETY: as above.
        unsafe {
            webkit::webkit_settings_set_allow_file_access_from_file_urls(
                raw,
                on(SettingsFlags::FILE_ACCESS_FROM_FILE_URLS),
            );
        };
        // SAFETY: as above.
        unsafe {
            webkit::webkit_settings_set_allow_universal_access_from_file_urls(
                raw,
                on(SettingsFlags::UNIVERSAL_ACCESS_FROM_FILE_URLS),
            );
        };
        // SAFETY: as above.
        unsafe { webkit::webkit_settings_set_enable_developer_extras(raw, on(SettingsFlags::DEVELOPER_EXTRAS)) };
        // SAFETY: as above.
        unsafe { webkit::webkit_settings_set_enable_fullscreen(raw, on(SettingsFlags::FULLSCREEN)) };
        // SAFETY: as above.
        unsafe {
            webkit::webkit_settings_set_media_playback_requires_user_gesture(
                raw,
                on(SettingsFlags::MEDIA_REQUIRES_USER_GESTURE),
            );
        };
        // SAFETY: as above.
        unsafe { webkit::webkit_settings_set_enable_webrtc(raw, on(SettingsFlags::WEBRTC)) };
        // SAFETY: as above.
        unsafe { webkit::webkit_settings_set_enable_page_cache(raw, on(SettingsFlags::PAGE_CACHE)) };
        settings
    }

    /// Turns `WebKit`'s `HardwareAcceleration` feature on or off for the pages built on these settings: off, a page's
    /// web process draws on the CPU into SHM buffers instead of dmabufs. False when this `WebKit` has no such feature.
    pub(crate) fn set_hardware_acceleration(&self, enabled: bool) -> bool {
        self.set_feature("HardwareAcceleration", enabled)
    }

    /// Turns the `WebKit` feature `identifier` (as `WebKitFeature` names it) on or off for the pages built on these
    /// settings. False when this `WebKit` has no such feature.
    pub(crate) fn set_feature(&self, identifier: &str, enabled: bool) -> bool {
        // SAFETY: a new reference to the list of every feature, dropped below.
        let features = unsafe { webkit::webkit_settings_get_all_features() };
        if features.is_null() {
            return false;
        }
        let identifier = c_string(identifier);
        // SAFETY: a live list and a NUL-terminated identifier; the feature stays owned by the list.
        let feature = unsafe { webkit::webkit_feature_list_find(features, identifier.as_ptr()) };
        let found = !feature.is_null();
        if found {
            // SAFETY: live settings and a feature of the list, which is still referenced.
            unsafe {
                webkit::webkit_settings_set_feature_enabled(
                    self.to_glib_none().0,
                    feature,
                    glib::ffi::gboolean::from(enabled),
                );
            };
        }
        // SAFETY: drops the reference taken above.
        unsafe { webkit::webkit_feature_list_unref(features) };
        found
    }
}

impl UserContentManager {
    pub(crate) fn new() -> Self {
        // SAFETY: a constructor returning a new reference, which we take over.
        let raw = unsafe { webkit::webkit_user_content_manager_new() };
        // SAFETY: `raw` comes straight from that call, owned as its documentation says.
        unsafe { from_glib_full(raw) }
    }

    fn raw(&self) -> *mut webkit::WebKitUserContentManager {
        self.to_glib_none().0
    }

    /// Replaces every user script with `scripts`, injected into the top frame.
    pub(crate) fn set_scripts(&self, scripts: &[UserScript]) {
        // SAFETY: a live manager.
        unsafe { webkit::webkit_user_content_manager_remove_all_scripts(self.raw()) };
        for script in scripts {
            let source = c_string(&script.source);
            let time = match script.time {
                InjectionTime::Start => webkit::WEBKIT_USER_SCRIPT_INJECT_AT_DOCUMENT_START,
                InjectionTime::End => webkit::WEBKIT_USER_SCRIPT_INJECT_AT_DOCUMENT_END,
            };
            let frames = webkit::WEBKIT_USER_CONTENT_INJECT_TOP_FRAME;
            let user_script = match &script.world {
                Some(world) => {
                    let world = c_string(world);
                    // SAFETY: NUL-terminated strings the call copies; null allow and block lists.
                    unsafe {
                        webkit::webkit_user_script_new_for_world(
                            source.as_ptr(),
                            frames,
                            time,
                            world.as_ptr(),
                            ptr::null(),
                            ptr::null(),
                        )
                    }
                }
                // SAFETY: as above.
                None => unsafe {
                    webkit::webkit_user_script_new(source.as_ptr(), frames, time, ptr::null(), ptr::null())
                },
            };
            // SAFETY: a live manager and a new script, which it references.
            unsafe { webkit::webkit_user_content_manager_add_script(self.raw(), user_script) };
            // SAFETY: drops our reference to the script.
            unsafe { webkit::webkit_user_script_unref(user_script) };
        }
    }

    /// Registers handler `name` in `world`, its messages answered by reply; false when the name is taken there.
    pub(crate) fn register_handler(&self, name: &str, world: Option<&str>) -> bool {
        let name = c_string(name);
        let world = optional_c_string(world);
        // SAFETY: a live manager and NUL-terminated strings the call copies.
        let registered = unsafe {
            webkit::webkit_user_content_manager_register_script_message_handler_with_reply(
                self.raw(),
                name.as_ptr(),
                as_ptr(world.as_ref()),
            )
        };
        registered != glib::ffi::GFALSE
    }

    pub(crate) fn unregister_handler(&self, name: &str, world: Option<&str>) {
        let name = c_string(name);
        let world = optional_c_string(world);
        // SAFETY: a live manager and NUL-terminated strings.
        unsafe {
            webkit::webkit_user_content_manager_unregister_script_message_handler(
                self.raw(),
                name.as_ptr(),
                as_ptr(world.as_ref()),
            );
        };
    }
}

// MARK: - network sessions

impl NetworkSession {
    pub(crate) fn ephemeral() -> Self {
        // SAFETY: a constructor returning a new reference, which we take over.
        let raw = unsafe { webkit::webkit_network_session_new_ephemeral() };
        // SAFETY: `raw` comes straight from that call, owned as its documentation says.
        unsafe { from_glib_full(raw) }
    }

    /// A store kept under `data_dir` and `cache_dir`, cookies included: `WebKit` keeps a session's cookies in memory
    /// unless told where to save them, so they go to `cookies.sqlite` in the data directory, which a later session
    /// on the same directories reads back.
    pub(crate) fn persistent(data_dir: &std::path::Path, cache_dir: &std::path::Path) -> Self {
        let data = c_string(&data_dir.to_string_lossy());
        let cache = c_string(&cache_dir.to_string_lossy());
        // SAFETY: NUL-terminated paths the call copies; a new reference, which we take over.
        let raw = unsafe { webkit::webkit_network_session_new(data.as_ptr(), cache.as_ptr()) };
        // SAFETY: `raw` comes straight from that call, owned as its documentation says.
        let session: Self = unsafe { from_glib_full(raw) };
        let cookies = c_string(&data_dir.join("cookies.sqlite").to_string_lossy());
        // SAFETY: a live session; the cookie manager is owned by it.
        let manager = unsafe { webkit::webkit_network_session_get_cookie_manager(session.to_glib_none().0) };
        if !manager.is_null() {
            // SAFETY: a live cookie manager and a NUL-terminated path the call copies.
            unsafe {
                webkit::webkit_cookie_manager_set_persistent_storage(
                    manager,
                    cookies.as_ptr(),
                    webkit::WEBKIT_COOKIE_PERSISTENT_STORAGE_SQLITE,
                );
            };
        }
        session
    }

    /// An ephemeral store with Intelligent Tracking Prevention off whose every load goes through the proxy at `proxy`,
    /// but those to `ignore_hosts` (`WEBKIT_NETWORK_PROXY_MODE_CUSTOM`: no direct fallback). `None` when the proxy
    /// settings can't be made: a store that would go direct is never handed out.
    pub(crate) fn guarded(proxy: &str, ignore_hosts: &[String]) -> Option<Self> {
        let session = Self::ephemeral();
        let raw = session.to_glib_none().0;
        // SAFETY: a live session.
        unsafe { webkit::webkit_network_session_set_itp_enabled(raw, glib::ffi::GFALSE) };
        let proxy = c_string(proxy);
        let hosts: Vec<CString> = ignore_hosts.iter().map(|host| c_string(host)).collect();
        let mut list: Vec<*const c_char> = hosts.iter().map(|host| host.as_ptr()).collect();
        list.push(ptr::null());
        // SAFETY: a NUL-terminated URI and a NULL-terminated array of NUL-terminated hosts, all copied by the call;
        // a new boxed value, freed below.
        let settings = unsafe { webkit::webkit_network_proxy_settings_new(proxy.as_ptr(), list.as_ptr()) };
        if settings.is_null() {
            return None;
        }
        // SAFETY: a live session and live settings, which the call copies.
        unsafe {
            webkit::webkit_network_session_set_proxy_settings(raw, webkit::WEBKIT_NETWORK_PROXY_MODE_CUSTOM, settings);
        };
        // SAFETY: frees the settings made above, which nothing else holds.
        unsafe { webkit::webkit_network_proxy_settings_free(settings) };
        Some(session)
    }

    pub(crate) fn website_data_manager(&self) -> Option<WebsiteDataManager> {
        // SAFETY: a live session; `from_glib_none` takes a reference to the manager it owns.
        let raw = unsafe { webkit::webkit_network_session_get_website_data_manager(self.to_glib_none().0) };
        // SAFETY: `raw` comes straight from that call, owned as its documentation says.
        unsafe { from_glib_none(raw) }
    }
}

impl WebsiteDataManager {
    /// Removes everything the store holds; `done` gets the outcome.
    pub(crate) fn clear_all(&self, done: impl FnOnce(Result<(), glib::Error>) + 'static) {
        type Done = Box<dyn FnOnce(Result<(), glib::Error>)>;
        unsafe extern "C" fn finished(
            source: *mut glib::gobject_ffi::GObject,
            result: *mut gio::ffi::GAsyncResult,
            data: glib::ffi::gpointer,
        ) {
            // SAFETY: `data` is the box leaked below, given back exactly once here.
            let done = unsafe { Box::from_raw(data.cast::<Done>()) };
            let mut error = ptr::null_mut();
            // SAFETY: `source` is the manager the call ran on and `result` its result.
            let cleared =
                unsafe { webkit::webkit_website_data_manager_clear_finish(source.cast(), result, &raw mut error) };
            if cleared == glib::ffi::GFALSE && !error.is_null() {
                // SAFETY: the call set `error`, which we take over.
                (done)(Err(unsafe { from_glib_full(error) }));
            } else {
                (done)(Ok(()));
            }
        }
        let done: Box<Done> = Box::new(Box::new(done));
        // SAFETY: a live manager, every data type, no time limit, and a callback that takes the box back.
        unsafe {
            webkit::webkit_website_data_manager_clear(
                self.to_glib_none().0,
                webkit::WEBKIT_WEBSITE_DATA_ALL,
                0,
                ptr::null_mut(),
                Some(finished),
                Box::into_raw(done).cast(),
            );
        };
    }
}

impl Download {
    pub(crate) fn cancel(&self) {
        // SAFETY: a live download.
        unsafe { webkit::webkit_download_cancel(self.to_glib_none().0) };
    }

    pub(crate) fn uri(&self) -> Option<String> {
        // SAFETY: a live download; the request is owned by it.
        let raw = unsafe { webkit::webkit_download_get_request(self.to_glib_none().0) };
        // SAFETY: `raw` comes straight from that call, owned as its documentation says.
        let request: Option<UriRequest> = unsafe { from_glib_none(raw) };
        request.and_then(|request| request.uri())
    }

    pub(crate) fn web_view(&self) -> Option<WebView> {
        // SAFETY: a live download; `from_glib_none` takes a reference to its view, if any.
        let raw = unsafe { webkit::webkit_download_get_web_view(self.to_glib_none().0) };
        // SAFETY: `raw` comes straight from that call, owned as its documentation says.
        unsafe { from_glib_none(raw) }
    }
}

// MARK: - requests and decisions

impl UriRequest {
    pub(crate) fn uri(&self) -> Option<String> {
        // SAFETY: a live request; the string is owned by it and copied at once.
        let raw = unsafe { webkit::webkit_uri_request_get_uri(self.to_glib_none().0) };
        // SAFETY: `raw` comes straight from that call, owned as its documentation says.
        unsafe { borrowed_string(raw) }
    }

    pub(crate) fn method(&self) -> Option<String> {
        // SAFETY: a live request; the string is owned by it and copied at once (null when the method was left out).
        let raw = unsafe { webkit::webkit_uri_request_get_http_method(self.to_glib_none().0) };
        // SAFETY: `raw` comes straight from that call, owned as its documentation says.
        unsafe { borrowed_string(raw) }
    }
}

impl UriResponse {
    pub(crate) fn uri(&self) -> Option<String> {
        // SAFETY: a live response; the string is owned by it and copied at once.
        let raw = unsafe { webkit::webkit_uri_response_get_uri(self.to_glib_none().0) };
        // SAFETY: `raw` comes straight from that call, owned as its documentation says.
        unsafe { borrowed_string(raw) }
    }
}

impl PolicyDecision {
    pub(crate) fn use_(&self) {
        // SAFETY: a live decision; deciding twice is a no-op in WebKit.
        unsafe { webkit::webkit_policy_decision_use(self.to_glib_none().0) };
    }

    pub(crate) fn ignore(&self) {
        // SAFETY: a live decision; deciding twice is a no-op in WebKit.
        unsafe { webkit::webkit_policy_decision_ignore(self.to_glib_none().0) };
    }
}

impl NavigationPolicyDecision {
    /// The navigation's target address and type.
    pub(crate) fn navigation(&self) -> Option<Navigation> {
        // SAFETY: a live decision; the action is owned by it.
        let action = unsafe { webkit::webkit_navigation_policy_decision_get_navigation_action(self.to_glib_none().0) };
        if action.is_null() {
            return None;
        }
        // SAFETY: a live action; the request is owned by it, and `from_glib_none` takes a reference.
        let raw = unsafe { webkit::webkit_navigation_action_get_request(action) };
        // SAFETY: `raw` comes straight from that call, owned as its documentation says.
        let request: Option<UriRequest> = unsafe { from_glib_none(raw) };
        // SAFETY: a live action.
        let kind = unsafe { webkit::webkit_navigation_action_get_navigation_type(action) };
        Some(Navigation {
            uri: request.as_ref().and_then(UriRequest::uri).unwrap_or_default(),
            method: request.as_ref().and_then(UriRequest::method),
            kind,
        })
    }
}

/// A navigation's request and why it started.
pub(crate) struct Navigation {
    pub(crate) uri: String,
    /// The request's HTTP method; `None` when `WebKit` left it out (a `GET`).
    pub(crate) method: Option<String>,
    pub(crate) kind: webkit::WebKitNavigationType,
}

impl ResponsePolicyDecision {
    fn raw(&self) -> *mut webkit::WebKitResponsePolicyDecision {
        self.to_glib_none().0
    }

    pub(crate) fn uri(&self) -> String {
        // SAFETY: a live decision; `from_glib_none` takes a reference to the response it owns.
        let raw = unsafe { webkit::webkit_response_policy_decision_get_response(self.raw()) };
        // SAFETY: `raw` comes straight from that call, owned as its documentation says.
        let response: Option<UriResponse> = unsafe { from_glib_none(raw) };
        response.and_then(|response| response.uri()).unwrap_or_default()
    }

    pub(crate) fn is_main_frame_main_resource(&self) -> bool {
        // SAFETY: a live decision.
        let main = unsafe { webkit::webkit_response_policy_decision_is_main_frame_main_resource(self.raw()) };
        main != glib::ffi::GFALSE
    }

    pub(crate) fn is_mime_type_supported(&self) -> bool {
        // SAFETY: a live decision.
        let supported = unsafe { webkit::webkit_response_policy_decision_is_mime_type_supported(self.raw()) };
        supported != glib::ffi::GFALSE
    }
}

impl UriSchemeRequest {
    fn raw(&self) -> *mut webkit::WebKitURISchemeRequest {
        self.to_glib_none().0
    }

    pub(crate) fn uri(&self) -> String {
        // SAFETY: a live request; the string is owned by it and copied at once.
        let raw = unsafe { webkit::webkit_uri_scheme_request_get_uri(self.raw()) };
        // SAFETY: `raw` comes straight from that call, owned as its documentation says.
        unsafe { borrowed_string(raw) }.unwrap_or_default()
    }

    pub(crate) fn web_view(&self) -> Option<WebView> {
        // SAFETY: a live request; `from_glib_none` takes a reference to its view.
        let raw = unsafe { webkit::webkit_uri_scheme_request_get_web_view(self.raw()) };
        // SAFETY: `raw` comes straight from that call, owned as its documentation says.
        unsafe { from_glib_none(raw) }
    }

    /// Answers with `stream`, `length` bytes long (-1: unknown), of type `mime`.
    pub(crate) fn finish(&self, stream: &gio::InputStream, length: i64, mime: &str) {
        let mime = c_string(mime);
        // SAFETY: a live request, a live stream the request references, and a NUL-terminated type it copies.
        unsafe { webkit::webkit_uri_scheme_request_finish(self.raw(), stream.to_glib_none().0, length, mime.as_ptr()) };
    }

    pub(crate) fn finish_error(&self, error: &glib::Error) {
        let raw: *const glib::ffi::GError = error.to_glib_none().0;
        // SAFETY: a live request and an error the call only reads (copies).
        unsafe { webkit::webkit_uri_scheme_request_finish_error(self.raw(), raw.cast_mut()) };
    }
}

// MARK: - script messages

impl JscValue {
    fn raw(&self) -> *mut jsc::JSCValue {
        self.to_glib_none().0
    }

    pub(crate) fn is_object(&self) -> bool {
        // SAFETY: a live value.
        let object = unsafe { jsc::jsc_value_is_object(self.raw()) };
        object != glib::ffi::GFALSE
    }

    pub(crate) fn is_undefined(&self) -> bool {
        // SAFETY: a live value.
        let undefined = unsafe { jsc::jsc_value_is_undefined(self.raw()) };
        undefined != glib::ffi::GFALSE
    }

    /// The value as JSON, `None` when JSON cannot express it.
    pub(crate) fn to_json(&self) -> Option<String> {
        // SAFETY: a live value; the string is new and ours.
        let raw = unsafe { jsc::jsc_value_to_json(self.raw(), 0) };
        // SAFETY: `raw` comes straight from that call, owned as its documentation says.
        unsafe { from_glib_full(raw) }
    }

    pub(crate) fn context(&self) -> Option<JscContext> {
        // SAFETY: a live value; `from_glib_none` takes a reference to its context.
        let raw = unsafe { jsc::jsc_value_get_context(self.raw()) };
        // SAFETY: `raw` comes straight from that call, owned as its documentation says.
        unsafe { from_glib_none(raw) }
    }
}

impl JscContext {
    /// `json` parsed in this context; `None` for text that is not JSON.
    pub(crate) fn value_from_json(&self, json: &str) -> Option<JscValue> {
        let json = c_string(json);
        // SAFETY: a live context and a NUL-terminated string; the value is new and ours.
        let raw = unsafe { jsc::jsc_value_new_from_json(self.to_glib_none().0, json.as_ptr()) };
        // SAFETY: `raw` comes straight from that call, owned as its documentation says.
        unsafe { from_glib_full(raw) }
    }
}

/// A script message's reply (`WebKitScriptMessageReply`), held until it is answered once.
pub(crate) struct ScriptReply(ptr::NonNull<webkit::WebKitScriptMessageReply>);

impl ScriptReply {
    /// Takes a reference to `reply`.
    ///
    /// # Safety
    ///
    /// `reply` is null or a live `WebKitScriptMessageReply`.
    pub(crate) unsafe fn retain(reply: *mut webkit::WebKitScriptMessageReply) -> Option<Self> {
        let reply = ptr::NonNull::new(reply)?;
        // SAFETY: the caller guarantees a live reply; the new reference is dropped by `Drop`.
        unsafe { webkit::webkit_script_message_reply_ref(reply.as_ptr()) };
        Some(Self(reply))
    }

    pub(crate) fn return_value(&self, value: &JscValue) {
        // SAFETY: a live reply (we hold a reference) and a live value.
        unsafe { webkit::webkit_script_message_reply_return_value(self.0.as_ptr(), value.raw()) };
    }

    pub(crate) fn return_error_message(&self, message: &str) {
        let message = c_string(message);
        // SAFETY: a live reply and a NUL-terminated message the call copies.
        unsafe { webkit::webkit_script_message_reply_return_error_message(self.0.as_ptr(), message.as_ptr()) };
    }
}

impl Drop for ScriptReply {
    fn drop(&mut self) {
        // SAFETY: drops the reference `retain` took.
        unsafe { webkit::webkit_script_message_reply_unref(self.0.as_ptr()) };
    }
}

impl WebContext {
    pub(crate) fn default() -> Option<Self> {
        // SAFETY: the default context, owned by WebKit; `from_glib_none` takes a reference.
        let raw = unsafe { webkit::webkit_web_context_get_default() };
        // SAFETY: `raw` comes straight from that call, owned as its documentation says.
        unsafe { from_glib_none(raw) }
    }

    /// Lets instrumented extensions keep their counters outside `WebKit`'s private filesystem. This is compiled only
    /// for coverage builds: ordinary engines never grant this additional writable path.
    #[cfg(coverage)]
    pub(crate) fn set_profiling_directory(&self) {
        let directory = std::env::var_os("LLVM_PROFILE_FILE")
            .and_then(|pattern| {
                std::path::Path::new(&pattern)
                    .parent()
                    .map(std::path::Path::to_path_buf)
            })
            .filter(|directory| directory.is_absolute())
            .and_then(|directory| directory.canonicalize().ok());
        let Some(directory) = directory else {
            return;
        };
        let path = c_string(&directory.to_string_lossy());
        // SAFETY: a live context, before any web process starts, and a NUL-terminated path it copies.
        unsafe {
            webkit::webkit_web_context_add_path_to_sandbox(self.to_glib_none().0, path.as_ptr(), glib::ffi::GFALSE);
        }
    }

    /// Loads every module in `directory` into each web process the context starts from now on, handing each one
    /// `data` (a `GVariant` string) when it initializes, and makes each of `sandbox_paths` visible (read-only) inside
    /// the web processes' sandbox.
    pub(crate) fn set_web_process_extensions(
        &self,
        directory: &std::path::Path,
        data: &str,
        sandbox_paths: &[std::path::PathBuf],
    ) {
        let raw = self.to_glib_none().0;
        let directory = c_string(&directory.to_string_lossy());
        // SAFETY: a live context and a NUL-terminated path it copies.
        unsafe { webkit::webkit_web_context_set_web_process_extensions_directory(raw, directory.as_ptr()) };
        let data = c_string(data);
        // SAFETY: a NUL-terminated string the call copies; the variant comes back floating.
        let variant = unsafe { glib::ffi::g_variant_new_string(data.as_ptr()) };
        // SAFETY: a live context; it sinks the floating variant.
        unsafe { webkit::webkit_web_context_set_web_process_extensions_initialization_user_data(raw, variant) };
        for path in sandbox_paths {
            let path = c_string(&path.to_string_lossy());
            // SAFETY: a live context and a NUL-terminated path it copies.
            unsafe { webkit::webkit_web_context_add_path_to_sandbox(raw, path.as_ptr(), glib::ffi::GTRUE) };
        }
    }

    /// Serves `scheme`: the context hands every request to [`super::scheme_requested`] until the process ends.
    pub(crate) fn register_uri_scheme(&self, scheme: &str) {
        unsafe extern "C" fn requested(request: *mut webkit::WebKitURISchemeRequest, _: glib::ffi::gpointer) {
            // SAFETY: WebKit passes a live request; `from_glib_none` takes a reference.
            let request: UriSchemeRequest = unsafe { from_glib_none(request) };
            super::scheme_requested(&request);
        }
        let scheme = c_string(scheme);
        // SAFETY: a live context, a NUL-terminated scheme it copies, and a callback that needs no data.
        unsafe {
            webkit::webkit_web_context_register_uri_scheme(
                self.to_glib_none().0,
                scheme.as_ptr(),
                Some(requested),
                ptr::null_mut(),
                None,
            );
        };
    }
}

/// The error domain and code of `error`, for matching `WebKit`'s quarks.
pub(crate) fn error_is(error: &glib::Error, domain: glib::ffi::GQuark, code: i32) -> bool {
    // SAFETY: a registered quark from WebKit's error domain functions.
    let domain = unsafe { glib::Quark::from_glib(domain) };
    error.domain() == domain && {
        let raw: *const glib::ffi::GError = error.to_glib_none().0;
        // SAFETY: a live error; reading its code field.
        unsafe { (*raw).code == code }
    }
}

/// The `WebKitLoadEvent` or other enum argument at `index` of a signal's arguments.
pub(crate) fn enum_arg(values: &[glib::Value], index: usize) -> i32 {
    values.get(index).map_or(-1, |value| {
        // SAFETY: the signal's argument at `index` is an enum, which `g_value_get_enum` reads.
        unsafe { glib::gobject_ffi::g_value_get_enum(value.to_glib_none().0) }
    })
}

/// The boxed or pointer argument at `index` of a signal's arguments.
pub(crate) fn pointer_arg(values: &[glib::Value], index: usize) -> glib::ffi::gpointer {
    values.get(index).map_or(ptr::null_mut(), |value| {
        // SAFETY: the signal's argument at `index` is boxed, which `g_value_get_boxed` reads without transfer.
        unsafe { glib::gobject_ffi::g_value_get_boxed(value.to_glib_none().0) }
    })
}

/// The object argument at `index` of a signal's arguments.
pub(crate) fn object_arg<T: IsA<glib::Object>>(values: &[glib::Value], index: usize) -> Option<T> {
    values
        .get(index)
        .and_then(|value| value.get::<Option<glib::Object>>().ok().flatten())
        .and_then(|object| object.downcast::<T>().ok())
}

/// A boolean signal's return value: whether the handler took care of it.
#[expect(
    clippy::unnecessary_wraps,
    reason = "a signal handler's return type, which every call site returns as is"
)]
pub(crate) fn handled(handled: bool) -> Option<glib::Value> {
    Some(handled.to_value())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_strings_end_at_an_interior_nul() {
        assert_eq!(c_string("a\0b").as_bytes(), b"a");
        assert_eq!(c_string("plain").as_bytes(), b"plain");
        assert_eq!(c_string("").as_bytes(), b"");
    }
}
