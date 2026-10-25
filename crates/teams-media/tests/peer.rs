//! The sans-IO peer on a virtual clock: two `Peer`s wired back to back with no sockets, so a test can cut one
//! direction of the link, advance time by minutes in microseconds, reorder frames and feed junk; and a plain `str0m`
//! caller offering audio and video lines, as Teams does, to a peer that answers.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use str0m::change::SdpOffer;
use str0m::format::{Codec, PayloadParams};
use str0m::media::{Direction, Frequency, MediaData, MediaKind, MediaTime, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, Input, Output, Rtc};
use teams_media::Error;
use teams_media::peer::{Peer, PeerEvent};

/// One virtual run-loop step.
const STEP: Duration = Duration::from_millis(10);
/// Longest any phase may take, in virtual time.
const PHASE_LIMIT: Duration = Duration::from_secs(120);
/// A payload str0m carries as is (an Opus silence frame).
const FRAME: [u8; 3] = [0xF8, 0xFF, 0xFE];

/// Two peers whose datagrams are carried by the test; B's are dropped while `deaf_a`.
struct Link {
    a: Peer,
    b: Peer,
    now: Instant,
    deaf_a: bool,
    a_events: Vec<PeerEvent>,
    b_events: Vec<PeerEvent>,
}

/// A peer whose one candidate is the host address `127.0.0.1:<port>` (the far end's stand-in; a call's own peer is
/// relayed).
fn host_peer(now: Instant, port: u16) -> Result<Peer, Error> {
    let candidate = Candidate::host(SocketAddr::from(([127, 0, 0, 1], port)), Protocol::Udp)
        .map_err(|e| Error::Rtc(e.to_string()))?;
    Peer::with_candidate(now, candidate)
}

impl Link {
    fn connected() -> Result<Self, Error> {
        let now = Instant::now();
        let mut a = host_peer(now, 10_000)?;
        let mut b = host_peer(now, 20_000)?;
        let (offer, pending) = a.offer_audio()?;
        let answer = b.accept_offer(offer)?;
        a.accept_answer(pending, answer)?;
        let mut link = Self {
            a,
            b,
            now,
            deaf_a: false,
            a_events: Vec::new(),
            b_events: Vec::new(),
        };
        link.run_until("connect", |l| l.a.is_connected() && l.b.is_connected())?;
        assert_eq!(link.a_events, [PeerEvent::Connected]);
        assert_eq!(link.b_events, [PeerEvent::Connected]);
        link.a_events.clear();
        link.b_events.clear();
        Ok(link)
    }

    /// Carries every queued datagram (B's only while A is not deaf), fires due timeouts, collects events, then
    /// advances the clock.
    fn step(&mut self) -> Result<(), Error> {
        while let Some(d) = self.a.poll_datagram() {
            self.b
                .handle_datagram(self.now, d.proto, d.source, d.destination, &d.contents)?;
        }
        while let Some(d) = self.b.poll_datagram() {
            if !self.deaf_a {
                self.a
                    .handle_datagram(self.now, d.proto, d.source, d.destination, &d.contents)?;
            }
        }
        for peer in [&mut self.a, &mut self.b] {
            if self.now >= peer.deadline() {
                peer.handle_timeout(self.now)?;
            }
        }
        self.a_events.extend(std::iter::from_fn(|| self.a.poll_event()));
        self.b_events.extend(std::iter::from_fn(|| self.b.poll_event()));
        self.now += STEP;
        Ok(())
    }

    fn run_until(&mut self, what: &str, done: impl Fn(&Self) -> bool) -> Result<(), Error> {
        let limit = self.now + PHASE_LIMIT;
        while !done(self) {
            assert!(
                self.now < limit,
                "{what}: no progress within {PHASE_LIMIT:?} of virtual time; A {:?}, B {:?}",
                self.a_events,
                self.b_events
            );
            self.step()?;
        }
        Ok(())
    }

    /// Writes one frame from A and returns the payloads B received within a few steps.
    fn send_a_to_b(&mut self, payload: &[u8]) -> Result<Vec<Vec<u8>>, Error> {
        self.a.write_audio(self.now, self.now, 0, payload)?;
        for _ in 0..5 {
            self.step()?;
        }
        Ok(self
            .b_events
            .drain(..)
            .filter_map(|event| match event {
                PeerEvent::Audio(frame) => Some(frame.payload.to_vec()),
                _ => None,
            })
            .collect())
    }
}

