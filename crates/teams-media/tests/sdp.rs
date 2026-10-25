//! `sdp-ngc-1.0` both ways, on the shapes the M3 calling runs captured (`tests/fixtures/sdp/captured.json`, synthetic):
//! `str0m`'s offer written as the web client writes its call start's and read back from the media controller's and a
//! callee's answers; an invitation's offer (the caller's, and the mixer's in `sdp-ngc-0.5`) read into `str0m` and our
//! answer written; renegotiations offered by either side; a browser's SDP refused where the dialect is required.

use std::sync::{Arc, LazyLock};
use std::time::Instant;

use serde_json::Value;
use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::media::{Direction, MediaKind, Mid};
use str0m::net::Protocol;
use str0m::{Candidate, Rtc};
use teams_media::sdp::{Dialect, NgcSdp, NgcSession, SDP_NGC, SDP_NGC_MIXER, SdpError};

const FIXTURE: &str = include_str!("fixtures/sdp/captured.json");
/// Our relayed candidate: the allocation (a documentation address) and the socket to the relay.
const RELAYED: &str = "192.0.2.50:50000";
const RELAY_BASE: &str = "0.0.0.0:41000";

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

static CAPTURED: LazyLock<Result<Value, String>> =
    LazyLock::new(|| serde_json::from_str(FIXTURE).map_err(|error| error.to_string()));

/// A captured SDP's text as the service sent it or the web client wrote it.
fn captured_text(name: &str) -> TestResult<&'static str> {
    let fixture = CAPTURED.as_ref().map_err(Clone::clone)?;
    Ok(fixture[name]["blob"].as_str().ok_or("no such SDP in the fixture")?)
}

/// A captured SDP, with its content type.
fn captured(name: &str) -> TestResult<NgcSdp> {
    let fixture = CAPTURED.as_ref().map_err(Clone::clone)?;
    let content_type = fixture[name]["contentType"].as_str().ok_or("no content type")?;
    Ok(NgcSdp::new(content_type, captured_text(name)?)?)
}

/// A `str0m` peer whose only candidate is relayed, as the native client's is.
fn relayed_peer() -> TestResult<Rtc> {
    let mut rtc = Rtc::builder()
        .set_crypto_provider(Arc::new(str0m::crypto::from_feature_flags()))
        .build(Instant::now());
    rtc.add_local_candidate(Candidate::relayed(
        RELAYED.parse()?,
        RELAY_BASE.parse()?,
        Protocol::Udp,
    )?);
    Ok(rtc)
}

fn audio_offer(rtc: &mut Rtc) -> TestResult<(Mid, SdpOffer, SdpPendingOffer)> {
    let mut change = rtc.sdp_api();
    let mid = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
    let (offer, pending) = change.apply().ok_or("no offer")?;
    Ok((mid, offer, pending))
}

/// The lines of an SDP, without empty ones.
fn lines(text: &str) -> Vec<&str> {
    text.split("\r\n").filter(|line| !line.is_empty()).collect()
}

/// The session part, then each media section, as lines.
fn sections(text: &str) -> Vec<Vec<&str>> {
    let mut out = vec![Vec::new()];
    for line in lines(text) {
        if line.starts_with("m=") {
            out.push(Vec::new());
        }
        if let Some(section) = out.last_mut() {
            section.push(line);
        }
    }
    out
}

/// What a line is (`m`, `c`, `b`, or an attribute's name), with runs of the same kind collapsed: the shape of a
/// section, apart from how many codecs, extensions or candidates it lists.
fn shape<'a>(section: &[&'a str]) -> Vec<&'a str> {
    let mut kinds: Vec<&str> = Vec::new();
    for line in section {
        let kind = match line.strip_prefix("a=") {
            Some(rest) => rest.split(':').next().unwrap_or(rest),
            None => &line[..1],
        };
        if kinds.last() != Some(&kind) {
            kinds.push(kind);
        }
    }
    kinds
}

fn value<'a>(section: &[&'a str], name: &str) -> Option<&'a str> {
    let prefix = format!("a={name}:");
    section.iter().find_map(|line| line.strip_prefix(prefix.as_str()))
}

/// Each media section's label.
fn labels<'a>(parts: &[Vec<&'a str>]) -> Vec<Option<&'a str>> {
    parts[1..].iter().map(|section| value(section, "label")).collect()
}

