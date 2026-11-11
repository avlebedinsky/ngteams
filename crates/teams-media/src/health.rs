//! A call's health, as Teams' Call health pane shows it: the network's round trip, loss and jitter, and what the audio,
//! the camera's video and a screen shared send and receive, read from libwebrtc's own statistics (`getStats`) of the
//! call's peer. Reading them sends nothing.
//!
//! libwebrtc counts from the call's start (bytes, packets, packets lost): a rate (a bitrate, a loss rate) is the
//! difference between two reads over the time between them ([`HealthReader`]), so the first read has none. The camera's
//! lines and a screen's are told apart by mid: our screen's line, and the sharing lines the dialect names (the far
//! end's share, a group offer's sharing line), are the screen sharing's; every other video line is the camera's (a
//! group call's receiving lines added together, the largest picture's frame rate, resolution and codec shown).

use std::collections::HashMap;
use std::time::Instant;

use libwebrtc::stats::{CandidatePairStats, InboundRtpStats, OutboundRtpStats, RemoteInboundRtpStats, RtcStats};

/// What the pane shows of the call.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CallHealth {
    pub network: NetworkHealth,
    pub audio: MediaHealth,
    pub video: MediaHealth,
    pub sharing: MediaHealth,
}

/// The connection's own measures.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NetworkHealth {
    /// The relayed connection's round trip now, ms.
    pub round_trip_ms: Option<f64>,
    /// The share of the packets of everything received lost since the last read, percent.
    pub received_loss_percent: Option<f64>,
    /// The received audio's jitter, ms (the largest of the streams').
    pub received_jitter_ms: Option<f64>,
    /// The share of what was sent the far end reports lost, percent (the largest of the streams').
    pub sent_loss_percent: Option<f64>,
    /// What libwebrtc's bandwidth estimate allows to send, kbit/s.
    pub available_send_kbps: Option<f64>,
    /// What it estimates can be received, kbit/s.
    pub available_receive_kbps: Option<f64>,
}

/// One kind of media (audio, the camera's video, screen sharing) each way; `None` a way that carries nothing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaHealth {
    pub sent: Option<StreamHealth>,
    pub received: Option<StreamHealth>,
}

impl MediaHealth {
    /// Whether it carries anything either way.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sent.is_none() && self.received.is_none()
    }
}

/// What a way of a kind of media carries.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StreamHealth {
    /// kbit/s since the last read (none on the first).
    pub bitrate_kbps: Option<f64>,
    /// Packets since the call's start.
    pub packets: u64,
    /// The share lost since the last read (received), or as the far end last reported it (sent), percent.
    pub loss_percent: Option<f64>,
    /// The jitter, ms: what we measure (received), or what the far end reports (sent).
    pub jitter_ms: Option<f64>,
    /// The round trip the far end's reports measure (sent), ms.
    pub round_trip_ms: Option<f64>,
    /// Frames a second (video).
    pub frame_rate: Option<f64>,
    /// Width and height, px (video).
    pub resolution: Option<(u32, u32)>,
    /// The codec's name as its MIME subtype has it (`opus`, `H264`).
    pub codec: Option<String>,
    /// Whether the video's encoder (sent) or decoder (received) runs in hardware; none when libwebrtc names none.
    pub hardware: Option<bool>,
}

/// The kind of media a stream carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Audio,
    Video,
    Sharing,
}

/// Which video lines are screen sharing's: our screen's (sending) and the sharing lines the dialect names.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lines {
    pub sharing: Vec<String>,
    pub screen: Option<String>,
}

impl Lines {
    fn kind(&self, kind: &str, mid: &str) -> Option<Kind> {
        match kind {
            "audio" => Some(Kind::Audio),
            "video" if self.screen.as_deref() == Some(mid) || self.sharing.iter().any(|line| line == mid) => {
                Some(Kind::Sharing)
            }
            "video" => Some(Kind::Video),
            _ => None,
        }
    }
}

