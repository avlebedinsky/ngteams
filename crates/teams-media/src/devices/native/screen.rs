//! The system picker and capture run on the same owned thread: the picker's owner HWND and `WinRT` apartment must
//! not cross threads. Dropping the source posts `WM_QUIT` even when the shared window is static and sends no frames.
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::graphics_capture_picker::{Error as PickerError, GraphicsCapturePicker};
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings, MinimumUpdateIntervalSettings,
    SecondaryWindowSettings, Settings,
};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, WM_QUIT};

use super::error;
use crate::devices::{DeviceError, PixelFormat, VideoFrame, VideoSource};
use crate::queue::Queue;

struct Flags {
    frames: Queue<VideoFrame>,
    ready: mpsc::SyncSender<Result<u32, String>>,
    thread: u32,
}
struct Capture {
    frames: Queue<VideoFrame>,
    packed: Vec<u8>,
}
impl GraphicsCaptureApiHandler for Capture {
    type Flags = Flags;
    type Error = DeviceError;
    fn new(ctx: Context<Flags>) -> Result<Self, DeviceError> {
        let _ = ctx.flags.ready.send(Ok(ctx.flags.thread));
        Ok(Self {
            frames: ctx.flags.frames,
            packed: Vec::new(),
        })
    }
    fn on_frame_arrived(&mut self, frame: &mut Frame<'_>, _control: InternalCaptureControl) -> Result<(), DeviceError> {
        let (width, height) = (frame.width(), frame.height());
        let stride = width.checked_mul(4).ok_or_else(|| error("screen width overflow"))?;
        let buffer = frame.buffer().map_err(error)?;
        let data = buffer.as_nopadding_buffer(&mut self.packed).to_vec();
        self.frames.push(VideoFrame {
            width,
            height,
            stride,
            format: PixelFormat::Bgra,
            data,
        });
        Ok(())
    }
    fn on_closed(&mut self) -> Result<(), DeviceError> {
        self.frames.close();
        Ok(())
    }
}

struct Screen {
    frames: Queue<VideoFrame>,
    thread: Option<JoinHandle<()>>,
    id: u32,
}
impl VideoSource for Screen {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<VideoFrame>, DeviceError> {
        self.frames.recv(timeout)
    }
}
impl Drop for Screen {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            // SAFETY: the retained JoinHandle keeps this thread's kernel object (and ID) alive. Its queue is
            // created before publishing the ID. No window pointer or borrowed memory crosses this call.
            if !thread.is_finished() {
                // SAFETY: the owned thread's queue exists and the message carries no borrowed memory.
                unsafe {
                    PostThreadMessageW(self.id, WM_QUIT, 0, 0);
                }
            }
            let _ = thread.join();
        }
    }
}

pub(super) fn open() -> Result<Box<dyn VideoSource>, DeviceError> {
    let frames = Queue::new(3);
    let (write, close) = (frames.clone(), frames.clone());
    let (ready, response) = mpsc::sync_channel(1);
    let thread = thread::Builder::new()
        .name("windows-share-picker".into())
        .spawn(move || {
            let result = run(write, &ready);
            if let Err(failure) = result {
                let _ = ready.try_send(Err(failure.to_string()));
            }
            close.close();
        })?;
    match response.recv() {
        Ok(Ok(id)) => Ok(Box::new(Screen {
            frames,
            thread: Some(thread),
            id,
        })),
        result => {
            let _ = thread.join();
            Err(error(match result {
                Ok(Err(failure)) => failure,
                _ => "screen capture ended before it started".into(),
            }))
        }
    }
}

fn run(frames: Queue<VideoFrame>, ready: &mpsc::SyncSender<Result<u32, String>>) -> Result<(), DeviceError> {
    let item = match GraphicsCapturePicker::pick_item() {
        Ok(Some(item)) => item,
        Ok(None) | Err(PickerError::Canceled) => {
            return Err(DeviceError::Refused("no screen was picked to share".into()));
        }
        Err(failure) => return Err(error(failure)),
    };
    let mut message = MSG::default();
    // SAFETY: message is a valid output buffer. PeekMessage creates this thread's queue without consuming messages.
    unsafe {
        PeekMessageW(&raw mut message, std::ptr::null_mut(), 0, 0, PM_NOREMOVE);
    }
    // SAFETY: querying the current thread's numeric ID has no preconditions.
    let id = unsafe { GetCurrentThreadId() };
    let settings = Settings::new(
        item,
        CursorCaptureSettings::WithCursor,
        DrawBorderSettings::WithBorder,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        Flags {
            frames,
            ready: ready.clone(),
            thread: id,
        },
    );
    Capture::start(settings).map_err(error)
}

#[cfg(test)]
mod tests {
    use super::open;
    use crate::devices::PixelFormat;
    use std::time::Duration;

    /// Offline hardware contract: the agent picks only a VM console window or monitor, never a tenant target.
    #[test]
    #[ignore = "requires the authorized win11 console and a human/agent selection in the system picker"]
    fn system_picker_delivers_bgra_and_drop_stops_a_static_capture() {
        teams_platform::windows_sandbox::window_allowed_here().unwrap();
        let mut screen = open().unwrap();
        let frame = screen.next_frame(Duration::from_secs(10)).unwrap().unwrap();
        assert_eq!(frame.format, PixelFormat::Bgra);
        assert!(frame.width > 0 && frame.height > 0);
        assert_eq!(frame.stride, frame.width * 4);
        assert_eq!(frame.data.len(), (frame.stride * frame.height) as usize);
        // The console runner displays only its own dark-blue fixture window; this also verifies BGRA channel order.
        let center =
            usize::try_from(u64::from(frame.height / 2) * u64::from(frame.stride) + u64::from(frame.width / 2) * 4)
                .unwrap();
        assert!(frame.data[center] > 80 && frame.data[center + 1] < 20 && frame.data[center + 2] < 20);
        if let Some(path) = std::env::var_os("NGTEAMS_MEDIA_PICKER_SHOT") {
            let mut rgba = frame.data.clone();
            for pixel in rgba.as_chunks_mut::<4>().0 {
                pixel.swap(0, 2);
            }
            image::save_buffer_with_format(
                path,
                &rgba,
                frame.width,
                frame.height,
                image::ColorType::Rgba8,
                image::ImageFormat::Png,
            )
            .unwrap();
        }
        let stopped = std::time::Instant::now();
        drop(screen);
        assert!(stopped.elapsed() < Duration::from_secs(3));
    }
}
