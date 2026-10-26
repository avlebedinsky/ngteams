//! A call's video (M3 Task 18): H.264 through OpenH264 (the bundled source, as every `test-support` build has it), and
//! two media sessions on loopback through the TURN test server sending each other the file backend's test pattern.
//!
//! - **The codec**: the test pattern encoded and decoded keeps its frame index; the first frame and every frame asked
//!   for is an IDR; the target bitrate stays between the minimum and the maximum, and a lower one makes smaller
//!   frames. Cisco's library is loaded only from a file that is there and is the one Cisco published.
//! - **The session**: A turns its camera on and offers its video line; B decodes A's pattern, in order and counting
//!   up; then B's camera too, both ways; the encoder's bitrate follows `str0m`'s bandwidth estimate; with loss on the
//!   relay the receiver asks for keyframes and the picture comes back; a camera turned off stops what is sent; a
//!   camera that goes away is off, the rest of the session going on.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

#[expect(
    dead_code,
    reason = "the helpers of tests/session.rs, of which the video tests take a few"
)]
mod support;

use std::time::{Duration, Instant};

use str0m::change::{SdpAnswer, SdpOffer};
use support::{A_TONE, B_TONE, CONNECT_WAIT, file_devices, negotiate, recording, start, wait_for};
use teams_media::Error;
use teams_media::devices::file::{FrameCounter, FrameCounts};
use teams_media::devices::pattern::{TestPattern, index_of};
use teams_media::devices::{DeviceError, Devices, PixelFormat, VideoFrame, VideoSink, VideoSource};
use teams_media::session::{MediaSession, SessionDevice, SessionEvent, VideoParts, VideoStats};
use teams_media::test_turn::{Impairment, TurnServer};
use teams_media::video::codec::{CISCO_LIBRARY, EncoderSettings, H264Library, MIN_BITRATE, VideoDecoder, VideoEncoder};
use teams_media::video::convert::MaxSize;

/// When frame `index` of a camera at `fps` was captured, into the stream.
fn at(index: u32, fps: u32) -> Duration {
    Duration::from_secs(1) * index / fps
}

fn settings() -> EncoderSettings {
    EncoderSettings {
        max_bitrate: 1_000_000,
        start_bitrate: 600_000,
        fps: 15,
    }
}

#[test]
fn the_test_pattern_survives_encoding_and_decoding_with_its_index() {
    let pattern = TestPattern::new(640, 360);
    let mut encoder = VideoEncoder::new(&H264Library::Bundled, settings()).unwrap();
    let mut decoder = VideoDecoder::new(&H264Library::Bundled).unwrap();
    let mut indices = Vec::new();
    for index in 0..30 {
        let Some(unit) = encoder.encode(&pattern.frame(index), at(index, 15), false).unwrap() else {
            continue;
        };
        assert_eq!(
            unit.keyframe,
            index == 0,
            "the first frame alone is an IDR (frame {index})"
        );
        assert!(unit.data.starts_with(&[0, 0, 0, 1]), "Annex B");
        if let Some(frame) = decoder.decode(&unit.data).unwrap() {
            assert_eq!((frame.width, frame.height), (640, 360));
            indices.push(index_of(&frame));
        }
    }
    assert!(indices.len() >= 25, "{} of 30 frames decoded", indices.len());
    let read: Vec<u32> = indices.iter().flatten().copied().collect();
    assert!(read.len() + 2 >= indices.len(), "the index is readable: {indices:?}");
    assert!(read.windows(2).all(|pair| pair[0] < pair[1]), "in order: {read:?}");
}

#[test]
fn a_keyframe_asked_for_is_an_idr() {
    let pattern = TestPattern::new(320, 240);
    let mut encoder = VideoEncoder::new(&H264Library::Bundled, settings()).unwrap();
    let kinds: Vec<bool> = (0..10)
        .filter_map(|index| {
            encoder
                .encode(&pattern.frame(index), at(index, 15), index == 6)
                .unwrap()
        })
        .map(|unit| unit.keyframe)
        .collect();
    assert!(kinds[0], "the first frame");
    assert!(
        kinds.iter().skip(1).filter(|keyframe| **keyframe).count() == 1,
        "{kinds:?}"
    );
}