/// A stream's counts at a read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Counts {
    bytes: u64,
    packets: u64,
    lost: i64,
}

/// Reads a call's health, keeping each read's counts for the next one's rates.
#[derive(Debug, Default)]
pub struct HealthReader {
    previous: Option<(Instant, HashMap<String, Counts>)>,
}

impl HealthReader {
    /// The health `stats` (one `getStats` of the call's peer, at `now`) tell, its rates against the last read.
    pub fn read(&mut self, stats: &[RtcStats], lines: &Lines, now: Instant) -> CallHealth {
        let index = Index::of(stats);
        let elapsed = self
            .previous
            .as_ref()
            .map(|(at, _)| now.saturating_duration_since(*at).as_secs_f64())
            .filter(|seconds| *seconds > 0.0);
        let empty = HashMap::new();
        let before = self.previous.as_ref().map_or(&empty, |(_, counts)| counts);
        let rates = Rates { before, elapsed };
        let mut health = CallHealth {
            network: network(&index, &rates),
            ..CallHealth::default()
        };
        for (kind, section) in [
            (Kind::Audio, &mut health.audio),
            (Kind::Video, &mut health.video),
            (Kind::Sharing, &mut health.sharing),
        ] {
            section.received = received(&index, lines, kind, &rates);
            section.sent = sent(&index, lines, kind, &rates);
        }
        let counts = index.counts();
        self.previous = Some((now, counts));
        health
    }
}

/// One read's reports by type.
struct Index<'a> {
    inbound: Vec<&'a InboundRtpStats>,
    outbound: Vec<&'a OutboundRtpStats>,
    remote: Vec<&'a RemoteInboundRtpStats>,
    codecs: HashMap<&'a str, &'a str>,
    pair: Option<&'a CandidatePairStats>,
}

impl<'a> Index<'a> {
    fn of(stats: &'a [RtcStats]) -> Self {
        let mut index = Index {
            inbound: Vec::new(),
            outbound: Vec::new(),
            remote: Vec::new(),
            codecs: HashMap::new(),
            pair: None,
        };
        let mut pairs = Vec::new();
        let mut selected = Vec::new();
        for report in stats {
            match report {
                RtcStats::InboundRtp(stream) => index.inbound.push(stream),
                RtcStats::OutboundRtp(stream) => index.outbound.push(stream),
                RtcStats::RemoteInboundRtp(stream) => index.remote.push(stream),
                RtcStats::Codec(codec) => {
                    index
                        .codecs
                        .insert(codec.rtc.id.as_str(), codec.codec.mime_type.as_str());
                }
                RtcStats::CandidatePair(pair) => pairs.push(pair),
                RtcStats::Transport(transport) => {
                    selected.push(transport.transport.selected_candidate_pair_id.as_str());
                }
                _ => {}
            }
        }
        // the transport's selected pair, else the pair ICE nominated
        index.pair = pairs
            .iter()
            .find(|pair| selected.contains(&pair.rtc.id.as_str()))
            .or_else(|| pairs.iter().find(|pair| pair.candidate_pair.nominated))
            .copied();
        index
    }

    /// The codec a stream names, by its MIME subtype.
    fn codec(&self, id: &str) -> Option<String> {
        let mime = self.codecs.get(id)?;
        let name = mime.rsplit('/').next().unwrap_or(mime);
        (!name.is_empty()).then(|| name.to_owned())
    }