#[test]
fn ice_recovers_after_a_transient_disconnect() {
    // A hears nothing until its ICE pair times out. B still hears A, so B keeps its pair and its checks, which is what
    // lets A rebuild the pair once the gap ends (str0m prunes a pair both sides lost, and only an ICE restart helps).
    let mut link = Link::connected().unwrap();
    link.deaf_a = true;
    link.run_until("ICE disconnect", |l| l.a_events.contains(&PeerEvent::Disconnected))
        .unwrap();
    assert!(!link.a.is_connected());
    assert!(matches!(
        link.a.write_audio(link.now, link.now, 0, &FRAME),
        Err(Error::NotConnected)
    ));

    link.deaf_a = false;
    link.a_events.clear();
    link.b_events.clear();
    link.run_until("ICE recovery", |l| l.a.is_connected() && l.b.is_connected())
        .unwrap();
    assert_eq!(link.a_events, [PeerEvent::Connected], "A reports the recovery");
    let received = link.send_a_to_b(&FRAME).unwrap();
    assert_eq!(received, [FRAME.to_vec()], "media flows again after the recovery");
}

#[test]
fn a_remote_close_ends_the_session() {
    let mut link = Link::connected().unwrap();
    link.a.close().unwrap();
    assert!(!link.a.is_connected());
    link.run_until("remote close", |l| l.b_events.contains(&PeerEvent::Closed))
        .unwrap();
    assert!(!link.b.is_connected());
    assert!(matches!(
        link.b.write_audio(link.now, link.now, 0, &FRAME),
        Err(Error::NotConnected)
    ));
    assert!(
        !link.a_events.contains(&PeerEvent::Closed),
        "only the remote side reports Closed"
    );
}

#[test]
fn write_audio_queues_the_rtp_datagram_at_once() {
    let mut link = Link::connected().unwrap();
    while link.a.poll_datagram().is_some() {}
    link.a.write_audio(link.now, link.now, 0, &FRAME).unwrap();
    let rtp = std::iter::from_fn(|| link.a.poll_datagram())
        .any(|d| d.contents.first().is_some_and(|b| (128..=191).contains(b)));
    assert!(rtp, "the frame left without waiting for a timeout");
}

#[test]
fn unclassifiable_datagrams_are_dropped_and_counted() {
    let mut link = Link::connected().unwrap();
    let from = SocketAddr::from(([127, 0, 0, 1], 30_000));
    let to = SocketAddr::from(([127, 0, 0, 1], 10_000));
    let junk: [&[u8]; 4] = [
        &[],                          // empty
        &[0, 1, 0, 0],                // STUN range, shorter than a header
        &[0x40, 0x00, 0x00, 0x02, 1], // TURN ChannelData
        &[200; 8],                    // first byte above the RTP range
    ];
    for contents in junk {
        link.a
            .handle_datagram(link.now, Protocol::Udp, from, to, contents)
            .unwrap();
    }
    assert_eq!(link.a.dropped_datagrams(), 4);
    let received = link.send_a_to_b(&FRAME).unwrap();
    assert_eq!(received, [FRAME.to_vec()], "the session is unaffected");
}

#[test]
fn audio_frames_come_out_as_they_arrive_even_out_of_order() {
    // str0m holds nothing back for a missing packet: the frame after a gap, and the late one that fills it, each come
    // out the moment they arrive (the jitter buffer puts them back in order)
    let mut link = Link::connected().unwrap();
    for rtp_time in [0, 960, 1_920] {
        link.a.write_audio(link.now, link.now, rtp_time, &FRAME).unwrap();
    }
    let datagrams: Vec<_> = std::iter::from_fn(|| link.a.poll_datagram())
        .filter(|d| d.contents.first().is_some_and(|b| (128..=191).contains(b)))
        .collect();
    assert_eq!(datagrams.len(), 3, "one RTP datagram per frame");
    for index in [0, 2, 1] {
        let d = &datagrams[index];
        link.b
            .handle_datagram(link.now, d.proto, d.source, d.destination, &d.contents)
            .unwrap();
        link.b.handle_timeout(link.now).unwrap();
    }
    let times: Vec<u64> = std::iter::from_fn(|| link.b.poll_event())
        .filter_map(|event| match event {
            PeerEvent::Audio(frame) => Some(frame.rtp_time),
            _ => None,
        })
        .collect();
    let base = times[0];
    let offsets: Vec<u64> = times.iter().map(|time| time - base).collect();
    assert_eq!(offsets, [0, 1_920, 960], "in arrival order, none held back");
}