/// Each media section's mid.
fn mids<'a>(parts: &[Vec<&'a str>]) -> Vec<Option<&'a str>> {
    parts[1..].iter().map(|section| value(section, "mid")).collect()
}

/// An SDP's text from its session part and media sections.
fn joined(parts: &[Vec<&str>]) -> String {
    parts.concat().join("\r\n") + "\r\n"
}

/// A session that read and answered the mixer's offer, as a callee in a group call.
fn mixer_session(rtc: &mut Rtc) -> TestResult<NgcSession> {
    let mut session = NgcSession::new();
    let offer = session.read_offer(&captured("mixer_offer")?)?;
    let answer = rtc.sdp_api().accept_offer(offer)?;
    session.write_answer(&answer)?;
    Ok(session)
}

// ---- the caller's side ----

#[test]
fn our_offer_is_written_in_the_shape_of_the_web_clients_call_start() -> TestResult {
    let mut rtc = relayed_peer()?;
    let (_, offer, _) = audio_offer(&mut rtc)?;
    let mut session = NgcSession::new();

    let ngc = session.write_offer(&offer).unwrap();

    assert_eq!(ngc.dialect(), Dialect::Ngc);
    assert_eq!(ngc.content_type(), SDP_NGC);
    let ours = sections(ngc.as_str());
    let web = sections(captured_text("start_offer")?);
    assert_eq!(shape(&ours[0]), shape(&web[0]), "the session part");
    assert_eq!(shape(&ours[1]), shape(&web[1]), "the audio line");
    assert_eq!(ours.len(), 2, "audio only");
    assert!(!ngc.as_str().contains("UDP/TLS"));
    assert!(!ngc.as_str().contains("str0m"));
    Ok(())
}

#[test]
fn our_offer_carries_the_relayed_candidate_and_teams_own_lines() -> TestResult {
    let mut rtc = relayed_peer()?;
    let (_, offer, _) = audio_offer(&mut rtc)?;
    let ngc = NgcSession::new().write_offer(&offer)?;
    let ours = sections(ngc.as_str());
    let web = sections(captured_text("start_offer")?);

    let audio = &ours[1];
    assert_eq!(
        audio[0], "m=audio 50000 RTP/SAVP 111",
        "the relayed candidate's port, Opus"
    );
    assert_eq!(audio[1], "c=IN IP4 192.0.2.50");
    assert_eq!(value(audio, "rtcp"), Some("50000"));
    assert_eq!(value(audio, "mid"), Some("0"), "Teams numbers its lines");
    assert_eq!(value(audio, "label"), Some("main-audio"));
    assert_eq!(value(&ours[0], "group"), Some("BUNDLE 0"));
    assert_eq!(ours[0][1].split(' ').next(), Some("o=-"));
    assert!(ours[0][1].ends_with(" IN IP4 127.0.0.1"));
    assert!(ours[0].contains(&"b=CT:4000"));
    let candidates: Vec<&str> = audio
        .iter()
        .filter(|line| line.starts_with("a=candidate:"))
        .copied()
        .collect();
    assert_eq!(
        candidates,
        ["a=candidate:1 1 udp 37748479 192.0.2.50 50000 typ relay raddr 0.0.0.0 rport 0"],
        "one relayed candidate, no related address, no ufrag extension"
    );
    // Teams' own lines exactly as the web client writes them
    for line in [
        "a=x-signaling-fb:* x-message app recv:dsh",
        r"a=extmap:2 http:\\www.webrtc.org\experiments\rtp-hdrext\abs-send-time",
        r"a=extmap:3 http:\\www.ietf.org\id\draft-holmer-rmcat-transport-wide-cc-extensions-01",
        "a=extmap:4 urn:ietf:params:rtp-hdrext:sdes:mid",
        "a=fmtp:111 minptime=10;useinbandfec=1",
        "a=rtpmap:111 opus/48000/2",
        "a=setup:actpass",
        "a=sendrecv",
        "a=rtcp-mux",
    ] {
        assert!(audio.contains(&line), "{line} in {audio:#?}");
        assert!(web[1].contains(&line), "{line} is the web client's too");
    }
    let ssrc = value(audio, "ssrc").unwrap().split(' ').next().unwrap();
    assert_eq!(value(audio, "x-ssrc-range"), Some(format!("{ssrc}-{ssrc}").as_str()));
    Ok(())
}