    /// What the far end reports of the sent stream `id`.
    fn remote_of(&self, id: &str) -> Option<&'a RemoteInboundRtpStats> {
        self.remote
            .iter()
            .find(|remote| remote.remote_inbound.local_id == id)
            .copied()
    }

    /// Every stream's counts, by its report's id.
    fn counts(&self) -> HashMap<String, Counts> {
        let inbound = self.inbound.iter().map(|stream| {
            let counts = Counts {
                bytes: stream.inbound.bytes_received,
                packets: stream.received.packets_received,
                lost: stream.received.packets_lost,
            };
            (stream.rtc.id.clone(), counts)
        });
        let outbound = self.outbound.iter().map(|stream| {
            let counts = Counts {
                bytes: stream.sent.bytes_sent,
                packets: stream.sent.packets_sent,
                lost: 0,
            };
            (stream.rtc.id.clone(), counts)
        });
        inbound.chain(outbound).collect()
    }
}

/// The last read's counts and the time since, for the rates.
struct Rates<'a> {
    before: &'a HashMap<String, Counts>,
    elapsed: Option<f64>,
}

/// What some streams carried since the last read: bytes, packets and packets lost, added together.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Moved {
    bytes: u64,
    packets: u64,
    lost: u64,
    /// Some stream of them was in the last read.
    known: bool,
}

impl Rates<'_> {
    /// What the stream `id` carried since the last read, its counts now `now`.
    fn moved(&self, id: &str, now: Counts) -> Moved {
        let Some(before) = self.before.get(id) else {
            return Moved::default();
        };
        Moved {
            bytes: now.bytes.saturating_sub(before.bytes),
            packets: now.packets.saturating_sub(before.packets),
            lost: u64::try_from(now.lost.saturating_sub(before.lost)).unwrap_or(0),
            known: true,
        }
    }

    fn kbps(&self, moved: Moved) -> Option<f64> {
        let seconds = self.elapsed.filter(|_| moved.known)?;
        #[expect(
            clippy::cast_precision_loss,
            reason = "a call's bytes between two reads are far below 2^52"
        )]
        let bits = (moved.bytes * 8) as f64;
        Some(bits / seconds / 1000.0)
    }
}

impl std::ops::Add for Moved {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            bytes: self.bytes + other.bytes,
            packets: self.packets + other.packets,
            lost: self.lost + other.lost,
            known: self.known || other.known,
        }
    }
}

/// The share of packets lost of those that should have come, percent; none when none came or went missing.
fn loss_percent(moved: Moved) -> Option<f64> {
    let expected = moved.packets + moved.lost;
    #[expect(
        clippy::cast_precision_loss,
        reason = "packet counts between two reads are far below 2^52"
    )]
    let share = (moved.known && expected > 0).then(|| moved.lost as f64 / expected as f64 * 100.0);
    share
}

/// A duration libwebrtc gives in seconds, in ms; none when it measured nothing (0).
fn ms(seconds: f64) -> Option<f64> {
    (seconds > 0.0).then_some(seconds * 1000.0)
}

/// A frame rate, a resolution, a bitrate libwebrtc gives as 0 when it has none.
fn positive(value: f64) -> Option<f64> {
    (value > 0.0).then_some(value)
}

fn resolution(width: u32, height: u32) -> Option<(u32, u32)> {
    (width > 0 && height > 0).then_some((width, height))
}

