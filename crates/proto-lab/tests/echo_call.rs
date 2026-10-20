//! The `echo-call` scenario's offline parts: the tone the fake microphone plays, the echo service's MRI from the lab
//! config, and what a capture's WebRTC statistics say about the call's audio (synthetic `rtc` entries shaped like the
//! WebRTC hooks' reports).

use std::path::Path;

use proto_lab::config::LabConfig;
use proto_lab::scenario::echo_call::{AudioSummary, call_starts, echo_mri, tone_wav};
use serde_json::json;

// --- the tone ---

#[test]
fn the_tone_is_a_mono_16_bit_wav_of_the_asked_frequency() {
    let rate = 8_000;
    let wav = tone_wav(440, 1, rate);
    assert_eq!(&wav[0..4], b"RIFF");
    assert_eq!(&wav[8..16], b"WAVEfmt ");
    let u16_at = |at: usize| u16::from_le_bytes([wav[at], wav[at + 1]]);
    let u32_at = |at: usize| u32::from_le_bytes([wav[at], wav[at + 1], wav[at + 2], wav[at + 3]]);
    assert_eq!(u32_at(4) as usize, wav.len() - 8, "the RIFF size");
    assert_eq!(u16_at(20), 1, "PCM");
    assert_eq!(u16_at(22), 1, "mono");
    assert_eq!(u32_at(24), rate);
    assert_eq!(u32_at(28), rate * 2, "bytes per second");
    assert_eq!(u16_at(32), 2, "bytes per frame");
    assert_eq!(u16_at(34), 16, "bits per sample");
    assert_eq!(&wav[36..40], b"data");
    assert_eq!(u32_at(40) as usize, (rate * 2) as usize);
    assert_eq!(wav.len(), 44 + (rate * 2) as usize);

    let samples: Vec<i16> = wav[44..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| i16::from_le_bytes(*pair))
        .collect();
    // half of full scale at the peak
    let peak = samples.iter().map(|sample| sample.unsigned_abs()).max().unwrap();
    assert!((16_000..=16_384).contains(&peak), "peak {peak}");
    // 440 Hz: 880 sign changes in one second
    let crossings = samples.windows(2).filter(|pair| (pair[0] < 0) != (pair[1] < 0)).count();
    assert!((878..=882).contains(&crossings), "{crossings} zero crossings");
}

// --- the echo service's MRI ---

const ACCOUNTS: &str = "[[accounts]]\nalias = \"a\"\nupn = \"bot-a@lab.example.com\"\n\n\
[[accounts]]\nalias = \"b\"\nupn = \"bot-b@lab.example.com\"\n\n\
[[accounts]]\nalias = \"c\"\nupn = \"bot-c@lab.example.com\"\n\n";

#[test]
fn the_echo_mri_comes_only_from_the_echo_service_s_entry() {
    let parse = |services: &str| LabConfig::parse(&format!("{ACCOUNTS}{services}"), Path::new("/lab")).unwrap();

    let none = parse("[[service_participants]]\nname = \"echo\"\n");
    assert_eq!(echo_mri(&none), None);

    let other =
        parse("[[service_participants]]\nname = \"other\"\nmri = \"28:00000000-0000-4000-8000-0000000000e1\"\n");
    assert_eq!(echo_mri(&other), None);

    let echo = parse(
        "[[service_participants]]\nname = \"other\"\nmri = \"28:00000000-0000-4000-8000-0000000000e1\"\n\n\
         [[service_participants]]\nname = \"echo\"\nmri = \"28:00000000-0000-4000-8000-0000000000e0\"\n",
    );
    assert_eq!(echo_mri(&echo), Some("28:00000000-0000-4000-8000-0000000000e0"));
}

// --- the audio, from the capture's WebRTC statistics ---

fn rtc(seq: u64, pc: u64, event: &str, data: &serde_json::Value) -> String {
    json!({"seq": seq, "ts_ms": 1_700_000_000_000_u64 + seq, "type": "rtc", "session": "S1", "context": 1,
        "payload": {"pc": pc, "event": event, "ts": 1, "data": data}})
    .to_string()
}

fn stats(sent: u64, received: u64, energy: f64, level: f64) -> serde_json::Value {
    json!([
        {"type": "outbound-rtp", "kind": "audio", "packetsSent": sent},
        {"type": "inbound-rtp", "kind": "audio", "packetsReceived": received, "totalAudioEnergy": energy, "audioLevel": level},
        {"type": "inbound-rtp", "kind": "video", "packetsReceived": 999_999},
        {"type": "candidate-pair", "state": "succeeded"},
    ])
}

#[test]
fn audio_both_ways_needs_packets_out_and_sound_back() {
    let text = [
        rtc(1, 1, "create", &json!({"iceServers": []})),
        rtc(2, 1, "connectionstatechange", &json!("connecting")),
        rtc(3, 1, "connectionstatechange", &json!("connected")),
        rtc(4, 1, "stats", &stats(50, 40, 0.5, 0.25)),
        // the largest counters any report gave
        rtc(5, 1, "stats", &stats(100, 90, 1.5, 0.125)),
        "not json".to_owned(),
        json!({"seq": 6, "type": "cdp", "method": "Network.requestWillBeSent"}).to_string(),
    ]
    .join("\n");
    let audio = AudioSummary::from_events(&text);
    assert_eq!(audio.connections, 1);
    assert!(audio.connected);
    assert_eq!(audio.packets_sent, 100);
    assert_eq!(audio.packets_received, 90, "video packets don't count");
    assert!((audio.received_energy - 1.5).abs() < f64::EPSILON);
    assert!((audio.max_received_level - 0.25).abs() < f64::EPSILON);
    assert!(audio.both_ways());
}

// --- the call start, from the capture's verdicts ---

fn verdict(network_id: &str, method: &str, url: &str, verdict: &str) -> String {
    json!({"seq": 1, "ts_ms": 1, "type": "verdict", "session": "S1", "channel": "http", "request_id": "interception-1",
        "network_id": network_id, "method": method, "url": url, "verdict": verdict})
    .to_string()
}

#[test]
fn call_starts_count_each_request_once_by_its_verdict() {
    let start = "https://api.flightproxy.teams.microsoft.com/api/v2/epconv";
    let text = [
        // one request pauses on its page's session and the browser's: two verdicts, one call start
        verdict("1", "POST", start, "allow"),
        verdict("1", "POST", start, "allow"),
        verdict("2", "POST", start, "deny"),
        // a preflight, another URL, another channel
        verdict("3", "OPTIONS", start, "allow"),
        verdict("4", "POST", &format!("{start}/x"), "allow"),
        json!({"type": "verdict", "channel": "web_socket", "method": "POST", "url": start, "verdict": "allow"})
            .to_string(),
    ]
    .join("\n");
    assert_eq!(call_starts(&text), (1, 1));
    assert_eq!(call_starts(""), (0, 0));
}

#[test]
fn silence_back_or_nothing_sent_is_not_audio_both_ways() {
    let silent = [rtc(1, 1, "stats", &stats(100, 90, 0.0, 0.0))].join("\n");
    assert!(!AudioSummary::from_events(&silent).both_ways());
    let unsent = [rtc(1, 1, "stats", &stats(0, 90, 1.0, 0.5))].join("\n");
    assert!(!AudioSummary::from_events(&unsent).both_ways());
    let nothing = AudioSummary::from_events("");
    assert_eq!(nothing, AudioSummary::default());
    assert!(!nothing.both_ways());
}
