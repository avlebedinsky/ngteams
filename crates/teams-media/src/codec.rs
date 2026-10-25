//! Opus as the calls configure it (libopus through the `opus` crate): 48 kHz mono, 20 ms packets (`a=ptime:20`), the
//! `a=fmtp` parameters both sides write (`minptime=10;useinbandfec=1`, the media controller's answer adding
//! `usedtx=1`; `docs/protocol/calling-echo.md`).
//!
//! The [`Encoder`] takes the device seam's 10 ms frames and makes one packet of every two. With in-band FEC on, each
//! packet also carries a low-bitrate copy of the one before it, so a receiver that lost a packet but holds the next
//! decodes the lost one from it ([`Decoder::recover`]); with nothing to recover from, Opus conceals the gap
//! ([`Decoder::conceal`]). With DTX on, the encoder sends nothing during silence but an occasional keep-alive.

use crate::Error;
use crate::devices::{AudioFrame, FRAME_SAMPLES, SAMPLE_RATE};

/// Samples in one packet: 20 ms at 48 kHz, mono.
pub const PACKET_SAMPLES: usize = 2 * FRAME_SAMPLES;
/// RTP ticks (48 kHz) one packet advances the media time by.
pub const PACKET_TICKS: u64 = 960;
/// Longest packet Opus allows, in samples (120 ms).
pub const MAX_PACKET_SAMPLES: usize = 6 * PACKET_SAMPLES;
/// Largest packet the encoder may write; a 20 ms mono packet stays far below it.
const MAX_PACKET_BYTES: usize = 1_500;
/// A packet this short or shorter is DTX's "nothing to send" (libopus: 2 bytes or less needn't be transmitted).
const DTX_PACKET_BYTES: usize = 2;
/// The packet loss the encoder plans its FEC for until told otherwise, in percent.
pub const DEFAULT_EXPECTED_LOSS: u8 = 10;

/// How the encoder runs, from the SDP's Opus parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpusConfig {
    /// In-band FEC (`useinbandfec=1`).
    pub inband_fec: bool,
    /// Discontinuous transmission (`usedtx=1`).
    pub dtx: bool,
    /// The packet loss to plan FEC for, in percent (0 to 100).
    pub expected_loss: u8,
}

impl Default for OpusConfig {
    /// What our own offer asks for (`minptime=10;useinbandfec=1`): FEC on, DTX off.
    fn default() -> Self {
        Self {
            inband_fec: true,
            dtx: false,
            expected_loss: DEFAULT_EXPECTED_LOSS,
        }
    }
}

impl OpusConfig {
    /// The configuration an `a=fmtp` line's Opus parameters ask for (`minptime=10;usedtx=1;useinbandfec=1`): each
    /// flag on only when set to `1`, off when absent (RFC 7587's defaults); other parameters don't change the encoder.
    #[must_use]
    pub fn from_fmtp(params: &str) -> Self {
        let flag = |name: &str| {
            params
                .split(';')
                .filter_map(|param| param.split_once('='))
                .any(|(key, value)| key.trim().eq_ignore_ascii_case(name) && value.trim() == "1")
        };
        Self {
            inband_fec: flag("useinbandfec"),
            dtx: flag("usedtx"),
            expected_loss: DEFAULT_EXPECTED_LOSS,
        }
    }

    /// What the Opus payload of the audio m-line `mid` in `sdp` (standard SDP text) asks for: [`OpusConfig::from_fmtp`]
    /// of its `a=fmtp` (all off when it has none). `None` when `sdp` has no audio line `mid`, or one without Opus.
    #[must_use]
    pub fn from_sdp(sdp: &str, mid: &str) -> Option<Self> {
        let mut sections: Vec<Vec<&str>> = Vec::new();
        for line in sdp.lines().map(str::trim_end) {
            if line.starts_with("m=") {
                sections.push(vec![line]);
            } else if let Some(section) = sections.last_mut() {
                section.push(line);
            }
        }
        let audio = sections.iter().find(|section| {
            section[0].starts_with("m=audio ") && section.iter().any(|line| line.strip_prefix("a=mid:") == Some(mid))
        })?;
        let pt = audio.iter().find_map(|line| {
            let (pt, encoding) = line.strip_prefix("a=rtpmap:")?.split_once(' ')?;
            let name = encoding.split('/').next()?;
            name.eq_ignore_ascii_case("opus").then_some(pt)
        })?;
        let params = audio
            .iter()
            .find_map(|line| {
                let (at, params) = line.strip_prefix("a=fmtp:")?.split_once(' ')?;
                (at == pt).then_some(params)
            })
            .unwrap_or("");
        Some(Self::from_fmtp(params))
    }
}

/// One encoded packet: its RTP media time (48 kHz ticks, counted from the first packet) and its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    pub rtp_time: u64,
    pub payload: Vec<u8>,
}

/// Encodes the device seam's 10 ms frames into 20 ms Opus packets.
pub struct Encoder {
    inner: opus::Encoder,
    dtx: bool,
    pending: Vec<f32>,
    rtp_time: u64,
}

