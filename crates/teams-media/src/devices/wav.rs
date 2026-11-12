//! WAV files at the device seam's rate: read as mono samples (16-bit PCM or 32-bit float, any channel count, mixed
//! down), written as 16-bit PCM mono. [`decode_any`] takes a file made elsewhere, untrusted: more sample formats, any
//! usual rate, every sample a finite number within `[-1, 1]`.
//!
//! A recording's header is brought up to date every second and when the writer is finished or dropped, so a recording
//! read while it is still being written (or after its writer was killed) holds everything up to the last update. The
//! reader takes such a file: a `data` size of 0, `0xFFFFFFFF` or past the end is read to the end.

use std::fs::File;
use std::io::{BufWriter, Seek as _, SeekFrom, Write as _};
use std::path::Path;

use super::{DeviceError, SAMPLE_RATE};

const FORMAT_PCM: u16 = 1;
const FORMAT_FLOAT: u16 = 3;
const FORMAT_EXTENSIBLE: u16 = 0xFFFE;
/// The header this writer writes: `RIFF`, `fmt ` (16 bytes) and the `data` chunk's header.
const HEADER_LEN: u64 = 44;
/// A recording's header is brought up to date after this many samples (1 s).
const UPDATE_EVERY: u64 = SAMPLE_RATE as u64;

/// The rates [`decode_any`] takes, in Hz.
pub const ANY_RATES: std::ops::RangeInclusive<u32> = 8_000..=192_000;
/// The most channels [`decode_any`] takes.
pub const MAX_CHANNELS: u16 = 8;

/// How a file's samples are stored.
#[derive(Clone, Copy, Debug)]
struct Format {
    float: bool,
    bits: u16,
    channels: u16,
}

/// A `fmt ` chunk as the file says it, nothing refused yet.
#[derive(Clone, Copy, Debug)]
struct Declared {
    tag: u16,
    channels: u16,
    rate: u32,
    bits: u16,
}

/// A WAV file made elsewhere, mixed down to mono at its own rate ([`decode_any`]).
#[derive(Clone, Debug, PartialEq)]
pub struct AnyWav {
    pub samples: Vec<f32>,
    /// In Hz, within [`ANY_RATES`].
    pub rate: u32,
}

impl AnyWav {
    /// Its samples at [`SAMPLE_RATE`] (linear interpolation: a short alert's sound, not a recording's).
    #[must_use]
    pub fn at_device_rate(&self) -> Vec<f32> {
        if self.rate == SAMPLE_RATE || self.samples.is_empty() {
            return self.samples.clone();
        }
        let step = f64::from(self.rate) / f64::from(SAMPLE_RATE);
        let count = (len_f64(self.samples.len()) / step).floor();
        (0..to_index(count))
            .map(|index| {
                let at = len_f64(index) * step;
                let first = to_index(at.floor());
                let next = self.samples.get(first + 1).copied().unwrap_or(self.samples[first]);
                let part = to_f32(at - at.floor());
                self.samples[first] + (next - self.samples[first]) * part
            })
            .collect()
    }

    /// How long it lasts, in seconds.
    #[must_use]
    pub fn seconds(&self) -> f64 {
        len_f64(self.samples.len()) / f64::from(self.rate)
    }
}

/// Reads `path` as mono samples in `[-1, 1]` at [`SAMPLE_RATE`].
///
/// # Errors
/// The file can't be read, isn't a WAV file, isn't at 48 kHz, or stores its samples other than as 16-bit PCM or
/// 32-bit float.
pub fn read(path: &Path) -> Result<Vec<f32>, DeviceError> {
    let bytes = std::fs::read(path)?;
    decode(&bytes)
}

/// Decodes a WAV file's bytes, as [`read`] does.
///
/// # Errors
/// As [`read`].
pub fn decode(bytes: &[u8]) -> Result<Vec<f32>, DeviceError> {
    let (declared, body) = audio_of(bytes)?;
    Ok(samples(body, strict_format(declared)?))
}

