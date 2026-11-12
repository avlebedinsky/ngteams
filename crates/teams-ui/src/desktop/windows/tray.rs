//! `Shell_NotifyIcon` and `ITaskbarList3`, on an owned message-loop thread. Explorer restart re-adds the icon.
//! The hidden window is top-level (not `HWND_MESSAGE`) so it receives `TaskbarCreated` broadcasts.
use super::{Apartment, registration::wide};
use crate::{
    app_icon,
    tray::{TrayEvent, TrayIcon, title},
};
use futures_util::future::BoxFuture;
use std::{
    io,
    sync::{OnceLock, mpsc},
};
use tokio::sync::oneshot;
use windows::Win32::{
    Foundation::HWND as ComHwnd,
    System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
    UI::Shell::{ITaskbarList3, TaskbarList},
};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Shell::{
            DefSubclassProc, GetWindowSubclass, NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE,
            NIM_MODIFY, NIM_SETFOCUS, NIM_SETVERSION, NIN_SELECT, NINF_KEY, NOTIFYICON_VERSION_4, NOTIFYICONDATAW,
            RemoveWindowSubclass, SetWindowSubclass, Shell_NotifyIconW,
        },
        WindowsAndMessaging::{
            AppendMenuW, CreateIcon, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu,
            DestroyWindow, DispatchMessageW, EndMenu, GetCursorPos, GetMessageW, HICON, InSendMessageEx, MF_STRING,
            MSG, PostMessageW, RegisterClassW, RegisterWindowMessageW, SetForegroundWindow, SetMenuDefaultItem,
            TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_APP, WM_CONTEXTMENU, WM_NCDESTROY,
            WM_NULL, WNDCLASSW, WS_EX_TOOLWINDOW,
        },
    },
};

#[cfg(test)]
use windows_sys::Win32::UI::WindowsAndMessaging::{SendMessageW, WS_OVERLAPPEDWINDOW, WS_VISIBLE};

const WAKE: u32 = WM_APP + 31;
const CALLBACK: u32 = WM_APP + 32;
const ICON_ID: u32 = 1;
// Windows SDK: NIN_KEYSELECT is NIN_SELECT | NINF_KEY (not exported by windows-sys).
const NIN_KEYSELECT: u32 = NIN_SELECT | NINF_KEY;

fn taskbar_created() -> u32 {
    static MESSAGE: OnceLock<u32> = OnceLock::new();
    *MESSAGE.get_or_init(|| {
        // SAFETY: fixed terminated system broadcast message name; the returned ID is process-independent.
        unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) }
    })
}

fn taskbar_button_created() -> u32 {
    static MESSAGE: OnceLock<u32> = OnceLock::new();
    *MESSAGE.get_or_init(|| {
        // SAFETY: fixed terminated system message name; IDs are shared with Explorer.
        unsafe { RegisterWindowMessageW(wide("TaskbarButtonCreated").as_ptr()) }
    })
}

struct OverlayWake {
    commands: mpsc::Sender<Command>,
    tray: usize,
}