/// A plain `str0m` peer on a host candidate, offering what a Teams caller does: audio, then video lines.
struct Caller {
    rtc: Rtc,
    received: Vec<MediaData>,
}

impl Caller {
    fn new(now: Instant) -> Self {
        let mut rtc = Rtc::builder()
            .set_crypto_provider(Arc::new(str0m::crypto::from_feature_flags()))
            .build(now);
        let candidate = Candidate::host(SocketAddr::from(([127, 0, 0, 1], 20_000)), Protocol::Udp).unwrap();
        rtc.add_local_candidate(candidate);
        Self {
            rtc,
            received: Vec::new(),
        }
    }

    /// Drains `poll_output`, carrying its datagrams to `answerer` and keeping the media it received.
    fn drain(&mut self, now: Instant, answerer: &mut Peer) {
        loop {
            match self.rtc.poll_output().unwrap() {
                Output::Timeout(_) => return,
                Output::Transmit(t) => answerer
                    .handle_datagram(now, t.proto, t.source, t.destination, &t.contents)
                    .unwrap(),
                Output::Event(Event::MediaData(data)) => self.received.push(data),
                Output::Event(_) => {}
            }
        }
    }

    /// One virtual step: the answerer's datagrams in, both clocks advanced, the caller's datagrams out.
    fn step(&mut self, now: Instant, answerer: &mut Peer, answerer_events: &mut Vec<PeerEvent>) {
        while let Some(d) = answerer.poll_datagram() {
            let receive = Receive {
                proto: d.proto,
                source: d.source,
                destination: d.destination,
                contents: d.contents.as_slice().try_into().unwrap(),
            };
            self.rtc.handle_input(Input::Receive(now, receive)).unwrap();
        }
        self.rtc.handle_input(Input::Timeout(now)).unwrap();
        self.drain(now, answerer);
        if now >= answerer.deadline() {
            answerer.handle_timeout(now).unwrap();
        }
        answerer_events.extend(std::iter::from_fn(|| answerer.poll_event()));
    }

    /// Writes one frame of `codec` on the line `mid`.
    fn write(&mut self, now: Instant, mid: Mid, codec: Codec, time: MediaTime, payload: &[u8]) {
        let writer = self.rtc.writer(mid).unwrap();
        let pt = writer
            .payload_params()
            .find(|params| params.spec().codec == codec)
            .map(PayloadParams::pt)
            .unwrap();
        writer.write(pt, now, time, payload.to_vec()).unwrap();
    }
}