#[test]
fn the_media_controllers_answer_reads_back_for_str0m() -> TestResult {
    let mut rtc = relayed_peer()?;
    let (mid, offer, pending) = audio_offer(&mut rtc)?;
    let mut session = NgcSession::new();
    session.write_offer(&offer).unwrap();

    let answer = session.read_answer(&captured("controller_answer")?).unwrap();

    let text = answer.to_sdp_string();
    let parts = sections(&text);
    assert_eq!(
        parts.len(),
        2,
        "only the line we offered; the other twelve dropped:\n{text}"
    );
    let audio = &parts[1];
    // str0m writes its own placeholder port back: the address is in the candidates
    assert!(
        audio[0].starts_with("m=audio 9 UDP/TLS/RTP/SAVPF 120 111 "),
        "{}",
        audio[0]
    );
    assert_eq!(value(audio, "mid"), Some(mid.to_string().as_str()), "str0m's own mid");
    assert!(
        audio.contains(&"a=ssrc:1000 cname:teams-ngc"),
        "the stream x-ssrc-range names"
    );
    assert!(audio.contains(&"a=extmap:2 http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time"));
    let candidates: Vec<&str> = audio
        .iter()
        .filter(|line| line.starts_with("a=candidate:"))
        .copied()
        .collect();
    assert_eq!(
        candidates.len(),
        2,
        "the UDP relay candidates, IPv4 and IPv6; TCP left out: {candidates:#?}"
    );
    assert!(
        candidates
            .iter()
            .all(|line| line.contains(" udp ") && line.contains(" typ relay"))
    );
    for teams in ["MTURNID", "a=x-", "a=label", "a=rtcp:", "b=CT", "RTP/SAVP "] {
        assert!(!text.contains(teams), "{teams} left in:\n{text}");
    }

    rtc.sdp_api().accept_answer(pending, answer).unwrap();
    Ok(())
}

#[test]
fn an_answer_with_a_data_line_we_never_offered_drops_it() -> TestResult {
    // the media controller's answer cut to its audio and data lines, to an audio-only offer
    let full = sections(captured_text("controller_answer")?);
    let session_part: Vec<&str> = full[0]
        .iter()
        .map(|line| {
            if line.starts_with("a=group:") {
                "a=group:BUNDLE 0 12"
            } else {
                line
            }
        })
        .collect();
    let blob = [session_part, full[1].clone(), full[13].clone()].concat().join("\r\n") + "\r\n";
    let answer = NgcSdp::new(SDP_NGC, &blob).unwrap();
    let mut rtc = relayed_peer()?;
    let (_, offer, pending) = audio_offer(&mut rtc)?;
    let mut session = NgcSession::new();
    session.write_offer(&offer).unwrap();

    let read = session.read_answer(&answer).unwrap();

    let text = read.to_sdp_string();
    assert!(!text.contains("m=application"), "{text}");
    rtc.sdp_api().accept_answer(pending, read).unwrap();
    Ok(())
}

/// Our offer in the web client's 1:1 shape: audio, the main video line receiving, the sharing line inactive, data.
fn one_to_one_offer(rtc: &mut Rtc) -> TestResult<(Vec<Mid>, SdpOffer, SdpPendingOffer)> {
    let mut change = rtc.sdp_api();
    let mids = vec![
        change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None),
        change.add_media(MediaKind::Video, Direction::RecvOnly, None, None, None),
        change.add_media(MediaKind::Video, Direction::Inactive, None, None, None),
    ];
    change.add_channel("data".to_owned());
    let (offer, pending) = change.apply().ok_or("no offer")?;
    Ok((mids, offer, pending))
}

