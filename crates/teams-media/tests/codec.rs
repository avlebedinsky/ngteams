//! Opus as the calls configure it: the `a=fmtp` parameters the captures show, 10 ms device frames into 20 ms packets,
//! the tone through a round trip, a lost packet recovered from the next one's in-band FEC (and only concealed without
//! it), and DTX sending nothing in silence.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use teams_media::codec::{
    DEFAULT_EXPECTED_LOSS, Decoder, Encoder, OpusConfig, PACKET_SAMPLES, PACKET_TICKS, Packet, packet_samples,
};
use teams_media::devices::{AudioFrame, FRAME_SAMPLES};
use teams_media::tone::{Tone, ToneGenerator, amplitude_at};

const TONE: Tone = Tone {
    hz: 440.0,
    amplitude: 0.5,
};

/// `count` device frames of `tone` (or of silence for `None`), continuing one generator.
fn frames(tone: Option<Tone>, count: usize) -> Vec<AudioFrame> {
    let mut generator = tone.map(ToneGenerator::new);
    (0..count)
        .map(|_| {
            let mut frame = AudioFrame::silence();
            if let Some(generator) = &mut generator {
                generator.fill(frame.samples_mut());
            }
            frame
        })
        .collect()
}

fn encode(config: OpusConfig, frames: &[AudioFrame]) -> Vec<Packet> {
    let mut encoder = Encoder::new(&config).unwrap();
    frames.iter().filter_map(|frame| encoder.push(frame).unwrap()).collect()
}

#[test]
fn the_captured_fmtp_lines_configure_fec_and_dtx() {
    // our offer's and the web client's line, and the media controller's answer
    let offer = OpusConfig::from_fmtp("minptime=10;useinbandfec=1");
    assert!(offer.inband_fec && !offer.dtx);
    assert_eq!(
        offer,
        OpusConfig::default(),
        "our own offer asks for what we run by default"
    );
    let answer = OpusConfig::from_fmtp("minptime=10;usedtx=1;useinbandfec=1");
    assert!(answer.inband_fec && answer.dtx);
    assert_eq!(answer.expected_loss, DEFAULT_EXPECTED_LOSS);
    // RFC 7587's defaults: a flag absent or not `1` is off
    let bare = OpusConfig::from_fmtp("");
    assert!(!bare.inband_fec && !bare.dtx);
    let off = OpusConfig::from_fmtp("useinbandfec=0; usedtx = 2");
    assert!(!off.inband_fec && !off.dtx);
    let spaced = OpusConfig::from_fmtp(" UseInbandFec = 1 ;minptime=10");
    assert!(spaced.inband_fec);
}

#[test]
fn the_audio_lines_opus_fmtp_is_read_from_an_sdp() {
    let sdp = "v=0\r\n\
        m=audio 9 UDP/TLS/RTP/SAVPF 111 9\r\n\
        a=mid:0\r\n\
        a=rtpmap:9 G722/8000\r\n\
        a=fmtp:9 usedtx=1\r\n\
        a=rtpmap:111 OPUS/48000/2\r\n\
        a=fmtp:111 minptime=10;usedtx=1;useinbandfec=1\r\n\
        m=video 9 UDP/TLS/RTP/SAVPF 96\r\n\
        a=mid:1\r\n\
        a=rtpmap:96 opus/48000/2\r\n\
        m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
        a=mid:2\r\n\
        a=rtpmap:111 opus/48000/2\r\n\
        m=audio 9 UDP/TLS/RTP/SAVPF 0\r\n\
        a=mid:3\r\n\
        a=rtpmap:0 PCMU/8000\r\n";
    let first = OpusConfig::from_sdp(sdp, "0").unwrap();
    assert!(first.inband_fec && first.dtx, "the Opus payload's fmtp, any case");
    assert_eq!(OpusConfig::from_sdp(sdp, "1"), None, "not an audio line");
    let bare = OpusConfig::from_sdp(sdp, "2").unwrap();
    assert!(!bare.inband_fec && !bare.dtx, "no fmtp: RFC 7587's defaults");
    assert_eq!(OpusConfig::from_sdp(sdp, "3"), None, "no Opus");
    assert_eq!(OpusConfig::from_sdp(sdp, "4"), None, "no such line");
}

#[test]
fn two_device_frames_make_one_packet_and_the_media_time_counts_packets() {
    let mut encoder = Encoder::new(&OpusConfig::default()).unwrap();
    let input = frames(Some(TONE), 6);
    let made: Vec<Option<Packet>> = input.iter().map(|frame| encoder.push(frame).unwrap()).collect();
    let times: Vec<Option<u64>> = made.iter().map(|packet| packet.as_ref().map(|p| p.rtp_time)).collect();
    assert_eq!(
        times,
        [None, Some(0), None, Some(PACKET_TICKS), None, Some(2 * PACKET_TICKS)]
    );
    for packet in made.into_iter().flatten() {
        assert_eq!(packet_samples(&packet.payload), Some(PACKET_SAMPLES), "20 ms each");
    }
    assert_eq!(PACKET_SAMPLES, 2 * FRAME_SAMPLES);
}

