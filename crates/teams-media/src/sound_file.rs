//! A sound file made elsewhere, and so untrusted, read as mono samples: WAV by this crate's own reader
//! ([`wav::decode_any`]), MP3 and Ogg by `GStreamer`, with the same elements kept out as the video player keeps out
//! (nothing that fetches: [`start_gstreamer`]). Its plugins are the ones the app's packages already install.
//!
//! The kind is told from the bytes, never from a name, and only a file that starts like an MP3 or an Ogg ever reaches
//! `GStreamer`. Reading stops once a file has run past the length its caller takes, so a small file that unpacks to
//! hours costs no more than one that long, and past [`DEADLINE`] whatever it is doing, so a file made to stall holds
//! nothing up.

use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use crate::devices::wav::{self, AnyWav};
use crate::devices::{DeviceError, SAMPLE_RATE};
use crate::player::start_gstreamer;

/// The longest one file is worked on.
const DEADLINE: Duration = Duration::from_secs(20);
/// How long the appsink is waited on before the bus and the clock are looked at again.
const POLL: Duration = Duration::from_millis(50);

fn refused(why: &str) -> DeviceError {
    DeviceError::SoundFile(why.to_owned())
}

/// Whether `bytes` start like an MP3: an ID3 tag, or an MPEG audio frame's sync.
fn looks_like_mp3(bytes: &[u8]) -> bool {
    bytes.starts_with(b"ID3") || matches!(bytes, [0xFF, second, ..] if second & 0xE0 == 0xE0)
}

/// Reads a WAV, MP3 or Ogg file's bytes as mono samples (see the module docs): a WAV at its own rate, the others at
/// the device's. Past `max_seconds` by a little it stops: a file too long reads as longer than `max_seconds`, not as
/// its whole self.
///
/// # Errors
/// It is none of those, or too damaged to read, or no installed plugin decodes it.
pub fn decode(bytes: &[u8], max_seconds: f64) -> Result<AnyWav, DeviceError> {
    if bytes.starts_with(b"RIFF") {
        return wav::decode_any(bytes);
    }
    if !bytes.starts_with(b"OggS") && !looks_like_mp3(bytes) {
        return Err(refused("not an MP3 or Ogg file"));
    }
    compressed(bytes, max_seconds)
}

/// A pipeline set to nothing when dropped, on every way out.
struct Running(gst::Pipeline);

impl Drop for Running {
    fn drop(&mut self) {
        // a pipeline that won't stop has nothing more to say
        let _ = self.0.set_state(gst::State::Null);
    }
}

fn element(factory: &str) -> Result<gst::Element, DeviceError> {
    gst::ElementFactory::make(factory)
        .build()
        .map_err(|_| refused("a GStreamer element is missing"))
}

/// `bytes` through `decodebin` to an appsink of mono floats at the device's rate.
fn pipeline(bytes: &[u8]) -> Result<(Running, gst_app::AppSink), DeviceError> {
    start_gstreamer().map_err(|_| refused("GStreamer can't start"))?;
    let pipeline = gst::Pipeline::new();
    let source = gst_app::AppSrc::builder().format(gst::Format::Bytes).build();
    let decode = element("decodebin")?;
    let convert = element("audioconvert")?;
    let resample = element("audioresample")?;
    let caps = gst::Caps::builder("audio/x-raw")
        .field("format", "F32LE")
        .field("layout", "interleaved")
        .field("channels", 1_i32)
        .field("rate", i32::try_from(SAMPLE_RATE).unwrap_or(48_000))
        .build();
    let sink = gst_app::AppSink::builder()
        .caps(&caps)
        .sync(false)
        .max_buffers(32)
        .build();
    let linked = (|| {
        pipeline.add_many([source.upcast_ref(), &decode, &convert, &resample, sink.upcast_ref()])?;
        source.link(&decode)?;
        gst::Element::link_many([&convert, &resample, sink.upcast_ref()])
    })();
    linked.map_err(|_| refused("the decoder can't be set up"))?;
    let target = convert
        .static_pad("sink")
        .ok_or_else(|| refused("the decoder can't be set up"))?;
    decode.connect_pad_added(move |_, pad| {
        let audio = pad
            .current_caps()
            .or_else(|| Some(pad.query_caps(None)))
            .and_then(|caps| caps.structure(0).map(|s| s.name().starts_with("audio/")))
            .unwrap_or(false);
        if audio && !target.is_linked() {
            // an unlinked pad ends as an error on the bus, which is told
            let _ = pad.link(&target);
        }
    });
    let buffer = gst::Buffer::from_slice(bytes.to_vec());
    source
        .push_buffer(buffer)
        .map_err(|_| refused("the decoder can't be set up"))?;
    source
        .end_of_stream()
        .map_err(|_| refused("the decoder can't be set up"))?;
    let running = Running(pipeline);
    running
        .0
        .set_state(gst::State::Playing)
        .map_err(|_| refused("not a file GStreamer reads"))?;
    Ok((running, sink))
}