#[test]
fn a_callees_answer_in_a_one_to_one_call_reads_back() -> TestResult {
    let mut rtc = relayed_peer()?;
    let (mids, offer, pending) = one_to_one_offer(&mut rtc)?;
    let mut session = NgcSession::new();
    let ngc = session.write_offer(&offer).unwrap();
    let ours = sections(ngc.as_str());
    assert_eq!(value(&ours[0], "group"), Some("BUNDLE 0 1 2 3"));
    let data = &ours[4];
    assert_eq!(
        data[0], "m=x-data 50000 RTP/SAVP 127 126",
        "the data channel as Teams writes it"
    );
    assert_eq!(
        shape(data),
        shape(&sections(captured_text("start_offer")?)[4]),
        "the data line"
    );
    assert_eq!(value(data, "sctp-port"), Some("5000"));
    assert_eq!(
        labels(&ours),
        labels(&sections(captured_text("start_offer")?)),
        "the web client's labels: the second video line is the sharing one"
    );

    let answer = session.read_answer(&captured("callee_answer")?).unwrap();

    let text = answer.to_sdp_string();
    let parts = sections(&text);
    assert_eq!(parts.len(), 5, "{text}");
    assert_eq!(
        value(&parts[3], "mid"),
        Some(mids[2].to_string().as_str()),
        "the rejected line, by its place"
    );
    assert!(parts[3][0].starts_with("m=video 0 "), "{}", parts[3][0]);
    assert!(parts[4][0].starts_with("m=application "), "{}", parts[4][0]);
    assert_eq!(value(&parts[4], "sctp-port"), Some("5000"));
    rtc.sdp_api().accept_answer(pending, answer).unwrap();
    Ok(())
}

// ---- the callee's side ----

#[test]
fn an_invitations_offer_is_answered_in_teams_dialect() -> TestResult {
    let mut rtc = relayed_peer()?;
    let mut session = NgcSession::new();

    let offer = session.read_offer(&captured("invitation_offer")?).unwrap();
    let read = offer.to_sdp_string();
    assert!(
        !read
            .lines()
            .any(|line| line.starts_with("a=candidate:") && line.contains(" tcp")),
        "the caller's TCP candidates are out of the relay's reach:\n{read}"
    );
    assert!(read.contains(" typ relay "), "{read}");
    assert!(
        read.contains("m=application 9 UDP/DTLS/SCTP webrtc-datachannel"),
        "{read}"
    );
    let answer = rtc.sdp_api().accept_offer(offer).unwrap();
    let ngc = session.write_answer(&answer).unwrap();

    let ours = sections(ngc.as_str());
    let web = sections(captured_text("accept_answer")?);
    assert_eq!(
        shape(&ours[1]),
        shape(&web[1]),
        "the audio line, as the web client's accept carries it"
    );
    assert_eq!(ours.len(), web.len(), "every offered line answered:\n{}", ngc.as_str());
    assert_eq!(
        labels(&ours),
        [
            Some("main-audio"),
            Some("main-video"),
            Some("applicationsharing-video"),
            Some("data")
        ],
        "the offer's labels"
    );
    assert_eq!(value(&ours[1], "mid"), Some("0"), "the offer's mids");
    assert_eq!(value(&ours[1], "rtcp"), None, "an answer has no a=rtcp");
    assert_eq!(
        ours[1][0].split(' ').nth(1),
        Some("50000"),
        "our relayed candidate's port"
    );
    assert!(
        ours[4][0].starts_with("m=x-data 50000 RTP/SAVP 127 126"),
        "{}",
        ours[4][0]
    );
    assert_eq!(
        value(&ours[1], "setup"),
        value(&web[1], "setup"),
        "the callee takes the DTLS client's part"
    );
    Ok(())
}

#[test]
fn a_mixers_offer_in_its_older_dialect_is_read_and_answered() -> TestResult {
    let mixer = captured("mixer_offer")?;
    assert_eq!(mixer.dialect(), Dialect::Mixer);
    assert_eq!(mixer.content_type(), SDP_NGC_MIXER);
    let mut rtc = relayed_peer()?;
    let mut session = NgcSession::new();

    let offer = session.read_offer(&mixer).unwrap();

    let read = offer.to_sdp_string();
    for gone in ["a=crypto", "a=cryptoscale", "RTP/AVP", "MTURNID", "a=x-", "x-message"] {
        assert!(!read.contains(gone), "{gone} left in:\n{read}");
    }
    let parts = sections(&read);
    assert_eq!(parts.len(), 14, "audio, twelve video lines and data");
    for section in &parts[1..] {
        assert!(
            value(section, "ice-ufrag").is_some(),
            "every line of the mixer's carries its ICE credentials, kept: {section:#?}"
        );
        assert_eq!(
            value(section, "setup"),
            Some("passive"),
            "the mixer's offer names no setup: we take the client's part"
        );
    }
    assert!(
        parts[2].contains(&"a=rtcp-fb:107 nack pli"),
        "a=rtcp-fb:* written per payload type"
    );
    assert!(parts[13][0].starts_with("m=application "), "{}", parts[13][0]);
    assert_eq!(value(&parts[13], "sctp-port"), Some("5000"));
    let answer = rtc.sdp_api().accept_offer(offer).unwrap();
    let ngc = session.write_answer(&answer).unwrap();
    assert_eq!(
        ngc.content_type(),
        SDP_NGC,
        "we answer in the current dialect, as the web client does"
    );
    assert_eq!(sections(ngc.as_str()).len(), 14);
    Ok(())
}

