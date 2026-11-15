//! The mixer's side of the dialect: host candidates written as they are, one data stream for the session, and the
//! stream ids the mixer names its lines by. Synthetic SDP in str0m's shape.

use teams_sdp::sdp::{NgcSdp, NgcSession, SdpError, StreamIds};
use teams_sdp::{SdpAnswer, SdpOffer};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const IDS: StreamIds = StreamIds {
    audio: 1000,
    video: 1001,
    sharing: 1002,
    data: 1003,
};

/// An offer in str0m's shape: audio and a data channel, one host candidate, version `version`.
fn str0m_offer(version: u32) -> String {
    let transport = "a=ice-ufrag:Ufrag0123\r\na=ice-pwd:Password0123456789abcd\r\na=ice-options:trickle\r\n\
        a=fingerprint:sha-256 00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF\r\n\
        a=setup:actpass\r\n";
    format!(
        "v=0\r\no=str0m-0.25.0 1234 {version} IN IP4 0.0.0.0\r\ns=-\r\nt=0 0\r\na=group:BUNDLE au da\r\n\
         a=msid-semantic: WMS stream\r\n\
         m=audio 9 UDP/TLS/RTP/SAVPF 111\r\nc=IN IP4 0.0.0.0\r\n\
         a=candidate:fffeff 1 udp 2130706175 192.0.2.10 3480 typ host ufrag Ufrag0123\r\n{transport}a=mid:au\r\n\
         a=sendrecv\r\na=msid:stream track\r\na=rtcp-mux\r\na=rtpmap:111 opus/48000/2\r\n\
         a=fmtp:111 minptime=10;useinbandfec=1\r\na=ssrc:777 cname:mixer\r\n\
         m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\nc=IN IP4 0.0.0.0\r\n{transport}a=mid:da\r\n\
         a=sctp-port:5000\r\na=max-message-size:262144\r\n"
    )
}

/// An answer in str0m's shape to an offer whose lines are `0` (audio) and `1` (data).
fn str0m_answer(version: u32) -> String {
    let transport = "a=ice-ufrag:Ufrag0123\r\na=ice-pwd:Password0123456789abcd\r\n\
        a=fingerprint:sha-256 00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF\r\n\
        a=setup:passive\r\n";
    format!(
        "v=0\r\no=str0m-0.25.0 99 {version} IN IP4 0.0.0.0\r\ns=-\r\nt=0 0\r\na=group:BUNDLE 0 1\r\n\
         m=audio 9 UDP/TLS/RTP/SAVPF 111\r\nc=IN IP4 0.0.0.0\r\n\
         a=candidate:abc 1 udp 2130706175 192.0.2.10 3480 typ host ufrag Ufrag0123\r\n{transport}a=mid:0\r\n\
         a=sendrecv\r\na=rtcp-mux\r\na=rtpmap:111 opus/48000/2\r\na=ssrc:778 cname:mixer\r\n\
         m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\nc=IN IP4 0.0.0.0\r\n{transport}a=mid:1\r\n\
         a=sctp-port:5000\r\n"
    )
}

/// The value of every `a=<name>` line of the section whose m-line starts with `kind`.
fn values<'a>(sdp: &'a NgcSdp, kind: &str, name: &str) -> Vec<&'a str> {
    let prefix = format!("a={name}:");
    sdp.as_str()
        .split("m=")
        .filter(|section| section.starts_with(kind))
        .flat_map(|section| section.split("\r\n"))
        .filter_map(|line| line.strip_prefix(prefix.as_str()))
        .collect()
}

#[test]
fn the_mixers_offer_keeps_its_host_candidate_and_names_its_streams() -> TestResult {
    let mut mixer = NgcSession::mixer();
    mixer.set_stream_ids(IDS);
    let offer = mixer.write_offer(&SdpOffer::new(str0m_offer(2)))?;
    assert_eq!(
        values(&offer, "audio", "candidate"),
        ["1 1 udp 2130706175 192.0.2.10 3480 typ host"]
    );
    assert!(
        offer
            .as_str()
            .contains("m=audio 3480 RTP/SAVP 111\r\nc=IN IP4 192.0.2.10\r\n")
    );
    assert_eq!(values(&offer, "audio", "x-source-streamid"), ["1000"]);
    assert_eq!(values(&offer, "x-data", "x-source-streamid"), ["1003"]);
    assert_eq!(values(&offer, "x-data", "label"), ["data"]);
    Ok(())
}

#[test]
fn the_data_line_names_one_stream_for_the_whole_session() -> TestResult {
    let mut mixer = NgcSession::mixer();
    let first = mixer.write_offer(&SdpOffer::new(str0m_offer(2)))?;
    let again = mixer.write_offer(&SdpOffer::new(str0m_offer(3)))?;
    let range = values(&first, "x-data", "x-ssrc-range");
    assert_eq!(range.len(), 1);
    assert_eq!(range, values(&again, "x-data", "x-ssrc-range"));
    // apart from the origin's version, the two writes are the same
    let lines = |sdp: &NgcSdp| -> Vec<String> {
        sdp.as_str()
            .split("\r\n")
            .filter(|line| !line.starts_with("o="))
            .map(str::to_owned)
            .collect()
    };
    assert_eq!(lines(&first), lines(&again));
    Ok(())
}

#[test]
fn the_mixers_answer_is_the_same_each_time_it_answers_the_same_offer() -> TestResult {
    let mut caller = NgcSession::mixer();
    let offered = caller.write_offer(&SdpOffer::new(str0m_offer(2)))?;
    let mut mixer = NgcSession::mixer();
    mixer.set_stream_ids(IDS);
    mixer.read_offer(&offered)?;
    let first = mixer.write_answer(&SdpAnswer::new(str0m_answer(2)))?;
    let again = mixer.write_answer(&SdpAnswer::new(str0m_answer(3)))?;
    assert_eq!(
        values(&first, "audio", "candidate"),
        ["1 1 udp 2130706175 192.0.2.10 3480 typ host"]
    );
    assert_eq!(values(&first, "audio", "x-source-streamid"), ["1000"]);
    assert_eq!(values(&first, "x-data", "x-source-streamid"), ["1003"]);
    let range = values(&first, "x-data", "x-ssrc-range");
    assert_eq!(range, values(&again, "x-data", "x-ssrc-range"));
    Ok(())
}

#[test]
fn the_clients_side_still_refuses_a_host_candidate() {
    let mut client = NgcSession::new();
    assert!(matches!(
        client.write_offer(&SdpOffer::new(str0m_offer(2))),
        Err(SdpError::LocalCandidate(kind)) if kind == "host"
    ));
}
