//! The `echo-call` scenario's offline parts: the fake microphone's tone and the call's captured WebRTC audio statistics.

// The full Linux browser/scenario harness is outside the Windows provisioning command set.
#![cfg(unix)]

use std::time::{Duration, Instant};

use proto_lab::scenario::echo_call::{
    AudioSummary, AudioTally, CallFollowUps, ECHO_WAIT, REQUIRED_FOLLOW_UPS, ToneWait, call_starts, tone_wav,
    wait_for_tone_in,
};
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
    assert!(!nothing.tone_back());
}

// --- the tone coming back, from the audio tap's readings ---

fn reading(seq: u64, pc: u64, track: &str, rms: f64, tone: f64) -> String {
    rtc(
        seq,
        pc,
        "audio",
        &json!({"track": track, "state": "running", "rate": 48_000, "rms": rms, "tone": tone}),
    )
}

#[test]
fn the_tone_is_back_after_enough_loud_tone_readings_of_one_track_in_a_row() {
    let mut lines = vec![
        // the echo's greeting: loud, but speech
        reading(1, 1, "T1", 0.2, 0.05),
        reading(2, 1, "T1", 0.2, 0.1),
    ];
    // three tone readings, broken by one that isn't: no run of four yet
    lines.extend((3..6).map(|seq| reading(seq, 1, "T1", 0.1, 0.95)));
    lines.push(reading(6, 1, "T1", 0.1, 0.2));
    let broken = AudioSummary::from_events(&lines.join("\n"));
    assert_eq!(broken.tap_readings, 6);
    assert_eq!(broken.longest_tone, 3);
    assert!(!broken.tone_back());
    assert!((broken.max_tone_share - 0.95).abs() < f64::EPSILON);
    assert!((broken.max_tap_rms - 0.2).abs() < f64::EPSILON);

    // another track's silent readings interleaved don't break T1's run of four
    lines.extend((7..11).flat_map(|seq| [reading(seq, 1, "T1", 0.1, 0.9), reading(seq, 1, "T2", 0.0, 0.0)]));
    let back = AudioSummary::from_events(&lines.join("\n"));
    assert_eq!(back.longest_tone, 4);
    assert!(back.tone_back());
}

#[test]
fn a_quiet_tone_or_a_tone_spread_over_tracks_is_not_the_tone_back() {
    // all tone, but at the level silence leaves behind
    let quiet: Vec<String> = (1..10).map(|seq| reading(seq, 1, "T1", 0.001, 0.99)).collect();
    let summary = AudioSummary::from_events(&quiet.join("\n"));
    assert_eq!(summary.longest_tone, 0);
    assert!(
        summary.max_tone_share.abs() < f64::EPSILON,
        "quiet readings don't count"
    );
    assert!(!summary.tone_back());
    // two tone readings on each of three tracks (one id on two connections)
    let spread = [
        reading(1, 1, "T1", 0.1, 0.9),
        reading(2, 1, "T1", 0.1, 0.9),
        reading(3, 2, "T1", 0.1, 0.9),
        reading(4, 2, "T1", 0.1, 0.9),
        reading(5, 1, "T2", 0.1, 0.9),
        reading(6, 1, "T2", 0.1, 0.9),
    ];
    let summary = AudioSummary::from_events(&spread.join("\n"));
    assert_eq!(summary.longest_tone, 2);
    assert_eq!(summary.connections, 2);
    assert!(!summary.tone_back());
}

#[test]
fn a_tally_fed_line_by_line_sums_up_as_the_whole_text() {
    let lines: Vec<String> = (1..8)
        .map(|seq| reading(seq, 1, "T1", 0.1, 0.9))
        .chain([rtc(9, 1, "stats", &stats(10, 9, 0.5, 0.2)), "torn {".to_owned()])
        .collect();
    let mut tally = AudioTally::default();
    for line in &lines {
        tally.add_line(line);
    }
    assert_eq!(tally.summary(), AudioSummary::from_events(&lines.join("\n")));
    assert!(tally.summary().tone_back());
    assert!(tally.summary().both_ways());
}

// --- the call's follow-ups, from the capture's verdicts ---

const EP: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep";

fn frame_verdict(data: &str, verdict: &str) -> String {
    json!({"seq": 1, "ts_ms": 1, "type": "verdict", "session": "S1", "channel": "web_socket",
        "url": "wss://zz-t.trouter.teams.microsoft.com/v4/c", "socket": 2, "frame_seq": 9,
        "frame": {"kind": "text", "data": data}, "verdict": verdict})
    .to_string()
}

fn counts(tally: &std::collections::BTreeMap<String, usize>) -> Vec<(&str, usize)> {
    tally
        .iter()
        .map(|(operation, count)| (operation.as_str(), *count))
        .collect()
}

