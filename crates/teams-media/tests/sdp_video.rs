//! The video lines in Teams' dialect, through the call's own peer (`teams_media::peer::Peer`: Opus and H.264):
//! which of an offer's video lines a call takes ([`VideoLines`]) and how our camera's line is offered.
//!
//! - A 1:1 call takes the main video line of the web client's camera-on offer and rejects the sharing line; with our
//!   camera off it answers that it only receives, with it on that it sends too; H.264 as the web client numbers
//!   constrained baseline.
//! - An audio-only call (a group call's, until it takes video) rejects every video line as the web client writes a
//!   rejected line (`m=video 0 RTP/SAVP 34`), the mixer's twelve too, and leaves them out of the bundle: as before
//!   H.264 was configured (M3 Task 16's `410 SdpParsingFailure`).
//! - Our camera's line offered in a renegotiation is written as the web client writes its main video line, and a
//!   native far end that reads it takes it and answers it. A line whose offer was turned away (`491`) is no line:
//!   the camera turned off and on again before it went again offers it anew, numbered as it was.

use std::time::Instant;

use serde_json::Value;
use str0m::Candidate;
use str0m::net::Protocol;
use teams_media::peer::{H264_PT, Peer};
use teams_media::sdp::{NgcSdp, NgcSession, VideoLines};

const FIXTURE: &str = include_str!("fixtures/sdp/captured.json");

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn captured(name: &str) -> TestResult<NgcSdp> {
    let fixture: Value = serde_json::from_str(FIXTURE)?;
    let content_type = fixture[name]["contentType"].as_str().ok_or("no content type")?;
    let blob = fixture[name]["blob"].as_str().ok_or("no blob")?;
    Ok(NgcSdp::new(content_type, blob)?)
}

/// The call's peer behind a relayed candidate (a documentation address), as the native client's is.
fn peer(port: u16) -> TestResult<Peer> {
    let candidate = Candidate::relayed(
        format!("192.0.2.50:{port}").parse()?,
        "0.0.0.0:41000".parse()?,
        Protocol::Udp,
    )?;
    Ok(Peer::with_candidate(Instant::now(), candidate)?)
}

/// The media sections of an SDP, as lines.
fn sections(text: &str) -> Vec<Vec<&str>> {
    let mut out: Vec<Vec<&str>> = Vec::new();
    for line in text.split("\r\n").filter(|line| !line.is_empty()) {
        if line.starts_with("m=") {
            out.push(Vec::new());
        }
        if let Some(section) = out.last_mut() {
            section.push(line);
        }
    }
    out
}

fn has(section: &[&str], line: &str) -> bool {
    section.contains(&line)
}

/// Our answer, in Teams' dialect, to the captured offer `name` read with `lines`.
fn answer(name: &str, lines: VideoLines) -> TestResult<String> {
    let mut session = NgcSession::new();
    session.set_video(lines);
    let mut peer = peer(50000)?;
    let offer = session.read_offer(&captured(name)?)?;
    let answer = peer.accept_offer(offer)?;
    Ok(session.write_answer(&answer)?.as_str().to_owned())
}

#[test]
fn a_one_to_one_call_takes_the_main_video_line_receiving_while_the_camera_is_off() -> TestResult {
    let text = answer("caller_renegotiation_offer", VideoLines::Main { sending: false })?;
    let lines = sections(&text);
    let video = &lines[1];
    assert!(
        video[0].starts_with("m=video ") && !video[0].starts_with("m=video 0 "),
        "{video:?}"
    );
    // H.264 as constrained baseline, on the offer's own number for it (the web client's `108`, not ours)
    assert!(has(video, "a=rtpmap:108 H264/90000"), "{video:?}");
    assert!(
        video
            .iter()
            .any(|line| line.starts_with("a=fmtp:108 ") && line.contains("profile-level-id=42e01f")),
        "{video:?}"
    );
    assert!(has(video, "a=recvonly"), "{video:?}");
    assert!(has(video, "a=label:main-video"));
    assert_eq!(lines[2], ["m=video 0 RTP/SAVP 34", "a=label:applicationsharing-video"]);
    assert!(text.contains("a=group:BUNDLE 0 1 3"), "{text}");
    Ok(())
}

#[test]
fn with_the_camera_on_the_main_video_line_sends_too() -> TestResult {
    let text = answer("caller_renegotiation_offer", VideoLines::Main { sending: true })?;
    let video = &sections(&text)[1];
    assert!(has(video, "a=sendrecv"), "{video:?}");
    Ok(())
}

#[test]
fn an_audio_only_call_rejects_every_video_line() -> TestResult {
    let text = answer("caller_renegotiation_offer", VideoLines::None)?;
    let lines = sections(&text);
    assert_eq!(lines[1], ["m=video 0 RTP/SAVP 34", "a=label:main-video"]);
    assert_eq!(lines[2], ["m=video 0 RTP/SAVP 34", "a=label:applicationsharing-video"]);
    assert!(!text.contains("H264"), "{text}");
    assert!(text.contains("a=group:BUNDLE 0 3"), "{text}");
    Ok(())
}

