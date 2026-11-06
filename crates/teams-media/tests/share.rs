//! A 1:1 call's shared screen through libwebrtc and Teams' dialect, both ways (`docs/protocol/calling.md`, "More of a
//! running call"): two media sessions on loopback through the TURN test server, each speaking the dialect.
//!
//! A shares its screen (the file backend's 720p pattern): its offer adds a line of its own, sending only, labelled
//! `applicationsharing-video`; B takes it (its camera off), answers it receiving, and shows A's screen on its sharing
//! sink, never on its camera's line. A stops: its offer rejects that line as the web client writes a rejected line, and
//! B's answer has no sharing line in use.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

#[expect(
    dead_code,
    reason = "the helpers of tests/session.rs, of which these tests take a few"
)]
mod support;

use std::time::{Duration, Instant};

use support::{A_TONE, B_TONE, CONNECT_WAIT, file_devices, recording, start, wait_for};
use teams_media::devices::file::FrameCounter;
use teams_media::devices::{Devices as _, VideoSink};
use teams_media::sdp::{NgcSdp, NgcSession, SharingLine, VideoLines};
use teams_media::session::{MediaSession, SHARING_LINE, SessionEvent, VideoParts};
use teams_media::test_turn::TurnServer;
use teams_media::video::codec::EncoderSettings;
use teams_media::video::convert::MaxSize;

/// One end of the call: its session, its dialect, and what it decoded on its camera's line and on its sharing line.
struct End {
    session: MediaSession,
    dialect: NgcSession,
    camera: FrameCounter,
    share: FrameCounter,
}

fn end(turn: &TurnServer, name: &str, dir: &std::path::Path) -> End {
    let devices = file_devices(if name == "a" { A_TONE } else { B_TONE }, &recording(dir, name));
    let session = start(turn, &devices);
    let (camera, share) = (FrameCounter::new(), FrameCounter::new());
    let (on_camera, on_share) = (camera.clone(), share.clone());
    session
        .start_video(VideoParts {
            encoder: EncoderSettings::default(),
            max_size: MaxSize::HD,
            remote: Box::new(move |line| {
                let sink = match line {
                    0 => on_camera.clone(),
                    SHARING_LINE => on_share.clone(),
                    _ => FrameCounter::new(),
                };
                Box::new(sink) as Box<dyn VideoSink>
            }),
            preview: None,
            background: teams_media::video::effects::BackgroundChoice::default(),
        })
        .unwrap();
    let mut dialect = NgcSession::new();
    dialect.set_video(VideoLines::Main { sending: false });
    End {
        session,
        dialect,
        camera,
        share,
    }
}

/// `to` answers `offer` (A's, in Teams' dialect) and `from` takes the answer: B's answer, in the dialect.
fn exchange(from: &mut End, to: &mut End, offer: &NgcSdp) -> NgcSdp {
    let read = to.dialect.read_offer(offer).unwrap();
    to.session.set_sharing_lines(to.dialect.sharing_mids());
    let answer = to.session.accept_offer(read).unwrap();
    let answer = to.dialect.write_answer(&answer).unwrap();
    let read = from.dialect.read_answer(&answer).unwrap();
    from.session.set_sharing_lines(from.dialect.sharing_mids());
    from.session.accept_answer(read).unwrap();
    answer
}

/// The media sections of an SDP in Teams' dialect, as lines.
fn sections(sdp: &NgcSdp) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = Vec::new();
    for line in sdp.as_str().split("\r\n").filter(|line| !line.is_empty()) {
        if line.starts_with("m=") {
            out.push(Vec::new());
        }
        if let Some(section) = out.last_mut() {
            section.push(line.to_owned());
        }
    }
    out
}

/// The section labelled the sharing line.
fn sharing_section(sdp: &NgcSdp) -> Vec<String> {
    sections(sdp)
        .into_iter()
        .find(|section| section.iter().any(|line| line == "a=label:applicationsharing-video"))
        .unwrap_or_else(|| panic!("no sharing line: {}", sdp.as_str()))
}

fn wait_frames(counter: &FrameCounter, frames: u64, wait: Duration) -> u64 {
    let limit = Instant::now() + wait;
    while Instant::now() < limit && counter.counts().indexed < frames {
        std::thread::sleep(Duration::from_millis(50));
    }
    counter.counts().indexed
}

#[test]
fn a_screen_shared_goes_on_a_sharing_line_of_its_own_and_the_far_end_shows_it_apart() {
    let turn = TurnServer::start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (mut a, mut b) = (end(&turn, "a", dir.path()), end(&turn, "b", dir.path()));
    let offer = a.session.offer_audio().unwrap();
    let offer = a.dialect.write_offer(&offer).unwrap();
    exchange(&mut a, &mut b, &offer);
    wait_for(&a.session, &SessionEvent::Connected, CONNECT_WAIT);

    // A shares: a line of its own, sending only, labelled as the web client labels it
    let screen = file_devices(A_TONE, std::path::Path::new("/dev/null"))
        .screen()
        .unwrap();
    let (offer, mid) = a.session.share_screen(screen).unwrap();
    a.dialect.mark_sharing(&mid);
    let offer = a.dialect.write_offer(&offer).unwrap();
    assert_eq!(offer.sharing_line(), Some(SharingLine::Sends));
    let sharing = sharing_section(&offer);
    assert!(
        sharing[0].starts_with("m=video ") && !sharing[0].starts_with("m=video 0 "),
        "{sharing:?}"
    );
    assert!(sharing.iter().any(|line| line == "a=sendonly"), "{sharing:?}");
    assert!(a.session.sharing());

    // B takes it receiving (its camera off) and shows A's screen on its sharing sink alone
    let answer = exchange(&mut a, &mut b, &offer);
    assert_eq!(answer.sharing_line(), Some(SharingLine::Receives));
    assert!(sharing_section(&answer).iter().any(|line| line == "a=recvonly"));
    assert!(
        wait_frames(&b.share, 20, Duration::from_secs(15)) >= 20,
        "B shows A's screen"
    );
    assert_eq!(b.camera.counts().frames, 0, "nothing on B's camera line");
    assert!(a.session.stats().video.screen_sent >= 20);

    // A stops: its line rejected as the web client writes one, B's answer shows no sharing line in use
    let offer = a.session.stop_screen().unwrap().expect("a screen was shared");
    let offer = a.dialect.write_offer(&offer).unwrap();
    assert_eq!(offer.sharing_line(), None);
    assert_eq!(
        sharing_section(&offer),
        ["m=video 0 RTP/SAVP 34", "a=label:applicationsharing-video"]
    );
    let answer = exchange(&mut a, &mut b, &offer);
    assert_eq!(answer.sharing_line(), None);
    assert!(!a.session.sharing());
    assert!(a.session.stop_screen().unwrap().is_none(), "nothing left to stop");
}