/// The larger of two measures, either one missing.
fn larger(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// Whether a video's coder runs in hardware (libwebrtc's power-efficient one); none when libwebrtc names no coder.
fn hardware(implementation: &str, power_efficient: bool) -> Option<bool> {
    (!implementation.is_empty()).then_some(power_efficient)
}

fn area(resolution: Option<(u32, u32)>) -> u64 {
    resolution.map_or(0, |(width, height)| u64::from(width) * u64::from(height))
}

/// What the streams of `kind` received; none when none of them received anything.
fn received(index: &Index<'_>, lines: &Lines, kind: Kind, rates: &Rates<'_>) -> Option<StreamHealth> {
    let streams: Vec<&InboundRtpStats> = index
        .inbound
        .iter()
        .filter(|stream| lines.kind(&stream.stream.kind, &stream.inbound.mid) == Some(kind))
        .filter(|stream| stream.received.packets_received > 0)
        .copied()
        .collect();
    let mut moved = Moved::default();
    let mut health = StreamHealth::default();
    for stream in &streams {
        let counts = Counts {
            bytes: stream.inbound.bytes_received,
            packets: stream.received.packets_received,
            lost: stream.received.packets_lost,
        };
        moved = moved + rates.moved(&stream.rtc.id, counts);
        health.packets += stream.received.packets_received;
        health.jitter_ms = larger(health.jitter_ms, ms(stream.received.jitter));
        let shown = resolution(stream.inbound.frame_width, stream.inbound.frame_height);
        if health.codec.is_none() || area(shown) > area(health.resolution) {
            health.resolution = shown;
            health.frame_rate = positive(stream.inbound.frames_per_second);
            health.codec = index.codec(&stream.stream.codec_id);
            health.hardware = hardware(
                &stream.inbound.decoder_implementation,
                stream.inbound.power_efficient_decoder,
            );
        }
    }
    health.bitrate_kbps = rates.kbps(moved);
    health.loss_percent = loss_percent(moved);
    (!streams.is_empty()).then_some(health)
}

/// What the streams of `kind` sent; none when none of them sends (a camera off, a screen not shared).
fn sent(index: &Index<'_>, lines: &Lines, kind: Kind, rates: &Rates<'_>) -> Option<StreamHealth> {
    let streams: Vec<&OutboundRtpStats> = index
        .outbound
        .iter()
        .filter(|stream| lines.kind(&stream.stream.kind, &stream.outbound.mid) == Some(kind))
        .filter(|stream| stream.sent.packets_sent > 0 && (kind == Kind::Audio || stream.outbound.active))
        .copied()
        .collect();
    let mut moved = Moved::default();
    let mut health = StreamHealth::default();
    for stream in &streams {
        let counts = Counts {
            bytes: stream.sent.bytes_sent,
            packets: stream.sent.packets_sent,
            lost: 0,
        };
        moved = moved + rates.moved(&stream.rtc.id, counts);
        health.packets += stream.sent.packets_sent;
        if let Some(remote) = index.remote_of(&stream.rtc.id) {
            health.loss_percent = larger(health.loss_percent, Some(remote.remote_inbound.fraction_lost * 100.0));
            health.jitter_ms = larger(health.jitter_ms, ms(remote.received.jitter));
            health.round_trip_ms = larger(health.round_trip_ms, ms(remote.remote_inbound.round_trip_time));
        }
        let sending = resolution(stream.outbound.frame_width, stream.outbound.frame_height);
        if health.codec.is_none() || area(sending) > area(health.resolution) {
            health.resolution = sending;
            health.frame_rate = positive(stream.outbound.frames_per_second);
            health.codec = index.codec(&stream.stream.codec_id);
            health.hardware = hardware(
                &stream.outbound.encoder_implementation,
                stream.outbound.power_efficient_encoder,
            );
        }
    }
    health.bitrate_kbps = rates.kbps(moved);
    (!streams.is_empty()).then_some(health)
}

/// The connection's measures: the selected pair's round trip and estimates, everything received's loss, the received
/// audio's jitter, the far end's reported loss.
fn network(index: &Index<'_>, rates: &Rates<'_>) -> NetworkHealth {
    let received = index.inbound.iter().fold(Moved::default(), |moved, stream| {
        let counts = Counts {
            bytes: stream.inbound.bytes_received,
            packets: stream.received.packets_received,
            lost: stream.received.packets_lost,
        };
        moved + rates.moved(&stream.rtc.id, counts)
    });
    let jitter = index
        .inbound
        .iter()
        .filter(|stream| stream.stream.kind == "audio")
        .map(|stream| ms(stream.received.jitter))
        .fold(None, larger);
    let sent_loss = index
        .remote
        .iter()
        .map(|remote| Some(remote.remote_inbound.fraction_lost * 100.0))
        .fold(None, larger);
    let pair = index.pair.map(|pair| &pair.candidate_pair);
    NetworkHealth {
        // The SDK preserves field presence: a reported zero is valid, while a missing RTT stays unknown.
        round_trip_ms: pair
            .and_then(|pair| pair.current_round_trip_time)
            .map(|seconds| seconds * 1000.0)
            .filter(|milliseconds| milliseconds.is_finite() && *milliseconds >= 0.0),
        received_loss_percent: loss_percent(received),
        received_jitter_ms: jitter,
        sent_loss_percent: sent_loss,
        available_send_kbps: pair.and_then(|pair| positive(pair.available_outgoing_bitrate / 1000.0)),
        available_receive_kbps: pair.and_then(|pair| positive(pair.available_incoming_bitrate / 1000.0)),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use libwebrtc::stats::{CodecStats, TransportStats};

    use super::*;

    fn inbound(id: &str, kind: &str, mid: &str, (bytes, packets, lost): (u64, u64, i64)) -> InboundRtpStats {
        let mut stream = InboundRtpStats::default();
        stream.rtc.id = id.to_owned();
        stream.stream.kind = kind.to_owned();
        stream.stream.codec_id = format!("C-{kind}");
        stream.inbound.mid = mid.to_owned();
        stream.inbound.bytes_received = bytes;
        stream.received.packets_received = packets;
        stream.received.packets_lost = lost;
        stream
    }

    fn outbound(id: &str, kind: &str, mid: &str, (bytes, packets): (u64, u64), active: bool) -> OutboundRtpStats {
        let mut stream = OutboundRtpStats::default();
        stream.rtc.id = id.to_owned();
        stream.stream.kind = kind.to_owned();
        stream.stream.codec_id = format!("C-{kind}");
        stream.outbound.mid = mid.to_owned();
        stream.outbound.active = active;
        stream.sent.bytes_sent = bytes;
        stream.sent.packets_sent = packets;
        stream
    }

    fn codec(kind: &str, mime: &str) -> RtcStats {
        let mut codec = CodecStats::default();
        codec.rtc.id = format!("C-{kind}");
        codec.codec.mime_type = mime.to_owned();
        RtcStats::Codec(codec)
    }

    fn pair(id: &str, rtt: f64, nominated: bool) -> RtcStats {
        let mut pair = CandidatePairStats::default();
        pair.rtc.id = id.to_owned();
        pair.candidate_pair.nominated = nominated;
        pair.candidate_pair.current_round_trip_time = Some(rtt);
        pair.candidate_pair.available_outgoing_bitrate = 1_500_000.0;
        RtcStats::CandidatePair(pair)
    }

    fn transport(selected: &str) -> RtcStats {
        let mut transport = TransportStats::default();
        transport.transport.selected_candidate_pair_id = selected.to_owned();
        RtcStats::Transport(transport)
    }

    fn remote(local: &str, fraction_lost: f64, rtt: f64, jitter: f64) -> RtcStats {
        let mut remote = RemoteInboundRtpStats::default();
        remote.remote_inbound.local_id = local.to_owned();
        remote.remote_inbound.fraction_lost = fraction_lost;
        remote.remote_inbound.round_trip_time = rtt;
        remote.received.jitter = jitter;
        RtcStats::RemoteInboundRtp(remote)
    }

    /// A 1:1 call's audio both ways, the camera's video both ways, the far end's share received; `scale` the counts.
    fn call(scale: u64) -> Vec<RtcStats> {
        let mut audio_in = inbound(
            "IA",
            "audio",
            "0",
            (2_000 * scale, 50 * scale, i64::try_from(scale).unwrap()),
        );
        audio_in.received.jitter = 0.012;
        let mut video_in = inbound("IV", "video", "1", (50_000 * scale, 100 * scale, 0));
        video_in.inbound.frame_width = 640;
        video_in.inbound.frame_height = 360;
        video_in.inbound.frames_per_second = 24.0;
        let mut share_in = inbound("IS", "video", "3", (25_000 * scale, 40 * scale, 0));
        share_in.inbound.frame_width = 1920;
        share_in.inbound.frame_height = 1080;
        share_in.inbound.frames_per_second = 5.0;
        let mut video_out = outbound("OV", "video", "1", (40_000 * scale, 90 * scale), true);
        video_out.outbound.frame_width = 1280;
        video_out.outbound.frame_height = 720;
        video_out.outbound.frames_per_second = 30.0;
        video_out.outbound.encoder_implementation = "OpenH264".to_owned();
        vec![
            RtcStats::InboundRtp(audio_in),
            RtcStats::InboundRtp(video_in),
            RtcStats::InboundRtp(share_in),
            RtcStats::OutboundRtp(outbound("OA", "audio", "0", (1_000 * scale, 50 * scale), true)),
            RtcStats::OutboundRtp(video_out),
            RtcStats::OutboundRtp(outbound("OS", "video", "4", (0, 0), false)),
            remote("OA", 0.02, 0.040, 0.005),
            codec("audio", "audio/opus"),
            codec("video", "video/H264"),
            pair("P1", 0.300, true),
            pair("P2", 0.035, false),
            transport("P2"),
            RtcStats::Track,
        ]
    }

    fn lines() -> Lines {
        Lines {
            sharing: vec!["3".to_owned()],
            screen: Some("4".to_owned()),
        }
    }

    #[test]
    fn a_first_read_has_counts_and_measures_but_no_rates() {
        let health = HealthReader::default().read(&call(1), &lines(), Instant::now());
        let audio = health.audio.received.unwrap();
        assert_eq!(audio.packets, 50);
        assert_eq!(audio.bitrate_kbps, None);
        assert_eq!(audio.loss_percent, None);
        assert_eq!(audio.jitter_ms, Some(12.0));
        assert_eq!(audio.codec.as_deref(), Some("opus"));
        let sent = health.audio.sent.unwrap();
        assert_eq!(sent.packets, 50);
        assert_eq!(sent.loss_percent, Some(2.0));
        assert_eq!(sent.round_trip_ms, Some(40.0));
        assert_eq!(sent.jitter_ms, Some(5.0));
    }

    #[test]
    fn the_network_is_the_selected_pair_s_and_every_stream_s() {
        let health = HealthReader::default().read(&call(1), &lines(), Instant::now());
        // the transport's selected pair, not the nominated one
        assert_eq!(health.network.round_trip_ms, Some(35.0));
        assert_eq!(health.network.available_send_kbps, Some(1_500.0));
        assert_eq!(health.network.available_receive_kbps, None);
        assert_eq!(health.network.received_jitter_ms, Some(12.0));
        assert_eq!(health.network.sent_loss_percent, Some(2.0));
        assert_eq!(health.network.received_loss_percent, None);
    }

    #[test]
    fn rates_come_from_the_difference_between_two_reads() {
        let mut reader = HealthReader::default();
        let start = Instant::now();
        reader.read(&call(1), &lines(), start);
        let health = reader.read(&call(3), &lines(), start + Duration::from_secs(2));
        let audio = health.audio.received.unwrap();
        // 4 000 bytes in 2 s
        assert_eq!(audio.bitrate_kbps, Some(16.0));
        // 2 lost of the 102 that should have come
        let loss = audio.loss_percent.unwrap();
        assert!((loss - 2.0 / 102.0 * 100.0).abs() < 1e-9, "{loss}");
        assert_eq!(health.audio.sent.unwrap().bitrate_kbps, Some(8.0));
        assert_eq!(health.video.received.unwrap().bitrate_kbps, Some(400.0));
        assert!(health.network.received_loss_percent.is_some());
    }

    #[test]
    fn the_camera_s_lines_and_the_screen_s_are_apart() {
        let health = HealthReader::default().read(&call(1), &lines(), Instant::now());
        let video = health.video.received.unwrap();
        assert_eq!((video.resolution, video.frame_rate), (Some((640, 360)), Some(24.0)));
        assert_eq!(video.codec.as_deref(), Some("H264"));
        let sent = health.video.sent.unwrap();
        assert_eq!((sent.resolution, sent.frame_rate), (Some((1280, 720)), Some(30.0)));
        assert_eq!(sent.hardware, Some(false), "software H.264");
        assert_eq!(video.hardware, None, "no decoder named");
        assert!(!health.sharing.is_empty());
        assert_eq!(health.sharing.sent, None, "our screen's line sends nothing yet");
        let share = health.sharing.received.unwrap();
        assert_eq!((share.resolution, share.packets), (Some((1920, 1080)), 40));
    }

    #[test]
    fn a_camera_off_and_a_call_without_video_show_no_video() {
        let mut stats = call(1);
        stats.retain(|report| !matches!(report, RtcStats::InboundRtp(stream) if stream.stream.kind == "video"));
        for report in &mut stats {
            if let RtcStats::OutboundRtp(stream) = report {
                stream.outbound.active = stream.stream.kind == "audio";
            }
        }
        let health = HealthReader::default().read(&stats, &lines(), Instant::now());
        assert!(health.video.is_empty());
        assert!(health.sharing.is_empty());
        assert!(!health.audio.is_empty());
    }

    #[test]
    fn a_group_call_s_receiving_lines_add_up_and_show_the_largest_picture() {
        let mut small = inbound("I1", "video", "1", (1_000, 10, 0));
        small.inbound.frame_width = 320;
        small.inbound.frame_height = 180;
        let mut large = inbound("I2", "video", "2", (3_000, 30, 0));
        large.inbound.frame_width = 1280;
        large.inbound.frame_height = 720;
        large.inbound.frames_per_second = 15.0;
        let stats = [
            RtcStats::InboundRtp(small),
            RtcStats::InboundRtp(large),
            pair("P", 0.05, true),
        ];
        let health = HealthReader::default().read(&stats, &Lines::default(), Instant::now());
        let video = health.video.received.unwrap();
        assert_eq!(video.packets, 40);
        assert_eq!((video.resolution, video.frame_rate), (Some((1280, 720)), Some(15.0)));
        assert_eq!(video.codec, None, "no codec report");
        assert_eq!(health.network.round_trip_ms, Some(50.0), "the nominated pair");
    }

    #[test]
    fn candidate_rtt_preserves_absence_and_a_reported_zero_without_guessing() {
        // Synthetic getStats records cover the field's absence, zero, positive and invalid values.
        for (value, expected) in [
            (None, None),
            (Some(0.0), Some(0.0)),
            (Some(0.025), Some(25.0)),
            (Some(-1.0), None),
        ] {
            let mut record = serde_json::json!({"type":"candidate-pair","id":"P","nominated":true});
            if let Some(value) = value {
                record["currentRoundTripTime"] = serde_json::json!(value);
            }
            let stats: RtcStats = serde_json::from_value(record).unwrap();
            let health = HealthReader::default().read(&[stats], &Lines::default(), Instant::now());
            assert_eq!(health.network.round_trip_ms, expected);
        }
        for value in [f64::NAN, f64::INFINITY, f64::MAX] {
            let health = HealthReader::default().read(&[pair("P", value, true)], &Lines::default(), Instant::now());
            assert_eq!(health.network.round_trip_ms, None);
        }
    }

    #[test]
    fn nothing_read_is_nothing_shown() {
        let health = HealthReader::default().read(&[], &Lines::default(), Instant::now());
        assert_eq!(health, CallHealth::default());
        assert_eq!(larger(None, Some(1.0)), Some(1.0));
        assert_eq!(larger(Some(2.0), Some(1.0)), Some(2.0));
    }
}