#[test]
fn the_bitrate_stays_between_the_minimum_and_the_maximum_and_a_lower_one_makes_smaller_frames() {
    let mut encoder = VideoEncoder::new(&H264Library::Bundled, settings()).unwrap();
    assert_eq!(encoder.bitrate(), 600_000);
    assert_eq!(encoder.set_bitrate(10_000_000), 1_000_000);
    assert_eq!(encoder.set_bitrate(1_000), MIN_BITRATE);

    // noise: a picture no bitrate is enough for, so the rate control spends what it is given
    let size_at = |bps: u64| {
        let mut encoder = VideoEncoder::new(&H264Library::Bundled, settings()).unwrap();
        encoder.set_bitrate(bps);
        // past the first second (its IDR and the rate control settling), what the frames add up to
        (0..60)
            .filter_map(|index| encoder.encode(&noise(320, 240, index), at(index, 15), false).unwrap())
            .skip(15)
            .map(|unit| unit.data.len())
            .sum::<usize>()
    };
    let (low, high) = (size_at(150_000), size_at(1_000_000));
    assert!(low * 2 < high, "150 kb/s made {low} bytes, 1 Mb/s {high}");
}

/// An I420 frame of noise (seeded by `seed`), grey chroma.
fn noise(width: u32, height: u32, seed: u32) -> VideoFrame {
    let luma = (width * height) as usize;
    let mut state = u64::from(seed).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut data: Vec<u8> = (0..luma)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_be_bytes()[4]
        })
        .collect();
    data.resize(luma * 3 / 2, 128);
    VideoFrame {
        width,
        height,
        format: PixelFormat::I420,
        stride: width,
        data,
    }
}

#[test]
fn ciscos_library_loads_only_from_a_file_that_is_cisco_s() {
    let dir = tempfile::tempdir().unwrap();
    let missing = H264Library::cisco_in(dir.path());
    let error = missing.check().unwrap_err().to_string();
    assert!(error.contains(CISCO_LIBRARY), "{error}");

    std::fs::write(dir.path().join(CISCO_LIBRARY), b"not the library").unwrap();
    let error = H264Library::cisco_in(dir.path()).check().unwrap_err();
    assert!(matches!(error, Error::Video(_)), "{error}");
    assert!(error.to_string().to_lowercase().contains("hash"), "{error}");
    assert!(VideoEncoder::new(&H264Library::cisco_in(dir.path()), settings()).is_err());
    assert!(VideoDecoder::new(&H264Library::cisco_in(dir.path())).is_err());
}

