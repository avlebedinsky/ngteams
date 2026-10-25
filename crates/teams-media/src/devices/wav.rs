//! WAV files at the device seam's rate: read as mono samples (16-bit PCM or 32-bit float, any channel count, mixed
//! down), written as 16-bit PCM mono.
//!
//! A recording's header is brought up to date every second and when the writer is finished or dropped, so a recording
//! read while it is still being written (or after its writer was killed) holds everything up to the last update. The
//! reader takes such a file: a `data` size of 0, `0xFFFFFFFF` or past the end is read to the end.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

use super::{DeviceError, SAMPLE_RATE};

const FORMAT_PCM: u16 = 1;
const FORMAT_FLOAT: u16 = 3;
const FORMAT_EXTENSIBLE: u16 = 0xFFFE;
/// The header this writer writes: `RIFF`, `fmt ` (16 bytes) and the `data` chunk's header.
const HEADER_LEN: u64 = 44;
/// A recording's header is brought up to date after this many samples (1 s).
const UPDATE_EVERY: u64 = SAMPLE_RATE as u64;

/// How a file's samples are stored.
#[derive(Clone, Copy, Debug)]
struct Format {
    float: bool,
    bits: u16,
    channels: u16,
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
            return Ok(samples(body, format));
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

fn parse_format(body: &[u8]) -> Result<Format, DeviceError> {
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
    let channels = u16_at(body, 2);
    let rate = u32_at(body, 4);
    let bits = u16_at(body, 14);
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
    if float {
        f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
    } else {
        f32::from(i16::from_le_bytes([bytes[0], bytes[1]])) / 32_768.0
    }
}

/// A 16-bit PCM mono WAV recording at [`SAMPLE_RATE`], made `0600`.
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
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
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