#[test]
fn the_mixers_video_lines_stay_rejected_in_a_group_call() -> TestResult {
    let text = answer("mixer_offer", VideoLines::None)?;
    let lines = sections(&text);
    let video: Vec<&Vec<&str>> = lines
        .iter()
        .filter(|section| section[0].starts_with("m=video"))
        .collect();
    assert_eq!(video.len(), 11);
    for section in video {
        assert_eq!(section[0], "m=video 0 RTP/SAVP 34", "{section:?}");
        assert_eq!(section.len(), 2, "its label alone: {section:?}");
    }
    assert!(!text.contains("H264"), "{text}");
    assert!(lines[0][0].starts_with("m=audio ") && !lines[0][0].starts_with("m=audio 0 "));
    Ok(())
}

#[test]
fn our_cameras_line_is_offered_as_the_web_clients_main_video_line_and_a_native_far_end_takes_it() -> TestResult {
    // the call: our audio offer answered by a native far end
    let (mut ours, mut theirs) = (peer(50000)?, peer(50002)?);
    let (mut our_dialect, mut their_dialect) = (NgcSession::new(), NgcSession::new());
    their_dialect.set_video(VideoLines::Main { sending: false });
    let (offer, pending) = ours.offer_audio()?;
    let offer = our_dialect.write_offer(&offer)?;
    let answer = theirs.accept_offer(their_dialect.read_offer(&offer)?)?;
    ours.accept_answer(pending, our_dialect.read_answer(&their_dialect.write_answer(&answer)?)?)?;

    // our camera goes on: the renegotiation adds our video line
    let (offer, pending) = ours.offer_video(true)?.ok_or("an offer")?;
    let offer = our_dialect.write_offer(&offer)?;
    let lines = sections(offer.as_str());
    let video = &lines[1];
    assert!(video[0].starts_with("m=video 50000 RTP/SAVP "), "{video:?}");
    assert!(has(video, &format!("a=rtpmap:{H264_PT} H264/90000")), "{video:?}");
    assert!(has(video, "a=sendrecv"));
    assert!(has(video, "a=mid:1") && has(video, "a=label:main-video"), "{video:?}");
    assert!(
        !offer.as_str().contains("sdes:mid"),
        "no line-naming extension (M3 Task 16)"
    );

    // the far end (camera off) takes it receiving
    let answer = theirs.accept_offer(their_dialect.read_offer(&offer)?)?;
    let answer = their_dialect.write_answer(&answer)?;
    assert!(has(&sections(answer.as_str())[1], "a=recvonly"), "{}", answer.as_str());
    ours.accept_answer(pending, our_dialect.read_answer(&answer)?)?;
    assert!(ours.video_sending(), "our line sends");
    // (the far end names the lines of an offer it took once the media connects)
    assert!(ours.video_mid().is_some());
    Ok(())
}

#[test]
fn a_camera_turned_off_and_on_while_its_offer_was_turned_away_offers_its_line_anew() -> TestResult {
    let (mut ours, mut theirs) = (peer(50000)?, peer(50002)?);
    let (mut our_dialect, mut their_dialect) = (NgcSession::new(), NgcSession::new());
    their_dialect.set_video(VideoLines::Main { sending: false });
    let (offer, pending) = ours.offer_audio()?;
    let offer = our_dialect.write_offer(&offer)?;
    let answer = theirs.accept_offer(their_dialect.read_offer(&offer)?)?;
    ours.accept_answer(pending, our_dialect.read_answer(&their_dialect.write_answer(&answer)?)?)?;

    // our camera goes on: the call controller turns its offer away (`491`), so no answer commits its line
    let (offer, _turned_away) = ours.offer_video(true)?.ok_or("an offer")?;
    let _ = our_dialect.write_offer(&offer)?;
    assert_eq!(ours.video_mid(), None, "no line until an answer commits it");

    // off and on again before that offer went again: each offers the line anew, never `str0m`'s pending line (a
    // stream declared on it panics)
    let (offer, _) = ours.offer_video(false)?.ok_or("an offer")?;
    let _ = our_dialect.write_offer(&offer)?;
    let (offer, pending) = ours.offer_video(true)?.ok_or("an offer")?;
    let offer = our_dialect.write_offer(&offer)?;
    let lines = sections(offer.as_str());
    assert_eq!(lines.len(), 2, "the lines turned away aren't offered: {lines:?}");
    assert!(
        has(&lines[1], "a=mid:1") && has(&lines[1], "a=sendrecv"),
        "numbered as the first: {:?}",
        lines[1]
    );

    // the far end takes it: the line is ours, and the camera's next turn changes it
    let answer = theirs.accept_offer(their_dialect.read_offer(&offer)?)?;
    ours.accept_answer(pending, our_dialect.read_answer(&their_dialect.write_answer(&answer)?)?)?;
    assert!(ours.video_sending(), "our line sends");
    let line = ours.video_mid();
    assert!(line.is_some());
    let (offer, _) = ours.offer_video(false)?.ok_or("an offer")?;
    assert_eq!(ours.video_mid(), line);
    let offer = our_dialect.write_offer(&offer)?;
    assert_eq!(sections(offer.as_str()).len(), 2);
    Ok(())
}