#[test]
fn follow_ups_are_counted_by_operation_once_per_request() {
    let conv = format!("{EP}/conv-zz-01-prod-aks.conv.skype.com/conv/SyntheticConversation0");
    let cc = format!("{EP}/cc-zz-01-prod-aks.cc.skype.com/cc/v1");
    let metadata = format!("{conv}/updateEndpointMetadata?i=0-0-0-0&e=1");
    let text = [
        // one request pauses twice
        verdict("1", "PUT", &metadata, "allow"),
        verdict("1", "PUT", &metadata, "allow"),
        verdict(
            "2",
            "POST",
            &format!("{cc}/callAcceptance/SyntheticCall0/1/a1/1/acknowledge?i=0"),
            "allow",
        ),
        verdict("3", "POST", &format!("{conv}/add?i=0-0-0-0&e=1"), "allow"),
        verdict(
            "4",
            "POST",
            &format!("{cc}/mcProxy/SyntheticCall0/1/k1/1/mediaRenegotiation/"),
            "deny",
        ),
        // a preflight isn't the request, and the call start is no follow-up
        verdict("5", "OPTIONS", &format!("{conv}/leave"), "allow"),
        verdict(
            "6",
            "POST",
            "https://api.flightproxy.teams.microsoft.com/api/v2/epconv",
            "allow",
        ),
        // a read on a calling service, the broker's long poll, isn't one either
        verdict(
            "8",
            "GET",
            &format!("{EP}/broker-zz-01-prod-aks.broker.skype.com/api/v1/subscribe/SyntheticSubscription0/3"),
            "allow",
        ),
        frame_verdict("3:::{\"id\":1,\"status\":200}", "allow"),
        frame_verdict("3:::{\"id\":2,\"status\":200}", "deny"),
        frame_verdict("5:1+::{\"name\":\"ping\"}", "allow"),
    ]
    .join("\n");
    let follow_ups = CallFollowUps::from_events(&text);
    assert_eq!(
        counts(&follow_ups.allowed),
        [("acknowledge", 1), ("add", 1), ("updateEndpointMetadata", 1)]
    );
    assert_eq!(counts(&follow_ups.denied), [("mediaRenegotiation", 1)]);
    assert_eq!(follow_ups.delivery_responses, 1);
    assert_eq!(follow_ups.missing(), ["leave"]);

    let leave = verdict("7", "POST", &format!("{conv}/leave?i=0-0-0-0&e=1"), "allow");
    assert!(
        CallFollowUps::from_events(&format!("{text}\n{leave}"))
            .missing()
            .is_empty()
    );
    assert_eq!(CallFollowUps::from_events("").missing(), REQUIRED_FOLLOW_UPS);
}

#[test]
fn the_acceptance_is_acknowledged_over_trouter_or_by_request() {
    let cc = format!("{EP}/cc-zz-01-prod-aks.cc.skype.com/cc/v1");
    let by_request = verdict(
        "1",
        "POST",
        &format!("{cc}/callAcceptance/SyntheticCall0/1/a1/1/acknowledge?i=0"),
        "allow",
    );
    let over_trouter = frame_verdict("3:::{\"id\":1,\"status\":200}", "allow");
    let refused = [
        verdict(
            "2",
            "POST",
            &format!("{cc}/callAcceptance/SyntheticCall0/1/a1/1/acknowledge?i=0"),
            "deny",
        ),
        frame_verdict("3:::{\"id\":1,\"status\":200}", "deny"),
    ]
    .join("\n");
    assert!(CallFollowUps::from_events(&by_request).acknowledged());
    assert!(CallFollowUps::from_events(&over_trouter).acknowledged());
    assert!(!CallFollowUps::from_events(&refused).acknowledged());
    assert!(!CallFollowUps::from_events("").acknowledged());
}

// --- waiting for the tone ---

#[tokio::test]
async fn waiting_for_the_tone_ends_as_soon_as_the_connection_is_gone() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let file = dir.path().join("events.jsonl");
    std::fs::write(&file, format!("{}\n", reading(1, 1, "T1", 0.001, 0.1))).expect("the capture");
    let started = Instant::now();
    let mut kept_up = 0;
    let waited = wait_for_tone_in(&file, ECHO_WAIT, async |_| {
        kept_up += 1;
        false
    })
    .await;
    assert_eq!(waited, ToneWait::Lost);
    assert_eq!(kept_up, 1, "no second look once the connection is gone");
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
}

#[tokio::test]
async fn waiting_for_the_tone_ends_once_the_capture_shows_it_back() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let file = dir.path().join("events.jsonl");
    let lines: String = (1..6).map(|seq| reading(seq, 1, "T1", 0.1, 0.9) + "\n").collect();
    std::fs::write(&file, lines).expect("the capture");
    let mut kept_up = 0;
    let waited = wait_for_tone_in(&file, ECHO_WAIT, async |_| {
        kept_up += 1;
        true
    })
    .await;
    assert!(matches!(waited, ToneWait::Back(_)), "{waited:?}");
    assert_eq!(kept_up, 1);
}

#[tokio::test]
async fn waiting_for_the_tone_gives_up_at_the_limit() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let file = dir.path().join("events.jsonl");
    let waited = wait_for_tone_in(&file, Duration::from_millis(50), async |_| {
        tokio::time::sleep(Duration::from_millis(10)).await;
        true
    })
    .await;
    assert_eq!(waited, ToneWait::TimedOut);
}