/// Decodes a WAV file made elsewhere (see the module docs): 8, 16, 24 or 32-bit PCM or 32-bit float, up to
/// [`MAX_CHANNELS`] channels mixed down, at a rate within [`ANY_RATES`]. A sample that isn't a finite number reads as
/// silence, and every one is kept within `[-1, 1]`.
///
/// # Errors
/// It isn't a WAV file, or stores its samples in any other way.
pub fn decode_any(bytes: &[u8]) -> Result<AnyWav, DeviceError> {
    let (declared, body) = audio_of(bytes)?;
    let float = match (declared.tag, declared.bits) {
        (FORMAT_PCM, 8 | 16 | 24 | 32) => false,
        (FORMAT_FLOAT, 32) => true,
        _ => {
            return Err(DeviceError::Wav(
                "a sample format that isn't PCM or 32-bit float".to_owned(),
            ));
        }
    };
    if !ANY_RATES.contains(&declared.rate) {
        return Err(DeviceError::Wav("a sample rate out of range".to_owned()));
    }
    if declared.channels == 0 || declared.channels > MAX_CHANNELS {
        return Err(DeviceError::Wav("a channel count out of range".to_owned()));
    }
    let format = Format {
        float,
        bits: declared.bits,
        channels: declared.channels,
    };
    Ok(AnyWav {
        samples: samples(body, format),
        rate: declared.rate,
    })
}

/// The file's format as it says it, and the bytes of its `data` chunk.
fn audio_of(bytes: &[u8]) -> Result<(Declared, &[u8]), DeviceError> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(DeviceError::Wav("not a RIFF WAVE file".to_owned()));
    }
    let mut format = None;
    let mut at = 12;
    while let Some(header) = bytes.get(at..at + 8) {
        let id = &header[..4];
        let raw = u32_at(header, 4);
        let declared = usize::try_from(raw).unwrap_or(usize::MAX);
        let body_start = at + 8;
        if id == b"data" {
            let format = format.ok_or_else(|| DeviceError::Wav("a data chunk before its fmt chunk".to_owned()))?;
            let rest = &bytes[body_start..];
            let body = if raw == 0 || raw == u32::MAX || declared > rest.len() {
                rest
            } else {
                &rest[..declared]
            };
            return Ok((format, body));
        }
        let body = bytes
            .get(body_start..body_start.saturating_add(declared))
            .ok_or_else(|| DeviceError::Wav("a chunk runs past the end of the file".to_owned()))?;
        if id == b"fmt " {
            format = Some(parse_format(body)?);
        }
        // chunks are padded to an even length
        at = body_start + declared + declared % 2;
    }
    Err(DeviceError::Wav("no data chunk".to_owned()))
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn parse_format(body: &[u8]) -> Result<Declared, DeviceError> {
    if body.len() < 16 {
        return Err(DeviceError::Wav("a fmt chunk under 16 bytes".to_owned()));
    }
    let mut tag = u16_at(body, 0);
    if tag == FORMAT_EXTENSIBLE {
        // the sub-format GUID's first two bytes are the plain format tag
        tag = body
            .get(24..26)
            .map(|sub| u16_at(sub, 0))
            .ok_or_else(|| DeviceError::Wav("an extensible fmt chunk without its sub-format".to_owned()))?;
    }
    Ok(Declared {
        tag,
        channels: u16_at(body, 2),
        rate: u32_at(body, 4),
        bits: u16_at(body, 14),
    })
}

/// What [`decode`] takes of a declared format.
fn strict_format(declared: Declared) -> Result<Format, DeviceError> {
    let Declared {
        tag,
        channels,
        rate,
        bits,
    } = declared;
    if rate != SAMPLE_RATE {
        return Err(DeviceError::Wav(format!("{rate} Hz; only {SAMPLE_RATE} Hz is taken")));
    }
    if channels == 0 {
        return Err(DeviceError::Wav("no channels".to_owned()));
    }
    match (tag, bits) {
        (FORMAT_PCM, 16) => Ok(Format {
            float: false,
            bits,
            channels,
        }),
        (FORMAT_FLOAT, 32) => Ok(Format {
            float: true,
            bits,
            channels,
        }),
        _ => Err(DeviceError::Wav(format!(
            "format {tag} at {bits} bits; only 16-bit PCM and 32-bit float are taken"
        ))),
    }
}

/// The samples of `body`, each frame's channels averaged; a trailing partial frame is dropped.
fn samples(body: &[u8], format: Format) -> Vec<f32> {
    let width = usize::from(format.bits / 8);
    let channels = usize::from(format.channels);
    let scale = 1.0 / f32::from(format.channels);
    body.chunks_exact(width * channels)
        .map(|frame| {
            frame
                .chunks_exact(width)
                .map(|sample| decode_sample(sample, format.float))
                .sum::<f32>()
                * scale
        })
        .collect()
}