// Called on the main window's GPUI thread, since comctl32 forbids cross-thread subclass installation.
fn watch_taskbar_button(hwnd: HWND, commands: mpsc::Sender<Command>, tray: usize) -> io::Result<()> {
    let mut existing = 0;
    // SAFETY: queries only our static subclass callback and ID on a live process-owned window.
    if unsafe { GetWindowSubclass(hwnd, Some(overlay_proc), 1, &raw mut existing) } != 0 {
        return Ok(());
    }
    let wake = Box::into_raw(Box::new(OverlayWake { commands, tray }));
    // SAFETY: the subclass owns wake until WM_NCDESTROY; the procedure is static for this image's lifetime.
    if unsafe { SetWindowSubclass(hwnd, Some(overlay_proc), 1, wake as usize) } == 0 {
        // SAFETY: installation failed, so Windows never took this allocation.
        drop(unsafe { Box::from_raw(wake) });
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

unsafe extern "system" fn overlay_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    if message != 0 && message == taskbar_button_created() {
        // SAFETY: successful subclass installation owns this allocation until WM_NCDESTROY below.
        let wake = unsafe { &*(data as *const OverlayWake) };
        if wake.commands.send(Command::OverlayReady(hwnd as usize)).is_ok() {
            // SAFETY: the live receiver owns its hidden window; no pointers are carried in the message.
            unsafe {
                PostMessageW(wake.tray as HWND, WAKE, 0, 0);
            }
        }
    }
    if message == WM_NCDESTROY {
        // SAFETY: this is our installed subclass and ID, removed on its owning thread before releasing its data.
        unsafe {
            RemoveWindowSubclass(hwnd, Some(overlay_proc), id);
        }
        // SAFETY: this is the one allocation installed above, and no callback can use it after removal.
        drop(unsafe { Box::from_raw(data as *mut OverlayWake) });
    }
    // SAFETY: preserve GPUI's complete window procedure and unchanged message parameters.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

pub(crate) enum Event {
    Asked(TrayEvent),
    Hosted(bool),
}
enum Command {
    #[cfg(test)]
    TestWindow(mpsc::Sender<Command>, oneshot::Sender<usize>),
    #[cfg(test)]
    TestOverlay(oneshot::Sender<bool>),
    #[cfg(test)]
    TestOverlayCount(oneshot::Sender<u64>),
    OverlayReady(usize),
    Unread(u64),
    Window(Option<usize>),
    Stop,
}
pub struct NativeTray {
    hwnd: usize,
    commands: mpsc::Sender<Command>,
    thread: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
}
impl NativeTray {
    pub(crate) async fn start(app: String) -> Result<(Self, async_channel::Receiver<Event>), String> {
        let (commands, queue) = mpsc::channel();
        let (events, asked) = async_channel::unbounded();
        let (ready, started) = oneshot::channel();
        // Cancellation before the frontend is returned must leave no detached native worker.
        let (activate, opened) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("ngteams-tray".into())
            .spawn(move || {
                let initialized = Apartment::new().map_err(|e| e.to_string()).and_then(|apartment| {
                    State::new(app, events)
                        .map(|state| (apartment, state))
                        .map_err(|e| e.to_string())
                });
                match initialized {
                    Ok((_apartment, mut state)) => {
                        if ready.send(Ok(state.hwnd as usize)).is_ok() && opened.recv().is_ok() {
                            state.run(&queue);
                        }
                    }
                    Err(error) => {
                        let _ = ready.send(Err(error));
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        let hwnd = started.await.map_err(|_| "tray thread stopped".to_owned())??;
        let frontend = Self {
            hwnd,
            commands,
            thread: std::sync::Mutex::new(Some(thread)),
        };
        let _ = activate.send(());
        Ok((frontend, asked))
    }
    fn send(&self, command: Command) {
        if self.commands.send(command).is_ok() {
            // SAFETY: a numeric handle only, never dereferenced. Windows rejects a closed HWND.
            unsafe {
                PostMessageW(self.hwnd as HWND, WAKE, 0, 0);
            }
        }
    }
    pub(crate) fn set_window(&self, hwnd: Option<usize>) {
        if let Some(hwnd) = hwnd
            && let Err(error) = watch_taskbar_button(hwnd as HWND, self.commands.clone(), self.hwnd)
        {
            log::warn!("taskbar button notifications could not be attached: {error}");
        }
        self.send(Command::Window(hwnd));
    }
}
impl NativeTray {
    pub(super) fn stop(&self) {
        self.send(Command::Stop);
        if let Some(thread) = self
            .thread
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = thread.join();
        }
    }
}
impl Drop for NativeTray {
    fn drop(&mut self) {
        self.stop();
    }
}
impl TrayIcon for NativeTray {
    fn set_unread(&self, unread: u64) -> BoxFuture<'static, ()> {
        self.send(Command::Unread(unread));
        Box::pin(async {})
    }
}
struct Icon(HICON);
impl Icon {
    fn new(unread: u64) -> io::Result<Self> {
        let picture = app_icon::pictures(unread)
            .into_iter()
            .find(|p| p.width() == 32)
            .ok_or_else(|| io::Error::other("missing tray icon"))?;
        let bgra: Vec<u8> = picture
            .pixels()
            .flat_map(|p| {
                let [r, g, b, a] = p.0;
                let premultiply = |c: u8| u8::try_from(u16::from(c) * u16::from(a) / 255).unwrap_or(0);
                [premultiply(b), premultiply(g), premultiply(r), a]
            })
            .collect();
        let mask = [0_u8; 128]; // 32x32 one-bit mask, all alpha supplied by the BGRA pixels.
        // SAFETY: exact 32x32 bitmap and DWORD-aligned AND mask, both alive until Windows copies them.
        let icon = unsafe { CreateIcon(std::ptr::null_mut(), 32, 32, 1, 32, mask.as_ptr(), bgra.as_ptr()) };
        if icon.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(icon))
        }
    }
}
impl Drop for Icon {
    fn drop(&mut self) {
        // SAFETY: this is our owned CreateIcon result, no longer installed in the shell.
        unsafe {
            DestroyIcon(self.0);
        }
    }
}
struct State {
    hwnd: HWND,
    app: String,
    events: async_channel::Sender<Event>,
    taskbar_created: u32,
    taskbar: ITaskbarList3,
    window: Option<usize>,
    icon: Icon,
    unread: u64,
    hosted: bool,
    #[cfg(test)]
    test_windows: Vec<HWND>,
    #[cfg(test)]
    overlay_count: std::cell::Cell<u64>,
}
impl State {
    fn new(app: String, events: async_channel::Sender<Event>) -> io::Result<Self> {
        let icon = Icon::new(0)?;
        // SAFETY: initialized COM apartment on our thread; no aggregation.
        let taskbar: ITaskbarList3 =
            unsafe { CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER) }.map_err(io::Error::other)?;
        // SAFETY: a valid newly created taskbar object.
        unsafe { taskbar.HrInit() }.map_err(io::Error::other)?;
        let hwnd = create_window()?;
        let taskbar_created = taskbar_created();
        let mut state = Self {
            hwnd,
            app,
            events,
            taskbar_created,
            taskbar,
            window: None,
            icon,
            unread: 0,
            hosted: false,
            #[cfg(test)]
            test_windows: Vec::new(),
            #[cfg(test)]
            overlay_count: std::cell::Cell::new(0),
        };
        state.install(NIM_ADD);
        if !state.hosted {
            return Err(io::Error::other("Windows shell refused the tray icon"));
        }
        Ok(state)
    }
    fn data(&self) -> NOTIFYICONDATAW {
        let mut data = NOTIFYICONDATAW {
            cbSize: u32::try_from(std::mem::size_of::<NOTIFYICONDATAW>()).unwrap_or(0),
            hWnd: self.hwnd,
            uID: ICON_ID,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: CALLBACK,
            hIcon: self.icon.0,
            ..Default::default()
        };
        for (slot, unit) in data
            .szTip
            .iter_mut()
            .take(127)
            .zip(title(&self.app, self.unread).encode_utf16())
        {
            *slot = unit;
        }
        data
    }
    fn install(&mut self, operation: u32) {
        let mut data = self.data();
        // SAFETY: initialized NOTIFYICONDATAW with live HWND and HICON.
        let mut added = operation == NIM_ADD;
        // SAFETY: same initialized data and owned icon/window as above.
        let mut up = unsafe { Shell_NotifyIconW(operation, &raw const data) } != 0;
        if !up {
            added = !added;
            let fallback = if added { NIM_ADD } else { NIM_MODIFY };
            // SAFETY: same owned icon; tolerate a duplicate broadcast or a shell disappearing before modify.
            up = unsafe { Shell_NotifyIconW(fallback, &raw const data) } != 0;
        }
        if up && added {
            data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            // SAFETY: this icon was just added. The shell requires the version on every add.
            unsafe {
                Shell_NotifyIconW(NIM_SETVERSION, &raw const data);
            }
        }
        if self.hosted != up {
            self.hosted = up;
            let _ = self.events.try_send(Event::Hosted(up));
        }
    }
    fn overlay(&self) -> windows::core::Result<()> {
        let Some(hwnd) = self.window else { return Ok(()) };
        let description = wide(&title(&self.app, self.unread));
        let icon = if self.unread == 0 {
            std::ptr::null_mut()
        } else {
            self.icon.0
        };
        // SAFETY: owned taskbar object; HWND belongs to our process, and Windows checks its continued validity.
        let result = unsafe {
            self.taskbar.SetOverlayIcon(
                ComHwnd(hwnd as _),
                windows::Win32::UI::WindowsAndMessaging::HICON(icon),
                windows::core::PCWSTR(description.as_ptr()),
            )
        };
        #[cfg(test)]
        if result.is_ok() {
            self.overlay_count.set(self.overlay_count.get() + 1);
        }
        if let Err(error) = &result {
            log::warn!("taskbar overlay failed ({:#x})", error.code().0);
        }
        result
    }

    fn run(&mut self, queue: &mpsc::Receiver<Command>) {
        let mut message = MSG::default();
        // SAFETY: initialized writable MSG; this thread owns the hidden window and pumps until shutdown.
        while unsafe { GetMessageW(&raw mut message, std::ptr::null_mut(), 0, 0) } > 0 {
            if message.hwnd == self.hwnd && message.message == WAKE {
                if !self.commands(queue) {
                    break;
                }
            } else if message.hwnd == self.hwnd && message.message == CALLBACK {
                self.callback(message.lParam);
            } else if message.message == self.taskbar_created {
                self.install(NIM_ADD);
                let _ = self.overlay();
            } else {
                // SAFETY: the MSG came from GetMessage on this thread.
                unsafe {
                    TranslateMessage(&raw const message);
                }
                // SAFETY: the same OS-supplied message.
                unsafe {
                    DispatchMessageW(&raw const message);
                }
            }
            // A modal popup may consume WAKE; drain after it returns too.
            if !self.commands(queue) {
                break;
            }
        }
    }
    fn commands(&mut self, queue: &mpsc::Receiver<Command>) -> bool {
        while let Ok(command) = queue.try_recv() {
            match command {
                #[cfg(test)]
                Command::TestWindow(commands, reply) => {
                    // SAFETY: borrowed module handle for the current process.
                    let module = unsafe { GetModuleHandleW(std::ptr::null()) };
                    // SAFETY: a synthetic top-level window owned and pumped by this thread; no account or media.
                    let hwnd = unsafe {
                        CreateWindowExW(
                            0,
                            wide("STATIC").as_ptr(),
                            wide("NGTEAMS SYNTHETIC DESKTOP").as_ptr(),
                            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                            120,
                            120,
                            640,
                            400,
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                            module,
                            std::ptr::null(),
                        )
                    };
                    watch_taskbar_button(hwnd, commands, self.hwnd as usize).unwrap();
                    self.test_windows.push(hwnd);
                    let _ = reply.send(hwnd as usize);
                }
                #[cfg(test)]
                Command::TestOverlay(reply) => {
                    let _ = reply.send(self.overlay().is_ok());
                }
                #[cfg(test)]
                Command::TestOverlayCount(reply) => {
                    let _ = reply.send(self.overlay_count.get());
                }
                Command::OverlayReady(hwnd) => {
                    if self.window == Some(hwnd) {
                        let _ = self.overlay();
                    }
                }
                Command::Stop => return false,
                Command::Unread(unread) => match Icon::new(unread) {
                    Ok(icon) => {
                        self.unread = unread;
                        let old = std::mem::replace(&mut self.icon, icon);
                        self.install(if self.hosted { NIM_MODIFY } else { NIM_ADD });
                        let _ = self.overlay();
                        drop(old);
                    }
                    Err(error) => log::warn!("tray icon failed: {error}"),
                },
                Command::Window(window) => {
                    self.window = window;
                    let _ = self.overlay();
                }
            }
        }
        true
    }
    fn callback(&self, parameter: LPARAM) {
        let event = u32::try_from(parameter & 0xffff).unwrap_or(0);
        match event {
            NIN_SELECT | NIN_KEYSELECT => {
                let _ = self.events.try_send(Event::Asked(TrayEvent::Open));
            }
            WM_CONTEXTMENU => self.menu(),
            _ => {}
        }
    }
    fn menu(&self) {
        // SAFETY: creates a popup owned by this thread, destroyed before returning.
        let menu = unsafe { CreatePopupMenu() };
        if menu.is_null() {
            return;
        }
        // SAFETY: a live owned menu and terminated label.
        unsafe {
            AppendMenuW(menu, MF_STRING, 1, wide("Open Teams").as_ptr());
        }
        // SAFETY: a live owned menu and terminated label.
        unsafe {
            AppendMenuW(menu, MF_STRING, 2, wide("Quit").as_ptr());
        }
        // SAFETY: item 1 is present in the menu.
        unsafe {
            SetMenuDefaultItem(menu, 1, 0);
        }
        let mut position = POINT::default();
        // SAFETY: initialized writable point.
        unsafe {
            GetCursorPos(&raw mut position);
        }
        // SAFETY: this thread's owned hidden window; required for popup dismissal.
        unsafe {
            SetForegroundWindow(self.hwnd);
        }
        // SAFETY: owned menu and window. Returns the selected id without borrowing State in callbacks.
        let selected = unsafe {
            TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                position.x,
                position.y,
                0,
                self.hwnd,
                std::ptr::null(),
            )
        };
        // SAFETY: this thread's live window; required by the shell popup-dismissal contract.
        unsafe {
            PostMessageW(self.hwnd, WM_NULL, 0, 0);
        }
        // SAFETY: the popup is finished and its owned menu is no longer used.
        unsafe {
            DestroyMenu(menu);
        }
        let event = match selected {
            1 => Some(TrayEvent::Open),
            2 => Some(TrayEvent::Quit),
            _ => None,
        };
        if let Some(event) = event {
            let _ = self.events.try_send(Event::Asked(event));
        }
        let data = self.data();
        // SAFETY: initialized data for our installed tray item; return keyboard focus after its popup.
        unsafe {
            Shell_NotifyIconW(NIM_SETFOCUS, &raw const data);
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        let data = self.data();
        // SAFETY: withdraw the installed shell copy before dropping our icon and window.
        unsafe {
            Shell_NotifyIconW(NIM_DELETE, &raw const data);
        }
        #[cfg(test)]
        for hwnd in &self.test_windows {
            // SAFETY: synthetic windows owned by this thread, never reused after State drops.
            unsafe {
                DestroyWindow(*hwnd);
            }
        }
        // SAFETY: the hidden window belongs to this thread and its loop has ended.
        unsafe {
            DestroyWindow(self.hwnd);
        }
    }
}
fn create_window() -> io::Result<HWND> {
    let class = wide("NgteamsDesktopTray");
    // SAFETY: borrowed current-process module handle, retained for the entire process.
    let module = unsafe { GetModuleHandleW(std::ptr::null()) };
    let window_class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: module,
        lpszClassName: class.as_ptr(),
        ..Default::default()
    };
    // SAFETY: valid class descriptor, stateless procedure and terminated class name. May already be registered.
    unsafe {
        RegisterClassW(&raw const window_class);
    }
    // SAFETY: registered class, borrowed module and no parent/menu/user-data pointers.
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class.as_ptr(),
            class.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            module,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(hwnd)
    }
}
unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // GetMessage dispatches cross-thread SendMessage/SendNotifyMessage inside the call, without returning them to
    // State::run. Queue the shell's numeric callbacks/broadcasts so the same loop handles both delivery methods.
    if (message == CALLBACK || (message != 0 && message == taskbar_created()))
        // SAFETY: read-only query of this thread's current window-procedure invocation.
        && unsafe { InSendMessageEx(std::ptr::null()) } != 0
    {
        // SAFETY: this procedure's live HWND; shell parameters are integers, never borrowed pointers.
        unsafe { PostMessageW(hwnd, message, wparam, lparam) };
        return 0;
    }
    if message == WAKE {
        // SAFETY: end only this thread's popup, so shutdown cannot wait on an open tray menu.
        unsafe {
            EndMenu();
        }
    }
    // SAFETY: forwards the window procedure's unchanged arguments to the OS default procedure.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

