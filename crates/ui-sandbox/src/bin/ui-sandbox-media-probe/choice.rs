//! The probe's commands about which devices there are and which one a device uses: the list, a node coming and
//! going, a microphone and a speaker moved to another node while they run (chosen, or following the system's default),
//! a chosen microphone staying on its device when another becomes the default and falling back when it is unplugged,
//! and the camera through the sandbox's camera portal.

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use teams_media::devices::catalog::{DeviceDirectory, DeviceKind, DeviceList};
use teams_media::devices::pipewire::{PipeWireDevices, Reach};
use teams_media::devices::{AudioFrame, AudioSink, AudioSource, DeviceError, Devices, FRAME_DURATION, FRAME_SAMPLES};
use teams_media::tone::{Tone, ToneGenerator, amplitude_at};

use super::{ProbeResult, SETTLE_FRAMES, WAIT, listen};

/// A 200 ms window of the microphone.
const WINDOW_FRAMES: usize = 20;
/// A tone this loud in a window is there; one this quiet isn't.
const THERE: f32 = 0.1;
const GONE: f32 = 0.03;
/// How long a choice gets to settle before the test plugs in another device.
const PIN_SETTLE: Duration = Duration::from_secs(2);
/// How long a chosen microphone must stay put once another device became the default.
const PIN_HOLD: Duration = Duration::from_secs(3);

/// Prints the devices there are, one `device=<kind>:<name>:<description>` line each, and each kind's default.
pub fn devices(devices: &PipeWireDevices) {
    let list = devices.devices();
    for device in list.all() {
        println!("device={}:{}:{}", device.kind.key(), device.name, device.description);
    }
    for kind in DeviceKind::ALL {
        println!("default_{}={}", kind.key(), list.default_name(kind).unwrap_or("-"));
    }
}

/// Says `listening=1`, then waits (up to `seconds`) for a node named `name` to come (`added=<kind>`, and
/// `default=1` once it is its kind's default) and to go again (`removed=1`).
pub fn watch_node(devices: &PipeWireDevices, name: &str, seconds: u64) -> ProbeResult {
    let (told, lists) = mpsc::channel();
    devices.watch(Box::new(move |list: &DeviceList| {
        let _ = told.send(list.clone());
    }));
    println!("listening=1");
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let (mut added, mut default) = (None, false);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let list = lists
            .recv_timeout(left)
            .map_err(|_| format!("{name} didn't come and go in time"))?;
        let device = list.all().iter().find(|device| device.name == name);
        match (device, added) {
            (Some(device), None) => {
                added = Some(device.kind);
                println!("added={}", device.kind.key());
            }
            (None, Some(_)) => {
                println!("removed=1");
                return Ok(());
            }
            _ => {}
        }
        if let Some(kind) = added
            && !default
            && list.default_name(kind) == Some(name)
        {
            default = true;
            println!("default=1");
        }
    }
}

/// A microphone read window by window, keeping the longest wait between two of its frames.
struct Listening {
    microphone: Box<dyn AudioSource>,
    last: Instant,
    longest_gap: Duration,
}

impl Listening {
    fn new(mut microphone: Box<dyn AudioSource>) -> ProbeResult<Self> {
        listen(microphone.as_mut(), SETTLE_FRAMES)?;
        Ok(Self {
            microphone,
            last: Instant::now(),
            longest_gap: Duration::ZERO,
        })
    }

    /// The levels at `hz` over the next 200 ms; fails if the microphone closed or stopped.
    fn window(&mut self, hz: [f32; 2]) -> ProbeResult<[f32; 2]> {
        let mut samples = Vec::with_capacity(WINDOW_FRAMES * FRAME_SAMPLES);
        for _ in 0..WINDOW_FRAMES {
            let frame = self.microphone.next_frame(WAIT)?.ok_or("the microphone stopped")?;
            self.longest_gap = self.longest_gap.max(self.last.elapsed());
            self.last = Instant::now();
            samples.extend_from_slice(frame.samples());
        }
        Ok(hz.map(|hz| amplitude_at(&samples, hz)))
    }