fn decode_sample(bytes: &[u8], float: bool) -> f32 {
    let sample = match (float, bytes) {
        (true, &[a, b, c, d]) => f32::from_le_bytes([a, b, c, d]),
        (false, &[a]) => (f32::from(a) - 128.0) / 128.0,
        (false, &[a, b]) => f32::from(i16::from_le_bytes([a, b])) / 32_768.0,
        // the high bytes, sign included: 24 bits are as many as an f32 holds
        (false, &[_, b, c]) => f32::from(i16::from_le_bytes([b, c])) / 32_768.0,
        (false, &[_, _, c, d]) => f32::from(i16::from_le_bytes([c, d])) / 32_768.0,
        _ => 0.0,
    };
    if sample.is_finite() {
        sample.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

#[expect(clippy::cast_precision_loss, reason = "a sound's sample count stays far below 2^52")]
fn len_f64(len: usize) -> f64 {
    len as f64
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a non-negative whole number of samples within the sound"
)]
fn to_index(value: f64) -> usize {
    value as usize
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "a part of one sample's distance, within 0 to 1"
)]
fn to_f32(value: f64) -> f32 {
    value as f32
}

/// A 16-bit PCM mono WAV recording at [`SAMPLE_RATE`], private to the user (0600 on Unix, a protected DACL on Windows).
pub struct WavWriter {
    file: BufWriter<File>,
    samples: u64,
    since_update: u64,
}

impl WavWriter {
    /// Creates (or truncates) `path` and writes the header of an empty recording.
    ///
    /// # Errors
    /// The file can't be created or written.
    pub fn create(path: &Path) -> Result<Self, DeviceError> {
        let file = teams_platform::private::replace(path)?;
        let mut writer = Self {
            file: BufWriter::new(file),
            samples: 0,
            since_update: 0,
        };
        writer.file.write_all(&header(0))?;
        writer.file.flush()?;
        Ok(writer)
    }

    /// Appends `samples` (clipped to `[-1, 1]`).
    ///
    /// # Errors
    /// The file can't be written.
    pub fn write(&mut self, samples: &[f32]) -> Result<(), DeviceError> {
        for &sample in samples {
            self.file.write_all(&to_pcm(sample).to_le_bytes())?;
        }
        let count = u64::try_from(samples.len()).unwrap_or(u64::MAX);
        self.samples = self.samples.saturating_add(count);
        self.since_update = self.since_update.saturating_add(count);
        if self.since_update >= UPDATE_EVERY {
            self.update_header()?;
        }
        Ok(())
    }

    /// Samples written so far.
    #[must_use]
    pub fn samples(&self) -> u64 {
        self.samples
    }

    /// Writes the header for everything written so far and flushes.
    ///
    /// # Errors
    /// The file can't be written.
    pub fn finish(mut self) -> Result<(), DeviceError> {
        self.update_header()
    }

    fn update_header(&mut self) -> Result<(), DeviceError> {
        self.file.flush()?;
        let data = u32::try_from(self.samples.saturating_mul(2)).unwrap_or(u32::MAX);
        let file = self.file.get_mut();
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&header(data))?;
        file.seek(SeekFrom::Start(HEADER_LEN + u64::from(data)))?;
        self.since_update = 0;
        Ok(())
    }
}

impl Drop for WavWriter {
    fn drop(&mut self) {
        let _ = self.update_header();
    }
}

/// The 44-byte header of a 16-bit PCM mono recording holding `data` bytes.
fn header(data: u32) -> Vec<u8> {
    let mut header = Vec::with_capacity(44);
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&data.saturating_add(36).to_le_bytes());
    header.extend_from_slice(b"WAVEfmt ");
    header.extend_from_slice(&16_u32.to_le_bytes());
    header.extend_from_slice(&FORMAT_PCM.to_le_bytes());
    header.extend_from_slice(&1_u16.to_le_bytes());
    header.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    header.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    header.extend_from_slice(&2_u16.to_le_bytes());
    header.extend_from_slice(&16_u16.to_le_bytes());
    header.extend_from_slice(b"data");
    header.extend_from_slice(&data.to_le_bytes());
    header
}