#[cfg(test)]
impl NativeTray {
    /// The HWND this fixture's tray currently targets.
    ///
    /// # Panics
    /// The owned tray loop stopped before answering the fixture.
    pub async fn test_window(&self) -> usize {
        let (reply, result) = oneshot::channel();
        self.send(Command::TestWindow(self.commands.clone(), reply));
        result.await.unwrap()
    }
    /// Whether the shell accepted this fixture's most recent overlay update.
    ///
    /// # Panics
    /// The owned tray loop stopped before answering the fixture.
    pub async fn test_overlay(&self) -> bool {
        let (reply, result) = oneshot::channel();
        self.send(Command::TestOverlay(reply));
        result.await.unwrap()
    }
    /// Successful native overlay applications, without applying another overlay for the query.
    ///
    /// # Panics
    /// The owned tray loop stopped before answering the fixture.
    pub async fn test_overlay_count(&self) -> u64 {
        let (reply, result) = oneshot::channel();
        self.send(Command::TestOverlayCount(reply));
        result.await.unwrap()
    }
    pub fn test_button_ready(hwnd: usize) {
        // SAFETY: fixture supplies its own synthetic window, never an Explorer window.
        unsafe {
            SendMessageW(hwnd as HWND, taskbar_button_created(), 0, 0);
        }
    }
    pub fn test_callback(&self, event: u32) {
        // SAFETY: the fixture targets only the HWND this object created.
        unsafe {
            SendMessageW(self.hwnd as HWND, CALLBACK, 0, isize::try_from(event).unwrap_or(0));
        }
    }
    pub fn test_explorer_restart(&self) {
        let message = taskbar_created();
        // SAFETY: simulate delivery only to our own hidden window; Explorer itself is never restarted.
        unsafe {
            SendMessageW(self.hwnd as HWND, message, 0, 0);
        }
    }
}
