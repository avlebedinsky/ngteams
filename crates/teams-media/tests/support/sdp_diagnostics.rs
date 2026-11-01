//! SDP failures retain structural context while arbitrary rejected content never becomes an error message.

use super::*;

const PRIVATE: &str = "SyntheticIceCredentialMustNeverReachDiagnostics";

#[test]
fn the_real_offer_and_answer_parsers_do_not_return_the_secret_bearing_rejected_origin() {
    let malformed = format!(
        "v=0\r\no=- a=ice-pwd:{PRIVATE}\r\ns=-\r\nt=0 0\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=mid:0\r\na=rtpmap:111 opus/48000/2\r\n"
    );
    for error in [
        SdpOffer::from_sdp_string(&malformed).unwrap_err(),
        SdpAnswer::from_sdp_string(&malformed).unwrap_err(),
    ] {
        assert_eq!(error, "invalid SDP origin line");
        assert!(!error.contains(PRIVATE));
        assert!(!error.contains("o="));
    }
}

#[test]
fn every_reported_structural_location_omits_its_untrusted_field_value() {
    for (field, location) in [
        ("v=", "version line"),
        ("o=", "origin line"),
        ("s=", "session name line"),
        ("t=", "timing line"),
        ("m=", "media line"),
        ("c=", "connection line"),
        ("a=ice-pwd:", "ICE credential attribute"),
        ("a=ice-ufrag:", "ICE credential attribute"),
        ("a=candidate:", "ICE candidate attribute"),
        ("a=fingerprint:", "DTLS fingerprint attribute"),
        ("a=rtpmap:", "codec attribute"),
        ("a=fmtp:", "codec attribute"),
        ("a=unknown:", "attribute"),
        ("unrecognized:", "session description"),
    ] {
        let error = sdp_parse_error(&format!("{field}{PRIVATE}"));
        assert_eq!(error, format!("invalid SDP {location}"));
        assert!(!error.contains(PRIVATE));
        assert!(!error.contains(field));
    }
}