/// Rounds and clips a sample to 16-bit PCM.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the value is clamped to the i16 range first"
)]
fn to_pcm(sample: f32) -> i16 {
    (sample * 32_767.0)
        .round()
        .clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A WAV file of `body`, its `fmt ` chunk as declared.
    fn file(tag: u16, channels: u16, rate: u32, bits: u16, body: &[u8]) -> Vec<u8> {
        let mut bytes = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&tag.to_le_bytes());
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&rate.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u16.to_le_bytes());
        bytes.extend_from_slice(&bits.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&u32::try_from(body.len()).unwrap_or(0).to_le_bytes());
        bytes.extend_from_slice(body);
        bytes
    }

    #[test]
    fn any_takes_every_pcm_width_and_float() {
        let eight = decode_any(&file(FORMAT_PCM, 1, 8_000, 8, &[128, 255, 0])).map(|wav| wav.samples);
        assert_eq!(eight.ok(), Some(vec![0.0, 127.0 / 128.0, -1.0]));
        let sixteen = decode_any(&file(FORMAT_PCM, 1, 44_100, 16, &i16::MIN.to_le_bytes()));
        assert_eq!(
            sixteen.ok().map(|wav| (wav.samples, wav.rate)),
            Some((vec![-1.0], 44_100))
        );
        let twenty_four = decode_any(&file(FORMAT_PCM, 1, 48_000, 24, &[0xFF, 0x00, 0x40]));
        assert_eq!(twenty_four.ok().map(|wav| wav.samples), Some(vec![0.5]));
        let thirty_two = decode_any(&file(FORMAT_PCM, 1, 48_000, 32, &[0xFF, 0xFF, 0x00, 0xC0]));
        assert_eq!(thirty_two.ok().map(|wav| wav.samples), Some(vec![-0.5]));
    }

    #[test]
    fn any_keeps_samples_finite_and_in_range() {
        let mut body = Vec::new();
        for value in [f32::NAN, f32::INFINITY, 3.0, -3.0, 0.25] {
            body.extend_from_slice(&value.to_le_bytes());
        }
        let wav = decode_any(&file(FORMAT_FLOAT, 1, 48_000, 32, &body));
        assert_eq!(wav.ok().map(|wav| wav.samples), Some(vec![0.0, 0.0, 1.0, -1.0, 0.25]));
    }

    #[test]
    fn any_mixes_channels_down() {
        let body = [i16::MAX.to_le_bytes(), 0_i16.to_le_bytes()].concat();
        let wav = decode_any(&file(FORMAT_PCM, 2, 48_000, 16, &body))
            .ok()
            .map(|wav| wav.samples);
        assert!(wav.is_some_and(|samples| samples.len() == 1 && (samples[0] - 0.5).abs() < 0.001));
    }

    #[test]
    fn any_refuses_what_it_cant_read() {
        for bad in [
            file(2, 1, 48_000, 16, &[0, 0]),
            file(FORMAT_FLOAT, 1, 48_000, 64, &[0; 8]),
            file(FORMAT_PCM, 1, 4_000, 16, &[0, 0]),
            file(FORMAT_PCM, 1, 400_000, 16, &[0, 0]),
            file(FORMAT_PCM, 0, 48_000, 16, &[0, 0]),
            file(FORMAT_PCM, 9, 48_000, 16, &[0, 0]),
            b"not a wave at all".to_vec(),
        ] {
            assert!(decode_any(&bad).is_err());
        }
    }

    #[test]
    fn a_file_at_another_rate_is_brought_to_the_device_rate() {
        let wav = AnyWav {
            samples: vec![0.0, 1.0, 0.0, -1.0],
            rate: 24_000,
        };
        assert_eq!(wav.at_device_rate(), vec![0.0, 0.5, 1.0, 0.5, 0.0, -0.5, -1.0, -1.0]);
        assert!((wav.seconds() - 4.0 / 24_000.0).abs() < 1e-9);
        let same = AnyWav {
            samples: vec![0.5],
            rate: SAMPLE_RATE,
        };
        assert_eq!(same.at_device_rate(), vec![0.5]);
    }

    #[test]
    fn the_strict_reader_still_takes_only_its_formats() {
        assert!(decode(&file(FORMAT_PCM, 1, 44_100, 16, &[0, 0])).is_err());
        assert!(decode(&file(FORMAT_PCM, 1, 48_000, 24, &[0, 0, 0])).is_err());
        assert!(decode(&file(FORMAT_PCM, 0, 48_000, 16, &[0, 0])).is_err());
        assert_eq!(decode(&file(FORMAT_PCM, 1, 48_000, 16, &[0, 0])).ok(), Some(vec![0.0]));
    }
}