// ---- renegotiations ----

#[test]
fn a_renegotiation_offered_to_the_callee_names_the_same_lines() -> TestResult {
    let mut rtc = relayed_peer()?;
    let mut session = NgcSession::new();
    let offer = session.read_offer(&captured("invitation_offer")?).unwrap();
    let answer = rtc.sdp_api().accept_offer(offer).unwrap();
    session.write_answer(&answer).unwrap();

    let renegotiation = session.read_offer(&captured("renegotiation_offer")?).unwrap();
    let answer = rtc.sdp_api().accept_offer(renegotiation).unwrap();
    let ngc = session.write_answer(&answer).unwrap();

    let ours = sections(ngc.as_str());
    assert_eq!(value(&ours[0], "group"), Some("BUNDLE 0 1 3"), "{}", ngc.as_str());
    assert_eq!(value(&ours[2], "mid"), Some("1"));
    Ok(())
}

#[test]
fn a_renegotiation_offered_to_the_caller_maps_back_to_its_own_mids() -> TestResult {
    let mut rtc = relayed_peer()?;
    let (mids, offer, pending) = one_to_one_offer(&mut rtc)?;
    let mut session = NgcSession::new();
    session.write_offer(&offer).unwrap();
    rtc.sdp_api()
        .accept_answer(pending, session.read_answer(&captured("callee_answer")?).unwrap())
        .unwrap();

    let renegotiation = session.read_offer(&captured("callee_renegotiation_offer")?).unwrap();

    let text = renegotiation.to_sdp_string();
    let parts = sections(&text);
    assert_eq!(value(&parts[2], "mid"), Some(mids[1].to_string().as_str()), "{text}");
    let answer = rtc.sdp_api().accept_offer(renegotiation).unwrap();
    let ngc = session.write_answer(&answer).unwrap();
    assert_eq!(value(&sections(ngc.as_str())[2], "mid"), Some("1"));
    Ok(())
}

#[test]
fn a_renegotiation_offered_by_us_keeps_the_numbering() -> TestResult {
    let mut rtc = relayed_peer()?;
    let (mids, offer, pending) = one_to_one_offer(&mut rtc)?;
    let mut session = NgcSession::new();
    session.write_offer(&offer).unwrap();
    rtc.sdp_api()
        .accept_answer(pending, session.read_answer(&captured("callee_answer")?).unwrap())
        .unwrap();

    // the camera comes on: the main video line sends too
    let mut change = rtc.sdp_api();
    change.set_direction(mids[1], Direction::SendRecv);
    let (offer, pending) = change.apply().unwrap();
    let ngc = session.write_offer(&offer).unwrap();

    let ours = sections(ngc.as_str());
    let video = &ours[2];
    assert_eq!(value(video, "mid"), Some("1"));
    assert!(video.contains(&"a=sendrecv"), "{video:#?}");
    assert!(
        value(video, "x-ssrc-range").is_some(),
        "the camera's stream named as Teams names it"
    );
    let answer = session.read_answer(&captured("caller_renegotiation_answer")?).unwrap();
    rtc.sdp_api().accept_answer(pending, answer).unwrap();
    Ok(())
}

#[test]
fn the_mixers_renegotiation_is_read_and_answered() -> TestResult {
    let mut rtc = relayed_peer()?;
    let mut session = NgcSession::new();
    let offer = session.read_offer(&captured("mixer_offer")?).unwrap();
    let answer = rtc.sdp_api().accept_offer(offer).unwrap();
    session.write_answer(&answer).unwrap();

    let renegotiation = session.read_offer(&captured("mixer_renegotiation_offer")?).unwrap();
    let answer = rtc.sdp_api().accept_offer(renegotiation).unwrap();
    let ngc = session.write_answer(&answer).unwrap();

    assert_eq!(sections(ngc.as_str()).len(), 14);
    Ok(())
}