    /// Reads until a window holds `wanted` and not `other` (`[wanted, other]` as `hz` gives them), within `within`;
    /// how long it took.
    fn until_heard(&mut self, hz: [f32; 2], within: Duration) -> ProbeResult<Duration> {
        let started = Instant::now();
        while started.elapsed() < within {
            let [wanted, other] = self.window(hz)?;
            if wanted > THERE && other < GONE {
                return Ok(started.elapsed());
            }
        }
        Err(format!("{} Hz alone wasn't heard within {within:?}", hz[0]).into())
    }
}

/// The microphone following the system: says `listening=1` once `system_hz` is heard, `moved=1` once only
/// `added_hz` is (the test added a node above the system's), `returned=1` once `system_hz` alone is again (the
/// test removed it); the microphone never closed or stopped, and the longest wait for a frame is printed.
pub fn follow_mic(devices: &PipeWireDevices, system_hz: f32, added_hz: f32, seconds: u64) -> ProbeResult {
    let within = Duration::from_secs(seconds);
    let mut listening = Listening::new(devices.microphone()?)?;
    listening.until_heard([system_hz, added_hz], within)?;
    println!("listening=1");
    let moved = listening.until_heard([added_hz, system_hz], within)?;
    println!("moved_ms={}", moved.as_millis());
    println!("moved=1");
    let returned = listening.until_heard([system_hz, added_hz], within)?;
    println!("returned_ms={}", returned.as_millis());
    println!("max_gap_ms={}", listening.longest_gap.as_millis());
    println!("returned=1");
    Ok(())
}

/// The microphone moved by the user's choice while it runs: `system_hz` alone on the system's microphone, then
/// `chosen_hz` alone once `name` is chosen, then `system_hz` alone again once the choice goes back to the system's;
/// prints how long each move took to be heard and the longest wait for a frame throughout.
pub fn switch_mic(devices: &PipeWireDevices, name: &str, system_hz: f32, chosen_hz: f32) -> ProbeResult {
    const WITHIN: Duration = Duration::from_secs(10);
    let mut listening = Listening::new(devices.microphone()?)?;
    listening.until_heard([system_hz, chosen_hz], WITHIN)?;
    devices.choose(DeviceKind::Microphone, Some(name));
    let chosen_now = devices.chosen(DeviceKind::Microphone).unwrap_or_default();
    let chosen = listening.until_heard([chosen_hz, system_hz], WITHIN)?;
    devices.choose(DeviceKind::Microphone, None);
    let back = listening.until_heard([system_hz, chosen_hz], WITHIN)?;
    println!("chosen_ms={}", chosen.as_millis());
    println!("back_ms={}", back.as_millis());
    println!("max_gap_ms={}", listening.longest_gap.as_millis());
    println!("chosen={chosen_now}");
    Ok(())
}

/// A device chosen while it is the system's default stays chosen: the microphone is moved to `chosen` (`chosen_hz`)
/// by the user's choice and says `pinned=1` once the move has settled; the test then plugs in `added` (`added_hz`)
/// above the system's devices, and once the system's default is `added` the microphone must hear `chosen_hz` alone
/// for [`PIN_HOLD`] (`kept=1`). Then `added` is chosen (`moved=1` once it alone is heard), the test unplugs it, and
/// the microphone falls back to the system's default without closing (`fell_back=1`); the longest wait for a frame
/// is printed.
pub fn pinned_mic(devices: &PipeWireDevices, chosen: (&str, f32), added: (&str, f32), seconds: u64) -> ProbeResult {
    let within = Duration::from_secs(seconds);
    let ((chosen, chosen_hz), (added, added_hz)) = (chosen, added);
    let mut listening = Listening::new(devices.microphone()?)?;
    listening.until_heard([chosen_hz, added_hz], within)?;
    devices.choose(DeviceKind::Microphone, Some(chosen));
    // the replacement swapped in, the old stream gone, and the session manager's rescans after both done
    let settling = Instant::now();
    while settling.elapsed() < PIN_SETTLE {
        listening.window([chosen_hz, added_hz])?;
    }
    println!("pinned=1");
    let waiting = Instant::now();
    while devices.devices().default_name(DeviceKind::Microphone) != Some(added) {
        if waiting.elapsed() > within {
            return Err(format!("{added} didn't become the default in time").into());
        }
        listening.window([chosen_hz, added_hz])?;
    }
    let holding = Instant::now();
    while holding.elapsed() < PIN_HOLD {
        let [heard, other] = listening.window([chosen_hz, added_hz])?;
        if heard < THERE || other > GONE {
            return Err(format!(
                "the chosen {chosen} was left for the new default {added} after {:?}: {chosen_hz} Hz at {heard}, \
                 {added_hz} Hz at {other}",
                holding.elapsed()
            )
            .into());
        }
    }
    println!("kept=1");
    devices.choose(DeviceKind::Microphone, Some(added));
    listening.until_heard([added_hz, chosen_hz], within)?;
    println!("moved=1");
    let fell_back = listening.until_heard([chosen_hz, added_hz], within)?;
    println!("fell_back_ms={}", fell_back.as_millis());
    println!("max_gap_ms={}", listening.longest_gap.as_millis());
    println!("fell_back=1");
    Ok(())
}