impl Encoder {
    /// A voice encoder as `config` asks.
    ///
    /// # Errors
    /// [`Error::Opus`] if libopus refuses the setup.
    pub fn new(config: &OpusConfig) -> Result<Self, Error> {
        let inner = opus::Encoder::new(SAMPLE_RATE, opus::Channels::Mono, opus::Application::Voip)?;
        let mut encoder = Self {
            inner,
            dtx: config.dtx,
            pending: Vec::with_capacity(PACKET_SAMPLES),
            rtp_time: 0,
        };
        encoder.configure(config)?;
        Ok(encoder)
    }

    /// Runs as `config` asks from the next packet on (the SDP's answer arriving after the encoder started).
    ///
    /// # Errors
    /// [`Error::Opus`] if libopus refuses it.
    pub fn configure(&mut self, config: &OpusConfig) -> Result<(), Error> {
        self.inner.set_inband_fec(config.inband_fec)?;
        self.inner.set_dtx(config.dtx)?;
        self.inner
            .set_packet_loss_perc(i32::from(config.expected_loss.min(100)))?;
        self.dtx = config.dtx;
        Ok(())
    }

    /// Plans FEC for `percent` packet loss (capped at 100).
    ///
    /// # Errors
    /// [`Error::Opus`] if libopus refuses it.
    pub fn set_expected_loss(&mut self, percent: u8) -> Result<(), Error> {
        Ok(self.inner.set_packet_loss_perc(i32::from(percent.min(100)))?)
    }

    /// Takes one frame. Every second frame completes a packet, returned unless DTX says it needn't be sent; the media
    /// time advances by [`PACKET_TICKS`] per packet either way, so the receiver sees the silence as a gap.
    ///
    /// # Errors
    /// [`Error::Opus`] if libopus fails.
    pub fn push(&mut self, frame: &AudioFrame) -> Result<Option<Packet>, Error> {
        self.pending.extend_from_slice(frame.samples());
        if self.pending.len() < PACKET_SAMPLES {
            return Ok(None);
        }
        let payload = self.inner.encode_vec_float(&self.pending, MAX_PACKET_BYTES)?;
        self.pending.clear();
        let rtp_time = self.rtp_time;
        self.rtp_time += PACKET_TICKS;
        if self.dtx && payload.len() <= DTX_PACKET_BYTES {
            return Ok(None);
        }
        Ok(Some(Packet { rtp_time, payload }))
    }
}

/// The samples a packet holds at 48 kHz, or `None` when it isn't a readable Opus packet.
#[must_use]
pub fn packet_samples(packet: &[u8]) -> Option<usize> {
    opus::packet::get_nb_samples(packet, SAMPLE_RATE)
        .ok()
        .filter(|samples| (1..=MAX_PACKET_SAMPLES).contains(samples))
}

/// Decodes Opus packets into 48 kHz mono samples, recovers a lost packet from the next one's FEC, and conceals a gap.
pub struct Decoder {
    inner: opus::Decoder,
    scratch: Vec<f32>,
}

impl Decoder {
    /// A new decoder.
    ///
    /// # Errors
    /// [`Error::Opus`] if libopus refuses the setup.
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            inner: opus::Decoder::new(SAMPLE_RATE, opus::Channels::Mono)?,
            scratch: vec![0.0; MAX_PACKET_SAMPLES],
        })
    }

    /// Decodes `packet`, appending its samples to `out`; returns how many.
    ///
    /// # Errors
    /// [`Error::Opus`] if the packet is not valid Opus.
    pub fn decode(&mut self, packet: &[u8], out: &mut Vec<f32>) -> Result<usize, Error> {
        if packet.is_empty() {
            return Err(Error::Opus("an empty packet".into()));
        }
        let samples = self.inner.decode_float(packet, &mut self.scratch, false)?;
        out.extend_from_slice(&self.scratch[..samples]);
        Ok(samples)
    }

    /// Recovers a lost packet of `samples` samples from `next`, the packet after it (its in-band FEC; Opus conceals
    /// the gap instead when `next` carries none), appending them to `out`. Decode `next` itself afterwards.
    ///
    /// # Errors
    /// [`Error::Opus`] if `next` is not valid Opus or `samples` isn't a whole number of 2.5 ms up to 120 ms.
    pub fn recover(&mut self, next: &[u8], samples: usize, out: &mut Vec<f32>) -> Result<usize, Error> {
        if next.is_empty() {
            return Err(Error::Opus("an empty packet".into()));
        }
        self.decode_into(next, samples, true, out)
    }

    /// Conceals a lost packet of `samples` samples (Opus's own loss concealment), appending them to `out`.
    ///
    /// # Errors
    /// [`Error::Opus`] if `samples` isn't a whole number of 2.5 ms up to 120 ms.
    pub fn conceal(&mut self, samples: usize, out: &mut Vec<f32>) -> Result<usize, Error> {
        self.decode_into(&[], samples, false, out)
    }

    fn decode_into(&mut self, packet: &[u8], samples: usize, fec: bool, out: &mut Vec<f32>) -> Result<usize, Error> {
        let scratch = self
            .scratch
            .get_mut(..samples)
            .filter(|_| samples > 0 && samples.is_multiple_of(FRAME_SAMPLES / 4))
            .ok_or_else(|| Error::Opus(format!("{samples} samples is no Opus frame size")))?;
        let decoded = self.inner.decode_float(packet, scratch, fec)?;
        out.extend_from_slice(&scratch[..decoded]);
        Ok(decoded)
    }
}

impl From<opus::Error> for Error {
    fn from(error: opus::Error) -> Self {
        Self::Opus(error.to_string())
    }
}