#[test]
fn a_line_we_add_in_a_group_call_is_numbered_past_the_mixers_mids() -> TestResult {
    let mut rtc = relayed_peer()?;
    let mut session = mixer_session(&mut rtc)?;

    // our camera comes on as a line of its own
    let mut change = rtc.sdp_api();
    let camera = change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
    let (offer, pending) = change.apply().ok_or("no offer")?;
    let ngc = session.write_offer(&offer).unwrap();

    let ours = sections(ngc.as_str());
    let named = mids(&ours);
    assert_eq!(named.len(), 14, "{}", ngc.as_str());
    assert_eq!(named[13], Some("14"), "one past the mixer's highest mid");
    for (place, mid) in named.iter().enumerate() {
        assert_eq!(
            named.iter().filter(|other| *other == mid).count(),
            1,
            "{mid:?} at {place} named once"
        );
    }
    let bundle: Vec<&str> = value(&ours[0], "group")
        .and_then(|group| group.strip_prefix("BUNDLE "))
        .ok_or("no bundle")?
        .split(' ')
        .collect();
    assert_eq!(bundle.len(), 14);
    assert_eq!(bundle.iter().filter(|mid| **mid == "14").count(), 1);
    assert_eq!(bundle.iter().filter(|mid| **mid == "13").count(), 1);
    assert_eq!(value(&ours[14], "label"), Some("main-video"));

    // the mixer's answer to it, made from its later lines in `sdp-ngc-1.0` (its own ICE credentials and
    // `a=setup:passive`), with one more for the camera, which it receives
    let mut answer = sections(captured_text("mixer_renegotiation_offer")?);
    for line in &mut answer[0] {
        if line.starts_with("a=group:BUNDLE ") {
            *line = "a=group:BUNDLE 1 2 5 6 7 8 9 10 11 12 13 3 4 14";
        }
    }
    let camera_answer: Vec<&str> = answer[2]
        .iter()
        .map(|line| match *line {
            "a=mid:2" => "a=mid:14",
            "a=sendrecv" | "a=sendonly" | "a=inactive" => "a=recvonly",
            other => other,
        })
        .collect();
    answer.push(camera_answer);
    let read = session.read_answer(&NgcSdp::new(SDP_NGC, &joined(&answer))?).unwrap();

    let text = read.to_sdp_string();
    let parts = sections(&text);
    assert_eq!(parts.len(), 15, "{text}");
    assert_eq!(
        value(&parts[14], "mid"),
        Some(camera.to_string().as_str()),
        "the camera's answer goes to the camera's line"
    );
    assert_eq!(value(&parts[2], "mid"), Some("2"), "the mixer's line stays its own");
    rtc.sdp_api().accept_answer(pending, read).unwrap();
    Ok(())
}

#[test]
fn a_rejected_line_without_a_mid_in_a_mixers_renegotiation_is_the_line_at_its_place() -> TestResult {
    let mut rtc = relayed_peer()?;
    let mut session = mixer_session(&mut rtc)?;

    // the mixer's renegotiation with its sharing line (mid 3, at place 11) rejected, named only by its label
    let mut parts = sections(captured_text("mixer_renegotiation_offer")?);
    for line in &mut parts[0] {
        if line.starts_with("a=group:BUNDLE ") {
            *line = "a=group:BUNDLE 1 2 5 6 7 8 9 10 11 12 13 4";
        }
    }
    assert_eq!(value(&parts[12], "mid"), Some("3"));
    parts[12] = vec!["m=video 0 RTP/SAVP 34", "a=label:applicationsharing-video"];
    let renegotiation = NgcSdp::new(SDP_NGC, &joined(&parts))?;

    let offer = session.read_offer(&renegotiation).unwrap();

    let text = offer.to_sdp_string();
    let read = sections(&text);
    assert_eq!(value(&read[12], "mid"), Some("3"), "the sharing line's mid:\n{text}");
    assert!(read[12][0].starts_with("m=video 0 "), "{}", read[12][0]);
    let named = mids(&read);
    assert_eq!(
        named.iter().filter(|mid| **mid == Some("11")).count(),
        1,
        "the camera line keeps mid 11 to itself"
    );
    let answer = rtc.sdp_api().accept_offer(offer).unwrap();
    let ngc = session.write_answer(&answer).unwrap();
    assert_eq!(sections(ngc.as_str()).len(), 14);
    Ok(())
}

