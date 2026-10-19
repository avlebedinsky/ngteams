//! When the GPU finished a submission: a timeline semaphore of the renderer's own, signalled by each submission that
//! copies or first draws an external frame (wgpu-hal's `add_signal_semaphore`), read for retiring imported images and
//! waited on by a thread that reports frames. The thread waits with `vkWaitSemaphores`, outside every wgpu lock:
//! `Device::poll` holds the device's fence lock while it waits, which would stall the next `Queue::submit`.

use super::Callback;
use anyhow::{Context as _, Result, bail};
use ash::vk;
use gpui::FrameOutcome;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use wgpu::hal::vulkan;

/// How long a wait goes on before it is logged, and how long a shutdown waits for submissions still running.
const PATIENCE: Duration = Duration::from_secs(10);

/// The renderer's timeline semaphore and the thread reporting frames once the GPU reached their value.
pub(super) struct Timeline {
    semaphore: vk::Semaphore,
    raw: ash::Device,
    /// The value the last signalling submission was given.
    last: u64,
    sender: Option<mpsc::Sender<(u64, Vec<Callback>)>>,
    thread: Option<std::thread::JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
    /// Keeps the device (and so `raw` and `semaphore`) alive until the thread ended.
    _device: wgpu::Device,
}

impl Timeline {
    /// A timeline on `device`; an error on a device that is not Vulkan or cannot make one.
    pub(super) fn new(device: &wgpu::Device) -> Result<Self> {
        let raw = {
            // SAFETY: the guard is dropped at the end of this block; the ash device it hands out stays valid while
            // `device` lives, which this value keeps.
            let hal_device = unsafe { device.as_hal::<vulkan::Api>() }.context("the device is not Vulkan")?;
            hal_device.raw_device().clone()
        };
        let mut timeline = vk::SemaphoreTypeCreateInfo::default()
            .semaphore_type(vk::SemaphoreType::TIMELINE)
            .initial_value(0);
        // SAFETY: the device was opened with Vulkan 1.2 and the timeline semaphore feature (`missing_requirements`).
        let semaphore = unsafe {
            raw.create_semaphore(&vk::SemaphoreCreateInfo::default().push_next(&mut timeline), None)
        }
        .context("vkCreateSemaphore (timeline)")?;
        let (sender, receiver) = mpsc::channel::<(u64, Vec<Callback>)>();
        let shutdown = Arc::new(AtomicBool::new(false));
        let waiter = raw.clone();
        let stopping = Arc::clone(&shutdown);
        let thread = std::thread::Builder::new()
            .name("gpui-external-surfaces".to_owned())
            .spawn(move || report(&waiter, semaphore, &receiver, &stopping));
        let thread = match thread {
            Ok(thread) => thread,
            Err(error) => {
                // SAFETY: the semaphore was created above and nothing waits on or signals it.
                unsafe { raw.destroy_semaphore(semaphore, None) };
                bail!("the completion thread did not start: {error}");
            }
        };
        Ok(Self {
            semaphore,
            raw,
            last: 0,
            sender: Some(sender),
            thread: Some(thread),
            shutdown,
            _device: device.clone(),
        })
    }

    /// Has the next submission on `queue` signal a new value, which it returns.
    pub(super) fn signal(&mut self, queue: &wgpu::Queue) -> Option<u64> {
        // SAFETY: the guard is only used to queue the signal and is dropped before the submission.
        let hal_queue = unsafe { queue.as_hal::<vulkan::Api>() }?;
        self.last += 1;
        hal_queue.add_signal_semaphore(self.semaphore, Some(self.last));
        Some(self.last)
    }

    /// Reports `callbacks` once the GPU reached `value`.
    pub(super) fn complete_at(&self, value: u64, callbacks: Vec<Callback>) {
        if callbacks.is_empty() {
            return;
        }
        if let Some(sender) = &self.sender
            && let Err(mpsc::SendError((_, callbacks))) = sender.send((value, callbacks))
        {
            for callback in callbacks {
                callback(FrameOutcome::Lost("the completion thread ended".to_owned()));
            }
        }
    }

