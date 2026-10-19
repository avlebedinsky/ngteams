//! Spike 3a: a tone survives the generator → Goertzel, an Opus round trip, and two str0m peers over loopback UDP.

use std::time::Duration;

use media_loop::codec::{OpusDecoder, OpusEncoder};
use media_loop::goertzel::{dominant_frequency, energy_fraction};
use media_loop::loopback::{LatencyStats, LoopConfig, run_loopback};
use media_loop::tone::ToneGenerator;
use media_loop::{Error, FRAME_SAMPLES, SAMPLE_RATE};

/// How far a detected frequency may sit from the sent one: one step of the detector's scan.
const TOLERANCE_HZ: f64 = 5.0;

fn tone(freq_hz: f64, samples: usize) -> Vec<i16> {
    let mut generator = ToneGenerator::new(freq_hz, SAMPLE_RATE, 0.5);
    let mut out = vec![0; samples];
    generator.fill(&mut out);
    out
}

fn assert_near(detected: Option<f64>, expected: f64) {
    assert!(
        detected.is_some_and(|hz| (hz - expected).abs() <= TOLERANCE_HZ),
        "detected {detected:?} Hz, expected {expected} Hz"
    );
}

#[test]
fn goertzel_detects_the_generated_frequency() {
    for freq in [300.0, 440.0, 697.0, 1000.0, 1633.0] {
        let samples = tone(freq, SAMPLE_RATE as usize / 2);
        assert_near(dominant_frequency(&samples, SAMPLE_RATE), freq);
        let fraction = energy_fraction(&samples, SAMPLE_RATE, freq);
        assert!(fraction > 0.95, "{freq} Hz carries {fraction} of the energy");
        assert!(energy_fraction(&samples, SAMPLE_RATE, freq + 200.0) < 0.01);
    }
}

#[test]
fn generator_keeps_phase_across_frames() {
    let whole = tone(440.0, FRAME_SAMPLES * 3);
    let mut generator = ToneGenerator::new(440.0, SAMPLE_RATE, 0.5);
    let mut framed = Vec::new();
    for _ in 0..3 {
        let mut frame = vec![0; FRAME_SAMPLES];
        generator.fill(&mut frame);
        framed.extend_from_slice(&frame);
    }
    assert_eq!(whole, framed);
}

#[test]
fn generator_amplitude_is_a_fraction_of_full_scale() {
    let samples = tone(440.0, SAMPLE_RATE as usize / 10);
    let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
    assert!((16_000..=16_384).contains(&peak), "peak {peak}");
    let loud = {
        let mut generator = ToneGenerator::new(440.0, SAMPLE_RATE, 7.0);
        let mut out = vec![0; FRAME_SAMPLES];
        generator.fill(&mut out);
        out
    };
    assert!(loud.contains(&i16::MAX), "an amplitude over 1 clips to full scale");
}

#[test]
fn goertzel_finds_nothing_in_silence_or_an_empty_buffer() {
    assert_eq!(dominant_frequency(&[0; FRAME_SAMPLES], SAMPLE_RATE), None);
    assert_eq!(dominant_frequency(&[], SAMPLE_RATE), None);
    assert!(energy_fraction(&[0; FRAME_SAMPLES], SAMPLE_RATE, 440.0).abs() < f64::EPSILON);
}

#[test]
fn opus_round_trip_keeps_the_tone() {
    let mut encoder = OpusEncoder::new().unwrap();
    let mut decoder = OpusDecoder::new().unwrap();
    let mut generator = ToneGenerator::new(440.0, SAMPLE_RATE, 0.5);
    let mut received = Vec::new();
    for _ in 0..50 {
        let mut frame = vec![0; FRAME_SAMPLES];
        generator.fill(&mut frame);
        let packet = encoder.encode(&frame).unwrap();
        assert!(
            packet.len() < frame.len() * 2,
            "Opus compresses: {} bytes",
            packet.len()
        );
        let pcm = decoder.decode(&packet).unwrap();
        assert_eq!(pcm.len(), FRAME_SAMPLES);
        received.extend_from_slice(&pcm);
    }
    // the decoder's first frames hold its warm-up, skip them
    let steady = &received[FRAME_SAMPLES * 5..];
    assert_near(dominant_frequency(steady, SAMPLE_RATE), 440.0);
    let fraction = energy_fraction(steady, SAMPLE_RATE, 440.0);
    assert!(fraction > 0.9, "440 Hz carries {fraction} of the decoded energy");
}

#[test]
fn opus_refuses_a_frame_of_the_wrong_length() {
    let mut encoder = OpusEncoder::new().unwrap();
    assert!(matches!(encoder.encode(&[0; 100]), Err(Error::Opus(_))));
}

#[test]
fn latency_stats_summarize_samples() {
    let samples: Vec<Duration> = (1..=100).map(Duration::from_millis).collect();
    let stats = LatencyStats::from_samples(&samples).unwrap();
    assert_eq!(stats.count, 100);
    assert_eq!(stats.min, Duration::from_millis(1));
    assert_eq!(stats.p50, Duration::from_millis(50));
    assert_eq!(stats.p95, Duration::from_millis(95));
    assert_eq!(stats.max, Duration::from_millis(100));
    assert_eq!(stats.mean, Duration::from_micros(50_500));
    assert_eq!(LatencyStats::from_samples(&[]), None);
    let one = LatencyStats::from_samples(&[Duration::from_millis(7)]).unwrap();
    let seven = Duration::from_millis(7);
    assert_eq!(
        (one.min, one.p50, one.p95, one.max, one.mean),
        (seven, seven, seven, seven, seven)
    );
}

#[test]
fn a_loop_without_frames_is_refused() {
    let config = LoopConfig {
        frames: 0,
        ..LoopConfig::default()
    };
    assert!(matches!(run_loopback(&config), Err(Error::Config(_))));
}

#[test]
fn tone_crosses_two_str0m_peers_over_loopback_udp() {
    let config = LoopConfig {
        tone_hz: 440.0,
        frames: 100,
        ..LoopConfig::default()
    };
    let report = run_loopback(&config).unwrap();
    eprintln!("{report}");

    assert_eq!(report.frames_sent, 100);
    assert!(
        report.frames_received >= 95,
        "B received {} of 100 frames",
        report.frames_received
    );
    assert!(
        report.frames_echoed >= 95,
        "A got {} of 100 echoes back",
        report.frames_echoed
    );
    assert_near(report.received_hz, 440.0);
    assert_near(report.echoed_hz, 440.0);

    let one_way = report.one_way.expect("one-way latency is recorded");
    let round_trip = report.round_trip.expect("round-trip latency is recorded");
    assert!(one_way.count >= 95 && round_trip.count >= 95);
    assert!(one_way.p50 > Duration::ZERO && one_way.p50 <= round_trip.p50);
    assert!(
        round_trip.p95 < Duration::from_millis(100),
        "loopback round trip p95 {:?}",
        round_trip.p95
    );
}

#[test]
fn a_different_tone_arrives_as_that_tone() {
    let config = LoopConfig {
        tone_hz: 1000.0,
        frames: 40,
        ..LoopConfig::default()
    };
    let report = run_loopback(&config).unwrap();
    assert_near(report.received_hz, 1000.0);
    assert_near(report.echoed_hz, 1000.0);
}