// ---- what is refused ----

#[test]
fn a_browsers_sdp_is_refused_where_teams_dialect_is_required() -> TestResult {
    for browser in [captured_text("browser_offer")?, captured_text("browser_answer")?] {
        assert!(
            matches!(NgcSdp::new(SDP_NGC, browser), Err(SdpError::NotNgc(_))),
            "a browser's SDP as it is"
        );
    }
    let mut rtc = relayed_peer()?;
    let (_, offer, _) = audio_offer(&mut rtc)?;
    assert!(
        NgcSdp::new(SDP_NGC, &offer.to_sdp_string()).is_err(),
        "str0m's as it is"
    );
    assert!(
        NgcSdp::new(SDP_NGC, "m=audio 50000 RTP/SAVP 111\r\n").is_err(),
        "no session part"
    );
    assert!(NgcSdp::new(SDP_NGC, "v=0\r\ns=-\r\n").is_err(), "no media line");
    assert!(
        NgcSdp::new(SDP_NGC, "v=0\r\ns=-\r\nm=audio 9 RTP/SAVP 111\r\n").is_err(),
        "the browser's placeholder port"
    );
    assert!(
        NgcSdp::new(SDP_NGC, captured_text("mixer_offer")?).is_err(),
        "the mixer's RTP/AVP outside its dialect"
    );
    assert!(matches!(
        NgcSdp::new("application/sdp", captured_text("start_offer")?),
        Err(SdpError::ContentType(_))
    ));
    for name in [
        "start_offer",
        "callee_answer",
        "invitation_offer",
        "accept_answer",
        "controller_answer",
    ] {
        assert_eq!(captured(name)?.dialect(), Dialect::Ngc, "{name}");
    }
    Ok(())
}

#[test]
fn a_local_address_never_goes_out() -> TestResult {
    let mut rtc = Rtc::builder()
        .set_crypto_provider(Arc::new(str0m::crypto::from_feature_flags()))
        .build(Instant::now());
    rtc.add_local_candidate(Candidate::host("192.0.2.60:40000".parse().unwrap(), Protocol::Udp).unwrap());
    let (_, offer, _) = audio_offer(&mut rtc)?;

    let error = NgcSession::new().write_offer(&offer).unwrap_err();

    assert_eq!(error, SdpError::LocalCandidate("host".to_owned()));
    Ok(())
}

#[test]
fn an_offer_without_a_relayed_candidate_is_unwritable() -> TestResult {
    let mut rtc = Rtc::builder()
        .set_crypto_provider(Arc::new(str0m::crypto::from_feature_flags()))
        .build(Instant::now());
    let (_, offer, _) = audio_offer(&mut rtc)?;

    let error = NgcSession::new().write_offer(&offer).unwrap_err();

    assert!(matches!(error, SdpError::Unwritable(_)), "{error}");
    Ok(())
}

#[test]
fn an_answer_to_an_offer_we_never_made_is_refused() -> TestResult {
    let session = NgcSession::new();
    let error = session.read_answer(&captured("controller_answer")?).unwrap_err();
    assert!(matches!(error, SdpError::Mismatch(_)), "{error}");
    Ok(())
}

#[test]
fn the_sdp_never_shows_in_debug() -> TestResult {
    let debug = format!("{:?}", captured("start_offer")?);
    assert!(!debug.contains("ice-pwd") && !debug.contains("192.0.2"), "{debug}");
    Ok(())
}

#[test]
fn every_captured_answer_reads_as_an_answer() -> TestResult {
    // `SdpAnswer` and `SdpOffer` are str0m's parse: the conversion of each captured form must be one it takes
    let mut rtc = relayed_peer()?;
    let (_, offer, _) = one_to_one_offer(&mut rtc)?;
    let mut session = NgcSession::new();
    session.write_offer(&offer).unwrap();
    for name in ["callee_answer", "caller_renegotiation_answer", "renegotiation_answer"] {
        let answer: SdpAnswer = session.read_answer(&captured(name)?).unwrap();
        assert!(answer.to_sdp_string().starts_with("v=0"), "{name}");
    }
    Ok(())
}