#[test]
fn the_tone_survives_a_round_trip() {
    let packets = encode(OpusConfig::default(), &frames(Some(TONE), 100));
    let mut decoder = Decoder::new().unwrap();
    let mut pcm = Vec::new();
    for packet in &packets {
        assert_eq!(decoder.decode(&packet.payload, &mut pcm).unwrap(), PACKET_SAMPLES);
    }
    // past the codec's warm-up, over whole periods of the tone's 440 Hz (12 000 samples hold 110)
    let steady = &pcm[pcm.len() - 12_000..];
    let level = amplitude_at(steady, TONE.hz);
    assert!((0.4..=0.6).contains(&level), "the tone's level after Opus: {level}");
    assert!(amplitude_at(steady, 1_000.0) < 0.02, "nothing else");
}

/// Decodes `packets` with the one at `lost` missing, filling it as `fill` says; returns the samples of the lost
/// packet's place.
fn decode_with_loss(
    packets: &[Packet],
    lost: usize,
    fill: impl Fn(&mut Decoder, &[u8], &mut Vec<f32>) -> usize,
) -> Vec<f32> {
    let mut decoder = Decoder::new().unwrap();
    let mut pcm = Vec::new();
    let mut place = Vec::new();
    for (index, packet) in packets.iter().enumerate() {
        if index == lost {
            let before = pcm.len();
            assert_eq!(
                fill(&mut decoder, &packets[index + 1].payload, &mut pcm),
                PACKET_SAMPLES
            );
            place = pcm[before..].to_vec();
        } else {
            decoder.decode(&packet.payload, &mut pcm).unwrap();
        }
    }
    place
}

#[test]
fn a_lost_packet_comes_back_from_the_next_ones_fec_and_is_only_concealed_without_it() {
    // silence, then the tone starts in the packet that is lost: concealment can only carry the silence on, while the
    // next packet's FEC holds a copy of the onset
    let mut input = frames(None, 40);
    input.extend(frames(Some(TONE), 40));
    let config = OpusConfig {
        expected_loss: 20,
        ..OpusConfig::default()
    };
    let packets = encode(config, &input);
    let lost = 20;
    let recovered = decode_with_loss(&packets, lost, |decoder, next, pcm| {
        decoder.recover(next, PACKET_SAMPLES, pcm).unwrap()
    });
    let concealed = decode_with_loss(&packets, lost, |decoder, _, pcm| {
        decoder.conceal(PACKET_SAMPLES, pcm).unwrap()
    });
    let (recovered, concealed) = (amplitude_at(&recovered, TONE.hz), amplitude_at(&concealed, TONE.hz));
    assert!(recovered > 0.15, "the onset recovered from FEC: {recovered}");
    assert!(concealed < 0.05, "concealment knows nothing of the onset: {concealed}");

    // without FEC the next packet carries no copy: "recovering" is concealment
    let plain = encode(
        OpusConfig {
            inband_fec: false,
            ..config
        },
        &input,
    );
    let without = decode_with_loss(&plain, lost, |decoder, next, pcm| {
        decoder.recover(next, PACKET_SAMPLES, pcm).unwrap()
    });
    let without = amplitude_at(&without, TONE.hz);
    assert!(without < 0.05, "no FEC, nothing to recover: {without}");
}

#[test]
fn concealment_continues_a_tone_through_a_gap() {
    let packets = encode(OpusConfig::default(), &frames(Some(TONE), 40));
    let place = decode_with_loss(&packets, 15, |decoder, _, pcm| {
        decoder.conceal(PACKET_SAMPLES, pcm).unwrap()
    });
    let level = amplitude_at(&place, TONE.hz);
    assert!(level > 0.1, "the gap is filled with the tone, not silence: {level}");
}

#[test]
fn dtx_sends_nothing_in_silence_but_keeps_the_media_time() {
    // two seconds of silence: 100 packets' worth
    let input = frames(None, 200);
    let with = encode(
        OpusConfig {
            dtx: true,
            ..OpusConfig::default()
        },
        &input,
    );
    let without = encode(OpusConfig::default(), &input);
    assert_eq!(without.len(), 100, "every packet goes without DTX");
    // DTX starts after a moment of silence and then sends only a keep-alive now and then (libopus: one in 400 ms)
    let last_second = with
        .iter()
        .filter(|packet| packet.rtp_time >= 50 * PACKET_TICKS)
        .count();
    assert!(
        last_second <= 4,
        "DTX sends {last_second} of 50 packets in a second of silence"
    );
    assert!(with.len() < 25, "DTX sends {} of 100 packets in silence", with.len());
    assert!(
        with.iter()
            .all(|packet| packet.rtp_time % PACKET_TICKS == 0 && packet.rtp_time < 100 * PACKET_TICKS),
        "the packets sent keep their places in media time"
    );
}

#[test]
fn what_is_not_opus_is_refused() {
    let mut decoder = Decoder::new().unwrap();
    let mut pcm = Vec::new();
    assert!(decoder.decode(&[], &mut pcm).is_err(), "an empty packet");
    assert_eq!(packet_samples(&[]), None);
    // a code-3 packet that claims more frames than it holds
    assert_eq!(packet_samples(&[0x03, 0xFF]), None);
    assert!(decoder.conceal(100, &mut pcm).is_err(), "no Opus frame size");
    assert!(decoder.conceal(0, &mut pcm).is_err());
    assert!(pcm.is_empty(), "nothing was appended");
}
