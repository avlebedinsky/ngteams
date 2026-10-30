//! A call's video through libwebrtc (H.264): two media sessions on loopback through the TURN test server sending each
//! other the file backend's test pattern.
//!
//! A turns its camera on and offers its video line in a renegotiation; B shows A's pattern on its line-0 sink, in order
//! and counting up; then B's camera too, both ways; a far end that only received turns its camera on later; with loss
//! on the relay the picture goes on (libwebrtc's retransmissions and keyframe requests); a camera turned off stops what
//! is sent; a camera that goes away is off, the rest of the session going on.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

#[expect(
    dead_code,
    reason = "the helpers of tests/session.rs, of which the video tests take a few"
)]
mod support;

use std::time::{Duration, Instant};

use support::{A_TONE, B_TONE, CONNECT_WAIT, file_devices, negotiate, recording, start, wait_for};
use teams_media::Error;
use teams_media::devices::file::{FrameCounter, FrameCounts};
use teams_media::devices::{DeviceError, Devices, VideoFrame, VideoSink, VideoSource};
use teams_media::session::{MediaSession, SdpAnswer, SdpOffer, SessionDevice, SessionEvent, VideoParts};
use teams_media::test_turn::{Impairment, TurnServer};
use teams_media::video::codec::EncoderSettings;
use teams_media::video::convert::MaxSize;

fn settings() -> EncoderSettings {
    EncoderSettings {
        max_bitrate: 1_000_000,
        start_bitrate: 600_000,
        fps: 15,
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
            encoder: settings(),
            max_size: MaxSize::HD,
            remote: Box::new({
                let remote = remote.clone();
                // a 1:1 call's video comes on its main line (line 0); anything on another line isn't counted
                move |line| {
                    let sink = if line == 0 { remote.clone() } else { FrameCounter::new() };
                    Box::new(sink) as Box<dyn VideoSink>
                }
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
            stats.sent > 30 && stats.received > 30 && stats.shown > 30,
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
fn with_loss_the_picture_goes_on() {
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
        a.video.sent >= 60 && b.video.shown >= 60,
        "A {:?}, B {:?}",
        a.video,
        b.video
    );
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