fn compressed(bytes: &[u8], max_seconds: f64) -> Result<AnyWav, DeviceError> {
    let (running, sink) = pipeline(bytes)?;
    let bus = running.0.bus().ok_or_else(|| refused("the decoder can't be set up"))?;
    let enough = (max_seconds + 0.5) * f64::from(SAMPLE_RATE);
    let started = Instant::now();
    let mut samples: Vec<f32> = Vec::new();
    while to_f64(samples.len()) <= enough {
        if started.elapsed() > DEADLINE {
            return Err(refused("it took too long to read"));
        }
        if bus.pop_filtered(&[gst::MessageType::Error]).is_some() {
            return Err(refused("a damaged file, or none GStreamer has a decoder for"));
        }
        let timeout = gst::ClockTime::from_mseconds(u64::try_from(POLL.as_millis()).unwrap_or(50));
        let Some(sample) = sink.try_pull_sample(timeout) else {
            if sink.is_eos() {
                break;
            }
            continue;
        };
        if let Some(map) = sample.buffer().and_then(|buffer| buffer.map_readable().ok()) {
            extend_floats(&mut samples, map.as_slice());
        }
    }
    if samples.is_empty() {
        return Err(refused("no audio in it"));
    }
    Ok(AnyWav {
        samples,
        rate: SAMPLE_RATE,
    })
}

/// Appends the little-endian floats of `bytes`, each finite and within `[-1, 1]`.
fn extend_floats(samples: &mut Vec<f32>, bytes: &[u8]) {
    samples.extend(bytes.as_chunks::<4>().0.iter().map(|sample| {
        let value = f32::from_le_bytes(*sample);
        if value.is_finite() { value.clamp(-1.0, 1.0) } else { 0.0 }
    }));
}

#[expect(clippy::cast_precision_loss, reason = "a sound's sample count stays far below 2^52")]
fn to_f64(len: usize) -> f64 {
    len as f64
}

#[cfg(test)]
#[path = "sound_file_samples.rs"]
mod samples;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tone::amplitude_at;

    fn bytes_of(hex: &str) -> Vec<u8> {
        hex.as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|[high, low]| {
                let digit = |byte: u8| char::from(byte).to_digit(16).unwrap_or(0);
                u8::try_from(digit(*high) * 16 + digit(*low)).unwrap_or(0)
            })
            .collect()
    }

    /// The 440 Hz tone is there at about the level encoded, somewhere in the file's middle.
    fn is_the_tone(sound: &AnyWav) {
        assert_eq!(sound.rate, SAMPLE_RATE);
        assert!((sound.seconds() - 0.25).abs() < 0.1, "{}", sound.seconds());
        let device = sound.at_device_rate();
        let middle = &device[device.len() / 4..device.len() * 3 / 4];
        assert!(amplitude_at(middle, 440.0) > 0.03, "{}", amplitude_at(middle, 440.0));
    }

    #[test]
    fn an_ogg_vorbis_file_reads() {
        is_the_tone(&decode(&bytes_of(samples::OGG), 30.0).expect("decoded"));
    }

    #[test]
    fn an_mp3_file_reads() {
        is_the_tone(&decode(&bytes_of(samples::MP3), 30.0).expect("decoded"));
    }

    #[test]
    fn reading_stops_past_the_length_taken() {
        let sound = decode(&bytes_of(samples::OGG), 0.0).expect("decoded");
        assert!(sound.seconds() <= 0.75, "{}", sound.seconds());
        assert!(!sound.samples.is_empty());
    }

    #[test]
    fn a_wav_goes_to_the_wav_reader() {
        assert!(matches!(decode(b"RIFFxxxxWAVE", 1.0), Err(DeviceError::Wav(_))));
    }

    #[test]
    fn what_isnt_a_sound_is_refused() {
        assert!(decode(b"not a sound at all", 30.0).is_err());
        assert!(decode(&[], 30.0).is_err());
        let mut torn = bytes_of(samples::OGG);
        torn.truncate(64);
        assert!(decode(&torn, 30.0).is_err());
        let mut garbled = bytes_of(samples::MP3);
        for byte in garbled.iter_mut().skip(40).step_by(3) {
            *byte ^= 0x5A;
        }
        // whatever it reads as, it doesn't take the app down
        let _ = decode(&garbled, 30.0);
    }

    #[test]
    fn floats_are_kept_finite_and_in_range() {
        let mut samples = Vec::new();
        let bytes: Vec<u8> = [0.5_f32, f32::NAN, f32::INFINITY, 2.0, -2.0]
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect();
        extend_floats(&mut samples, &bytes);
        assert_eq!(samples, vec![0.5, 0.0, 0.0, 1.0, -1.0]);
    }

    #[test]
    fn only_what_starts_like_an_mp3_or_ogg_reaches_gstreamer() {
        assert!(looks_like_mp3(b"ID3\x04"));
        assert!(looks_like_mp3(&[0xFF, 0xFB, 0x90]));
        assert!(!looks_like_mp3(&[0xFF, 0x10]));
        assert!(!looks_like_mp3(b""));
        assert!(matches!(
            decode(b"#EXTM3U\nhttp://x", 1.0),
            Err(DeviceError::SoundFile(_))
        ));
    }
}