/// `cargo tree` of each lab binary's crate alone (its normal and build dependencies with their features): none turns on
/// the binding's `source`, so a lab binary built normally carries no H.264 code of its own and loads only Cisco's
/// library (the bundled source is `test-support`'s alone).
#[test]
fn a_lab_binary_built_alone_compiles_no_h264_source() {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for binary in ["teams-headless", "teams-app", "proto-lab"] {
        let output = std::process::Command::new(&cargo)
            .current_dir(&root)
            .args([
                "tree",
                "--offline",
                "--locked",
                "-p",
                binary,
                "-e",
                "normal,build,features",
                "--prefix",
                "none",
            ])
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let tree = String::from_utf8(output.stdout).unwrap();
        if binary == "proto-lab" {
            assert!(!tree.contains("openh264"), "proto-lab takes no codec");
            continue;
        }
        let features: Vec<&str> = tree
            .lines()
            .filter(|line| line.starts_with("openh264-sys2 feature ") || line.starts_with("openh264 feature "))
            .collect();
        assert!(!features.is_empty(), "{binary} takes the codec: {tree}");
        assert!(
            features.iter().all(|line| line.ends_with(r#""libloading""#)),
            "{binary} turns on {features:?}"
        );
    }
}

/// One side of a video call: its session, what it decoded of the far end, and what its self view saw.
struct Side {
    session: MediaSession,
    remote: FrameCounter,
    preview: FrameCounter,
}

fn side(turn: &TurnServer, name: &str, dir: &std::path::Path) -> Side {
    let devices = file_devices(if name == "a" { A_TONE } else { B_TONE }, &recording(dir, name));
    let session = start(turn, &devices);
    let (remote, preview) = (FrameCounter::new(), FrameCounter::new());
    session
        .start_video(VideoParts {
            library: H264Library::Bundled,
            encoder: settings(),
            max_size: MaxSize::HD,
            remote: Box::new({
                let remote = remote.clone();
                move |_| Box::new(remote.clone()) as Box<dyn VideoSink>
            }),
            preview: Some(Box::new(preview.clone())),
        })
        .unwrap();
    Side {
        session,
        remote,
        preview,
    }
}

/// `from` turns its camera on and offers its video line sending; `to` answers. With no offer to make (the line
/// already sends: an offer `from` answered asked for it), the camera alone goes on.
fn camera_on(from: &MediaSession, to: &MediaSession) {
    let devices = file_devices(A_TONE, std::path::Path::new("/dev/null"));
    from.set_camera(Some(devices.camera().unwrap())).unwrap();
    let Some(offer) = from.offer_video(true).unwrap() else {
        return;
    };
    let offer = SdpOffer::from_sdp_string(&offer.to_sdp_string()).unwrap();
    assert!(offer.to_sdp_string().contains("H264/90000"), "H.264 offered");
    let answer = to.accept_offer(offer).unwrap();
    let answer = SdpAnswer::from_sdp_string(&answer.to_sdp_string()).unwrap();
    from.accept_answer(answer).unwrap();
}

/// Waits until `counts` says at least `frames` were shown, or `wait` passed; the counts then.
fn wait_frames(counter: &FrameCounter, frames: u64, wait: Duration) -> FrameCounts {
    let limit = Instant::now() + wait;
    while Instant::now() < limit && counter.counts().indexed < frames {
        std::thread::sleep(Duration::from_millis(50));
    }
    counter.counts()
}

fn connected(turn: &TurnServer, dir: &std::path::Path) -> (Side, Side) {
    let (a, b) = (side(turn, "a", dir), side(turn, "b", dir));
    negotiate(&a.session, &b.session);
    wait_for(&a.session, &SessionEvent::Connected, CONNECT_WAIT);
    wait_for(&b.session, &SessionEvent::Connected, CONNECT_WAIT);
    (a, b)
}

#[test]
fn a_camera_turned_on_reaches_the_far_end_in_order_both_ways() {
    let turn = TurnServer::start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = connected(&turn, dir.path());

    camera_on(&a.session, &b.session);
    let at_b = wait_frames(&b.remote, 30, Duration::from_secs(15));
    assert!(at_b.indexed >= 30, "B decoded A's pattern: {at_b:?}");
    // the rate control may skip a frame now and then (a gap), but most go
    assert!(
        at_b.gaps * 3 <= at_b.indexed,
        "in order: {at_b:?}, A {:?}",
        a.session.stats().video
    );
    assert!(at_b.changed >= at_b.frames - 1, "a moving picture: {at_b:?}");
    assert!(
        a.preview.counts().frames >= 30,
        "A's self view: {:?}",
        a.preview.counts()
    );
    assert_eq!(a.remote.counts().frames, 0, "B's camera is off");

    camera_on(&b.session, &a.session);
    let at_a = wait_frames(&a.remote, 30, Duration::from_secs(15));
    assert!(at_a.indexed >= 30, "A decoded B's pattern: {at_a:?}");
    let before = b.remote.counts().indexed;
    let at_b = wait_frames(&b.remote, before + 15, Duration::from_secs(10));
    assert!(at_b.indexed >= before + 15, "A's still reaches B: {at_b:?}");

    let (a, b) = (a.session.close(), b.session.close());
    for (who, stats) in [("A", a.video), ("B", b.video)] {
        assert!(
            stats.keyframes >= 1 && stats.sent > 30 && stats.shown > 30,
            "{who}: {stats:?}"
        );
    }
}

/// `sdp` without the header extensions that name a line or a stream (`sdes:mid`, `rtp-stream-id`, its repair), as
/// our offers go through Teams' dialect: the streams are then known only by the SSRCs the SDP names.
fn unnamed(sdp: &str) -> String {
    sdp.split("\r\n")
        .filter(|line| !line.contains("urn:ietf:params:rtp-hdrext:sdes:"))
        .collect::<Vec<_>>()
        .join("\r\n")
}

/// `from`'s offer `offer` answered by `to` and applied, both without line-naming extensions; `edit` changes the
/// offer before `to` takes it.
fn exchange(from: &MediaSession, to: &MediaSession, offer: &SdpOffer, edit: impl Fn(String) -> String) -> String {
    let offer = edit(unnamed(&offer.to_sdp_string()));
    let answer = to.accept_offer(SdpOffer::from_sdp_string(&offer).unwrap()).unwrap();
    let answer = unnamed(&answer.to_sdp_string());
    from.accept_answer(SdpAnswer::from_sdp_string(&answer).unwrap())
        .unwrap();
    answer
}

#[test]
fn a_far_end_that_only_received_turns_its_camera_on_later_and_names_its_stream() {
    let turn = TurnServer::start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (side(&turn, "a", dir.path()), side(&turn, "b", dir.path()));
    exchange(&a.session, &b.session, &a.session.offer_audio().unwrap(), |offer| offer);
    wait_for(&a.session, &SessionEvent::Connected, CONNECT_WAIT);
    wait_for(&b.session, &SessionEvent::Connected, CONNECT_WAIT);

    // A's camera on, B answering that it only receives (its camera off, as the dialect has it read)
    let devices = file_devices(A_TONE, std::path::Path::new("/dev/null"));
    a.session.set_camera(Some(devices.camera().unwrap())).unwrap();
    let offer = a.session.offer_video(true).unwrap().unwrap();
    let answer = exchange(&a.session, &b.session, &offer, |offer| {
        let video_at = offer.find("m=video").unwrap_or(offer.len());
        format!(
            "{}{}",
            &offer[..video_at],
            offer[video_at..].replacen("a=sendrecv", "a=sendonly", 1)
        )
    });
    assert!(answer.contains("a=recvonly"), "{answer}");
    assert!(wait_frames(&b.remote, 15, Duration::from_secs(15)).indexed >= 15);

    // B's camera on later: its renegotiation names the stream it sends (its SSRC: nothing else names it), and A
    // shows B's pattern
    let devices = file_devices(B_TONE, std::path::Path::new("/dev/null"));
    b.session.set_camera(Some(devices.camera().unwrap())).unwrap();
    let offer = b.session.offer_video(true).unwrap().unwrap();
    let text = offer.to_sdp_string();
    let video = &text[text.find("m=video").unwrap()..];
    assert!(
        video.contains("a=ssrc:") && video.contains("a=ssrc-group:FID"),
        "{video}"
    );
    exchange(&b.session, &a.session, &offer, |offer| offer);
    let at_a = wait_frames(&a.remote, 15, Duration::from_secs(15));
    assert!(
        at_a.indexed >= 15,
        "{at_a:?}; A {:?}; B {:?}",
        a.session.stats().video,
        b.session.stats().video
    );
    let _ = (a.session.close(), b.session.close());
}

#[test]
fn the_encoder_follows_the_bandwidth_estimate() {
    let turn = TurnServer::start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = connected(&turn, dir.path());
    camera_on(&a.session, &b.session);
    wait_frames(&b.remote, 45, Duration::from_secs(15));
    let stats: VideoStats = a.session.stats().video;
    assert!(stats.estimate > 0, "str0m estimated the bandwidth: {stats:?}");
    let expected = stats
        .estimate
        .saturating_sub(64_000)
        .clamp(u64::from(MIN_BITRATE), 1_000_000);
    // the encoder takes the newest estimate at its next frame: within one estimate's change of it
    let off = stats.bitrate.abs_diff(expected);
    assert!(
        off <= expected / 4,
        "target {} for estimate {}: {stats:?}",
        stats.bitrate,
        stats.estimate
    );
    let _ = (a.session.close(), b.session.close());
}

#[test]
fn with_loss_the_receiver_asks_for_keyframes_and_the_picture_goes_on() {
    let turn = TurnServer::start_impaired(Impairment {
        loss_percent: 5,
        reorder_percent: 0,
        seed: 18,
    })
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = connected(&turn, dir.path());
    camera_on(&a.session, &b.session);
    let at_b = wait_frames(&b.remote, 60, Duration::from_secs(25));
    assert!(at_b.indexed >= 60, "the picture went on: {at_b:?}");
    let (a, b) = (a.session.close(), b.session.close());
    assert!(
        b.video.keyframes_requested >= 1 || b.video.undecoded == 0,
        "a broken picture asked for a keyframe: {:?}",
        b.video
    );
    assert!(a.video.keyframes >= 1, "{:?}", a.video);
}

#[test]
fn a_camera_turned_off_stops_what_is_sent() {
    let turn = TurnServer::start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = connected(&turn, dir.path());
    camera_on(&a.session, &b.session);
    wait_frames(&b.remote, 20, Duration::from_secs(15));
    a.session.set_camera(None).unwrap();
    assert!(!a.session.camera_on());
    std::thread::sleep(Duration::from_millis(500));
    let sent = a.session.stats().video.sent;
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(a.session.stats().video.sent, sent, "nothing more is sent");
    assert_eq!(a.session.stats().video.bitrate, 0);
    let _ = (a.session.close(), b.session.close());
}

/// A camera that went away (unplugged): every read fails.
struct GoneCamera;

impl VideoSource for GoneCamera {
    fn next_frame(&mut self, _timeout: Duration) -> Result<Option<VideoFrame>, DeviceError> {
        Err(DeviceError::Closed)
    }
}

#[test]
fn a_camera_that_goes_away_is_off_and_the_session_goes_on() {
    let turn = TurnServer::start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let a = side(&turn, "a", dir.path());
    a.session.set_camera(Some(Box::new(GoneCamera))).unwrap();
    wait_for(
        &a.session,
        &SessionEvent::DeviceClosed(SessionDevice::Camera),
        Duration::from_secs(5),
    );
    assert!(!a.session.camera_on(), "the camera is off");
    assert!(a.session.offer_video(false).is_ok(), "the session goes on");
    let _ = a.session.close();
}

#[test]
fn a_camera_before_the_video_started_is_refused() {
    let turn = TurnServer::start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let devices = file_devices(A_TONE, &recording(dir.path(), "a"));
    let session = start(&turn, &devices);
    let error = session.set_camera(Some(devices.camera().unwrap())).unwrap_err();
    assert!(matches!(error, Error::Video(_)), "{error}");
    assert!(!session.camera_on());
    let _ = session.close();
}