    /// The value the GPU has reached (0 when it cannot be read).
    pub(super) fn reached(&self) -> u64 {
        // SAFETY: the semaphore is a timeline semaphore of this device, alive while `self` is.
        unsafe { self.raw.get_semaphore_counter_value(self.semaphore) }.unwrap_or(0)
    }

    /// Waits, a bounded while, until the GPU reached every value signalled so far; whether it did.
    pub(super) fn settle(&self) -> bool {
        let semaphores = [self.semaphore];
        let values = [self.last];
        let info = vk::SemaphoreWaitInfo::default().semaphores(&semaphores).values(&values);
        // SAFETY: the semaphore and device are alive while `self` is; waiting needs no external synchronization.
        unsafe { self.raw.wait_semaphores(&info, PATIENCE.as_nanos() as u64) }.is_ok()
    }
}

impl Drop for Timeline {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        self.sender = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if self.reached() >= self.last {
            // SAFETY: the GPU reached every value any submission signals, so no queue operation refers to the
            // semaphore any more, and the thread that waited on it has ended.
            unsafe { self.raw.destroy_semaphore(self.semaphore, None) };
        } else {
            log::warn!("external surfaces: a submission never finished; its timeline semaphore is leaked");
        }
    }
}

/// The completion thread: for each batch in order, waits until the GPU reached its value and reports its frames:
/// `Presented`, or `Lost` when the device was lost or a shutdown gave up waiting.
fn report(
    raw: &ash::Device,
    semaphore: vk::Semaphore,
    receiver: &mpsc::Receiver<(u64, Vec<Callback>)>,
    shutdown: &AtomicBool,
) {
    // One shutdown deadline for the whole thread, so a stalled GPU delays a teardown by PATIENCE, not per batch.
    let mut shutdown_since: Option<Instant> = None;
    for (value, callbacks) in receiver {
        let outcome = match wait_value(raw, semaphore, value, shutdown, &mut shutdown_since) {
            Ok(()) => FrameOutcome::Presented,
            Err(error) => FrameOutcome::Lost(format!("{error:#}")),
        };
        for callback in callbacks {
            callback(outcome.clone());
        }
    }
}

/// Waits until the GPU reached `value`. A slow GPU is waited for (logged once after [`PATIENCE`]); only a lost
/// device, or a shutdown that has waited [`PATIENCE`] since it began (`shutdown_since`, shared by every wait of the
/// thread), ends the wait without it — and once that deadline passed, every later wait only checks, without waiting.
fn wait_value(
    raw: &ash::Device,
    semaphore: vk::Semaphore,
    value: u64,
    shutdown: &AtomicBool,
    shutdown_since: &mut Option<Instant>,
) -> Result<()> {
    let semaphores = [semaphore];
    let values = [value];
    let info = vk::SemaphoreWaitInfo::default().semaphores(&semaphores).values(&values);
    let started = Instant::now();
    let mut logged = false;
    loop {
        let expired = shutdown_since.is_some_and(|since| since.elapsed() >= PATIENCE);
        let timeout = if expired { 0 } else { Duration::from_millis(100).as_nanos() as u64 };
        // SAFETY: the semaphore and device stay alive while this thread runs (`Timeline` joins it before destroying
        // the semaphore and keeps the device); waiting needs no external synchronization.
        match unsafe { raw.wait_semaphores(&info, timeout) } {
            Ok(()) => return Ok(()),
            Err(vk::Result::TIMEOUT) => {}
            Err(error) => bail!("waiting for the frame's submission: {error}"),
        }
        if expired {
            bail!("the renderer shut down before the GPU finished the frame");
        }
        if !logged && started.elapsed() >= PATIENCE {
            log::warn!("external surfaces: the GPU has not finished a frame for {PATIENCE:?}; still waiting");
            logged = true;
        }
        if shutdown.load(Ordering::Relaxed) {
            shutdown_since.get_or_insert_with(Instant::now);
        }
    }
}
