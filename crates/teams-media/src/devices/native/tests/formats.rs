//! The device formats that are common on Windows, in both directions. A tone that the speaker plays reaches a device of
//! each format whole, and a microphone of each format gives the call whole 48 kHz mono frames of its tone. The pipelines
//! ask each device for its own format and convert. They never assume one.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::OnceLock;

use gstreamer_app::{AppSink, AppSinkCallbacks};

use super::*;
use crate::devices::{AudioFrame, FRAME_DURATION, SAMPLE_RATE};
use crate::tone::{Tone, ToneGenerator, purity};

pub(super) const MICROPHONE: &str = "format-microphone ";
pub(super) const SPEAKER: &str = "format-speaker ";

const TONE: Tone = Tone {
    hz: 1000.0,
    amplitude: 0.5,
};

/// The shared-mode mix formats of Windows endpoints, and the formats of USB and Bluetooth hands-free headsets.
const FORMATS: [(&str, i32, i32); 8] = [
    ("F32LE", 48_000, 2),
    ("F32LE", 44_100, 2),
    ("S16LE", 44_100, 2),
    ("S16LE", 48_000, 1),
    ("S24LE", 48_000, 2),
    ("S32LE", 96_000, 2),
    ("F32LE", 16_000, 1),
    ("S16LE", 8_000, 1),
];

fn caps(format: &str, rate: i32, channels: i32) -> String {
    format!("audio/x-raw,format={format},rate={rate},channels={channels},layout=interleaved")
}

/// What each synthetic speaker received, by the label of its device.
fn received() -> &'static Mutex<HashMap<String, Vec<u8>>> {
    static RECEIVED: OnceLock<Mutex<HashMap<String, Vec<u8>>>> = OnceLock::new();
    RECEIVED.get_or_init(Mutex::default)
}

pub(super) fn microphone(caps: &str) -> Result<gst::Element, glib::BoolError> {
    let bin = gst::Bin::new();
    let source = gst::ElementFactory::make("audiotestsrc")
        .property("is-live", true)
        .property("freq", f64::from(TONE.hz))
        .property("volume", f64::from(TONE.amplitude))
        .build()?;
    let filter = gst::ElementFactory::make("capsfilter")
        .property("caps", gst::Caps::from_str(caps)?)
        .build()?;
    bin.add_many([&source, &filter])?;
    source.link(&filter)?;
    let pad = filter
        .static_pad("src")
        .ok_or_else(|| glib::bool_error!("no source pad"))?;
    bin.add_pad(&gst::GhostPad::with_target(&pad)?)?;
    Ok(bin.upcast())
}

pub(super) fn speaker(label: &str, caps: &str) -> gst::Element {
    let sink = AppSink::builder().caps(&gst::Caps::from_str(caps).unwrap()).build();
    let label = label.to_owned();
    let callbacks = AppSinkCallbacks::builder().new_sample(move |sink| {
        let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
        let map = sample.buffer().unwrap().map_readable().unwrap();
        lock(received())
            .entry(label.clone())
            .or_default()
            .extend_from_slice(&map);
        Ok(gst::FlowSuccess::Ok)
    });
    sink.set_callbacks(callbacks.build());
    sink.upcast()
}

/// The first channel of interleaved `bytes` in `format`, as samples in `[-1, 1]`.
#[expect(clippy::cast_possible_truncation, reason = "audio samples fit an f32")]
fn first_channel(bytes: &[u8], format: &str, channels: i32) -> Vec<f32> {
    let width = match format {
        "S16LE" => 2,
        "S24LE" => 3,
        _ => 4,
    };
    let frame = width * usize::try_from(channels).unwrap();
    let sample = |s: &[u8]| -> f64 {
        match format {
            "S16LE" => f64::from(i16::from_le_bytes([s[0], s[1]])) / 32_768.0,
            "S24LE" => f64::from(i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8) / 8_388_608.0,
            "S32LE" => f64::from(i32::from_le_bytes([s[0], s[1], s[2], s[3]])) / 2_147_483_648.0,
            _ => f64::from(f32::from_le_bytes([s[0], s[1], s[2], s[3]])),
        }
    };
    bytes.chunks_exact(frame).map(|f| sample(&f[..width]) as f32).collect()
}

/// Each 20 ms window of the tone in `samples` at `rate` (less one at each edge): its amplitude and purity in dB.
fn windows(samples: &[f32], rate: i32) -> Vec<(f32, f32)> {
    let size = usize::try_from(rate / 50).unwrap();
    // `purity` reads at 48 kHz: the same tone at `rate` is that many periods in a window
    #[expect(clippy::cast_precision_loss, reason = "sample rates are far below 2^23")]
    let hz = TONE.hz * SAMPLE_RATE as f32 / rate as f32;
    let fits: Vec<(f32, f32)> = samples.chunks_exact(size).map(|w| purity(w, hz)).collect();
    let audible = |(amplitude, _): &(f32, f32)| *amplitude > TONE.amplitude / 4.0;
    let (Some(first), Some(last)) = (fits.iter().position(audible), fits.iter().rposition(audible)) else {
        return Vec::new();
    };
    fits.get(first + 1..last).unwrap_or_default().to_vec()
}

fn assert_whole(fits: &[(f32, f32)], what: &str) {
    assert!(fits.len() >= 10, "{what}: only {} windows of the tone", fits.len());
    for (amplitude, db) in fits {
        assert!((0.3..0.75).contains(amplitude), "{what}: the tone at {amplitude}");
        assert!(*db > 40.0, "{what}: the tone only {db} dB over the rest");
    }
}

#[test]
fn a_speaker_of_each_common_format_receives_the_tone_whole() {
    let mut tone = ToneGenerator::new(TONE);
    for (format, rate, channels) in FORMATS {
        let label = format!("{SPEAKER}{}", caps(format, rate, channels));
        let mut speaker = devices(vec![device(DeviceKind::Speaker, &label, true)])
            .speaker()
            .unwrap();
        let start = Instant::now();
        for index in 0..60 {
            let mut frame = AudioFrame::silence();
            tone.fill(frame.samples_mut());
            speaker.play(&frame).unwrap();
            if let Some(wait) = (start + FRAME_DURATION * (index + 1)).checked_duration_since(Instant::now()) {
                thread::sleep(wait);
            }
        }
        thread::sleep(Duration::from_millis(200));
        drop(speaker);
        let bytes = lock(received()).remove(&label).unwrap_or_default();
        let fits = windows(&first_channel(&bytes, format, channels), rate);
        assert_whole(&fits, &label);
    }
}

#[test]
fn a_microphone_of_each_common_format_gives_whole_frames_of_its_tone() {
    for (format, rate, channels) in FORMATS {
        let label = format!("{MICROPHONE}{}", caps(format, rate, channels));
        let mut microphone = devices(vec![device(DeviceKind::Microphone, &label, true)])
            .microphone()
            .unwrap();
        let mut samples = Vec::new();
        for _ in 0..40 {
            let frame = microphone.next_frame(Duration::from_secs(2)).unwrap().unwrap();
            samples.extend_from_slice(frame.samples());
        }
        assert_whole(&windows(&samples, SAMPLE_RATE.try_into().unwrap()), &label);
    }
}