#[test]
fn answering_an_offer_with_video_lines_keeps_the_audio_line() {
    // a Teams offer carries audio first, then main video and sharing: the answerer's audio stays on the audio line, and
    // what arrives on a video line is no audio
    let mut now = Instant::now();
    let mut caller = Caller::new(now);
    let mut change = caller.rtc.sdp_api();
    let audio = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
    let video = change.add_media(MediaKind::Video, Direction::SendRecv, None, None, None);
    change.add_media(MediaKind::Video, Direction::RecvOnly, None, None, None);
    let (offer, pending) = change.apply().unwrap();
    let offer_text = offer
        .to_sdp_string()
        .replace("useinbandfec=1", "useinbandfec=1;usedtx=1");
    let mut answerer = host_peer(now, 10_000).unwrap();
    let answer = answerer
        .accept_offer(SdpOffer::from_sdp_string(&offer_text).unwrap())
        .unwrap();
    caller.rtc.sdp_api().accept_answer(pending, answer).unwrap();
    caller.drain(now, &mut answerer);
    let mut events = Vec::new();
    let limit = now + PHASE_LIMIT;
    while !(answerer.is_connected() && caller.rtc.is_connected()) {
        assert!(now < limit, "no connection: {events:?}");
        caller.step(now, &mut answerer, &mut events);
        now += STEP;
    }
    // str0m names a remote offer's lines once the media connects
    caller.step(now, &mut answerer, &mut events);
    assert_eq!(answerer.audio_mid(), Some(audio), "the offer's audio line");
    assert_eq!(
        answerer.remote_opus().map(|opus| (opus.inband_fec, opus.dtx)),
        Some((true, true)),
        "the caller's Opus line asks for FEC and DTX"
    );

    answerer.write_audio(now, now, 0, &FRAME).unwrap();
    let keyframe = [0x10, 0x02, 0x00, 0x9D, 0x01, 0x2A, 0x10, 0x00, 0x10, 0x00, 0, 0, 0, 0];
    for frame in 0..3_u64 {
        let time = MediaTime::new(frame * 3_000, Frequency::NINETY_KHZ);
        caller.write(now, video, Codec::Vp8, time, &keyframe);
    }
    let time = MediaTime::new(0, Frequency::FORTY_EIGHT_KHZ);
    caller.write(now, audio, Codec::Opus, time, &FRAME);
    for _ in 0..10 {
        caller.step(now, &mut answerer, &mut events);
        now += STEP;
    }
    assert!(
        caller
            .received
            .iter()
            .any(|data| data.mid == audio && data.data[..] == FRAME[..]),
        "the answerer's audio went out on the audio line: {:?}",
        caller.received.iter().map(|data| data.mid).collect::<Vec<_>>()
    );
    let heard: Vec<Vec<u8>> = events
        .iter()
        .filter_map(|event| match event {
            PeerEvent::Audio(audio) => Some(audio.payload.to_vec()),
            _ => None,
        })
        .collect();
    assert_eq!(heard, [FRAME.to_vec()], "only the audio line's frame is audio");
}

#[test]
fn an_answer_without_dtx_leaves_it_off() {
    let mut link = Link::connected().unwrap();
    let opus = link.a.remote_opus().unwrap();
    assert_eq!(
        (opus.inband_fec, opus.dtx),
        (true, false),
        "str0m's own answer: FEC, no DTX"
    );
    let opus = link.b.remote_opus().unwrap();
    assert_eq!((opus.inband_fec, opus.dtx), (true, false), "and its offer");
    link.step().unwrap();
}

#[test]
fn a_peer_with_a_relayed_candidate_sends_from_and_receives_at_the_allocation() {
    // The test plays the TURN server: whatever A sends leaves from the allocation, whatever B sends to the allocation
    // reaches A, and A's socket to the server (`local`) never shows up as a source or destination.
    let relayed = SocketAddr::from(([127, 0, 0, 1], 40_000));
    let local = SocketAddr::from(([127, 0, 0, 1], 10_000));
    let mut now = Instant::now();
    let candidate = str0m::Candidate::relayed(relayed, local, "udp").unwrap();
    let mut a = Peer::with_candidate(now, candidate).unwrap();
    let mut b = host_peer(now, 20_000).unwrap();
    let (offer, pending) = a.offer_audio().unwrap();
    assert!(
        offer.to_sdp_string().contains("typ relay"),
        "A offers its relayed candidate"
    );
    let answer = b.accept_offer(offer).unwrap();
    a.accept_answer(pending, answer).unwrap();
    let limit = now + PHASE_LIMIT;
    while !(a.is_connected() && b.is_connected()) {
        assert!(now < limit, "no connection through the carried relay");
        while let Some(d) = a.poll_datagram() {
            assert_eq!(d.source, relayed, "A sends from its allocation");
            b.handle_datagram(now, d.proto, d.source, d.destination, &d.contents)
                .unwrap();
        }
        while let Some(d) = b.poll_datagram() {
            assert_eq!(d.destination, relayed, "B only knows A's allocation");
            a.handle_datagram(now, d.proto, d.source, d.destination, &d.contents)
                .unwrap();
        }
        for peer in [&mut a, &mut b] {
            if now >= peer.deadline() {
                peer.handle_timeout(now).unwrap();
            }
        }
        now += STEP;
    }
}