/// Plays `tone` for `duration` at the device's pace, from `generator`; fails if the speaker says it closed.
fn play_for(speaker: &mut dyn AudioSink, generator: &mut ToneGenerator, duration: Duration) -> ProbeResult {
    let frames = u32::try_from(duration.as_millis() / FRAME_DURATION.as_millis())?;
    let start = Instant::now();
    let mut frame = AudioFrame::silence();
    for index in 0..frames {
        generator.fill(frame.samples_mut());
        speaker.play(&frame)?;
        let due = start + FRAME_DURATION * (index + 1);
        thread::sleep(due.saturating_duration_since(Instant::now()));
    }
    Ok(())
}

/// The speaker moved by the user's choice while it plays `tone`: on the system's speaker for `each`, then (after
/// `phase=chosen`) on `name` for `each`, then (after `phase=back`) on the system's again for `each`; the speaker
/// never says it closed. The test reads both recordings at each phase.
pub fn switch_speaker(devices: &PipeWireDevices, name: &str, tone: Tone, each: Duration) -> ProbeResult {
    let mut speaker = devices.speaker()?;
    let mut generator = ToneGenerator::new(tone);
    println!("phase=system");
    play_for(speaker.as_mut(), &mut generator, each)?;
    devices.choose(DeviceKind::Speaker, Some(name));
    println!("phase=chosen");
    play_for(speaker.as_mut(), &mut generator, each)?;
    devices.choose(DeviceKind::Speaker, None);
    println!("phase=back");
    play_for(speaker.as_mut(), &mut generator, each)?;
    println!("phase=done");
    Ok(())
}

/// The camera through the camera portal on the sandbox's bus: `frames` of them within `seconds`, or
/// `camera=refused` when the portal (the user) said no.
/// The screen through the screen-cast portal on the sandbox's bus (sandbox reach asks only that one): `screen=open`,
/// its first frame's size and how many frames came, each a later one of the pattern than the one before; or
/// `screen=refused` and why.
pub fn portal_screen(wanted: u64, seconds: u64) -> ProbeResult {
    let devices = PipeWireDevices::connect(Reach::Sandbox)?;
    let mut screen = match devices.screen() {
        Ok(screen) => screen,
        Err(DeviceError::Refused(why)) => {
            println!("screen=refused");
            println!("why={why}");
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let (mut frames, mut size, mut last, mut in_order) = (0, None, None, true);
    while frames < wanted && Instant::now() < deadline {
        let Some(frame) = screen.next_frame(WAIT)? else {
            continue;
        };
        frames += 1;
        size.get_or_insert((frame.width, frame.height));
        let index = teams_media::devices::pattern::index_of(&frame);
        in_order &= last.is_none_or(|last| index > Some(last));
        last = index.or(last);
    }
    println!("screen=open");
    if let Some((width, height)) = size {
        println!("size={width}x{height}");
    }
    println!("frames={frames}");
    println!("in_order={}", u8::from(in_order));
    Ok(())
}

pub fn portal_camera(wanted: u64, seconds: u64) -> ProbeResult {
    let bus = std::env::var("DBUS_SESSION_BUS_ADDRESS").map_err(|_| "no bus")?;
    let devices = PipeWireDevices::connect_with_camera_portal(Reach::Sandbox, &bus)?;
    let mut camera = match devices.camera() {
        Ok(camera) => camera,
        Err(DeviceError::Refused(why)) => {
            println!("camera=refused");
            println!("why={why}");
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut frames = 0;
    while frames < wanted && Instant::now() < deadline {
        if camera.next_frame(WAIT)?.is_some() {
            frames += 1;
        }
    }
    println!("camera=open");
    println!("frames={frames}");
    Ok(())
}
