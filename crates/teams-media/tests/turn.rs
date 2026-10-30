//! The STUN codec of the test TURN server (`teams_media::stun`, `test-support`) on the RFC 5769 test vectors, and the
//! relay's credentials kept out of a log. No sockets: libwebrtc is the call's TURN client.

use teams_media::relay::Credentials;
use teams_media::stun::{
    Attribute, ChannelData, Class, Framing, IntegrityKey, Message, Method, StreamDecoder, StunError, TransactionId,
    verify_integrity,
};

fn hex(text: &str) -> Vec<u8> {
    let nibble = |digit: u8| {
        char::from(digit)
            .to_digit(16)
            .and_then(|value| u8::try_from(value).ok())
            .unwrap_or_default()
    };
    let digits: Vec<u8> = text.bytes().filter(u8::is_ascii_hexdigit).collect();
    digits
        .as_chunks::<2>()
        .0
        .iter()
        .map(|[high, low]| (nibble(*high) << 4) | nibble(*low))
        .collect()
}

// RFC 5769 2.1: a request with short-term credentials, SOFTWARE padded with spaces, and a FINGERPRINT
const SAMPLE_REQUEST: &str = "
    00 01 00 58 21 12 a4 42 b7 e7 a7 01 bc 34 d6 86 fa 87 df ae
    80 22 00 10 53 54 55 4e 20 74 65 73 74 20 63 6c 69 65 6e 74
    00 24 00 04 6e 00 01 ff
    80 29 00 08 93 2f f9 b1 51 26 3b 36
    00 06 00 09 65 76 74 6a 3a 68 36 76 59 20 20 20
    00 08 00 14 9a ea a7 0c bf d8 cb 56 78 1e f2 b5 b2 d3 f2 49 c1 b5 71 a2
    80 28 00 04 e5 7a 3b cf";
const SAMPLE_REQUEST_PASSWORD: &str = "VOkJxbRl1RmTxUk/WvJxBt";

// RFC 5769 2.2: an IPv4 success response
const SAMPLE_IPV4_RESPONSE: &str = "
    01 01 00 3c 21 12 a4 42 b7 e7 a7 01 bc 34 d6 86 fa 87 df ae
    80 22 00 0b 74 65 73 74 20 76 65 63 74 6f 72 20
    00 20 00 08 00 01 a1 47 e1 12 a6 43
    00 08 00 14 2b 91 f5 99 fd 9e 90 c3 8c 74 89 f9 2a f9 ba 53 f0 6b e7 d7
    80 28 00 04 c0 7d 4c 96";

// RFC 5769 2.3: an IPv6 success response
const SAMPLE_IPV6_RESPONSE: &str = "
    01 01 00 48 21 12 a4 42 b7 e7 a7 01 bc 34 d6 86 fa 87 df ae
    80 22 00 0b 74 65 73 74 20 76 65 63 74 6f 72 20
    00 20 00 14 00 02 a1 47 01 13 a9 fa a5 d3 f1 79 bc 25 f4 b5 be d2 b9 d9
    00 08 00 14 a3 82 95 4e 4b e6 7b f1 17 84 c9 7c 82 92 c2 75 bf e3 ed 41
    80 28 00 04 c8 fb 0b 4c";

// RFC 5769 2.4: a request with long-term credentials (user name in katakana, the password after SASLprep)
const SAMPLE_LONG_TERM: &str = "
    00 01 00 60 21 12 a4 42 78 ad 34 33 c6 ad 72 c0 29 da 41 2e
    00 06 00 12 e3 83 9e e3 83 88 e3 83 aa e3 83 83 e3 82 af e3 82 b9 00 00
    00 15 00 1c 66 2f 2f 34 39 39 6b 39 35 34 64 36 4f 4c 33 34 6f 4c 39 46 53 54 76 79 36 34 73 41
    00 14 00 0b 65 78 61 6d 70 6c 65 2e 6f 72 67 00
    00 08 00 14 f6 70 24 65 6d d6 4a 3e 02 b8 e0 71 2e 85 c9 a2 8c a8 96 66";

#[test]
fn rfc5769_sample_request_decodes_and_verifies() {
    let bytes = hex(SAMPLE_REQUEST);
    let message = Message::decode(&bytes).unwrap();
    assert_eq!((message.class, message.method), (Class::Request, Method::Binding));
    assert!(
        message
            .attributes
            .contains(&Attribute::Software("STUN test client".into()))
    );
    assert!(message.attributes.contains(&Attribute::Username("evtj:h6vY".into())));
    assert!(verify_integrity(
        &bytes,
        &IntegrityKey::short_term(SAMPLE_REQUEST_PASSWORD)
    ));
    assert!(!verify_integrity(&bytes, &IntegrityKey::short_term("wrong")));
}

#[test]
fn rfc5769_responses_give_their_xor_mapped_addresses() {
    let cases = [
        (SAMPLE_IPV4_RESPONSE, "192.0.2.1:32853"),
        (SAMPLE_IPV6_RESPONSE, "[2001:db8:1234:5678:11:2233:4455:6677]:32853"),
    ];
    for (vector, expected) in cases {
        let bytes = hex(vector);
        let message = Message::decode(&bytes).unwrap();
        assert_eq!((message.class, message.method), (Class::Success, Method::Binding));
        assert!(
            message
                .attributes
                .contains(&Attribute::XorMappedAddress(expected.parse().unwrap()))
        );
        assert!(verify_integrity(
            &bytes,
            &IntegrityKey::short_term(SAMPLE_REQUEST_PASSWORD)
        ));
    }
}

#[test]
fn rfc5769_long_term_request_verifies_with_the_md5_key() {
    let bytes = hex(SAMPLE_LONG_TERM);
    let message = Message::decode(&bytes).unwrap();
    let username = "\u{30DE}\u{30C8}\u{30EA}\u{30C3}\u{30AF}\u{30B9}";
    assert!(message.attributes.contains(&Attribute::Username(username.into())));
    assert!(message.attributes.contains(&Attribute::Realm("example.org".into())));
    let key = IntegrityKey::long_term(username, "example.org", "TheMatrIX");
    assert!(verify_integrity(&bytes, &key));
}

#[test]
fn encoding_writes_the_xor_address_as_the_rfc_does() {
    let vector = hex(SAMPLE_IPV4_RESPONSE);
    let id = TransactionId(vector[8..20].try_into().unwrap());
    let message = Message::new(Class::Success, Method::Binding, id)
        .with(Attribute::XorMappedAddress("192.0.2.1:32853".parse().unwrap()));
    let bytes = message.encode(None, false);
    assert_eq!(&bytes[20..], &vector[36..48], "XOR-MAPPED-ADDRESS as in RFC 5769 2.2");
}

#[test]
fn encode_and_decode_round_trip_with_integrity_and_fingerprint() {
    let key = IntegrityKey::long_term("user", "realm", "pass");
    let message = Message::new(Class::Request, Method::ChannelBind, TransactionId([9; 12]))
        .with(Attribute::ChannelNumber(0x4001))
        .with(Attribute::XorPeerAddress("[2001:db8::1]:5000".parse().unwrap()))
        .with(Attribute::Username("user".into()))
        .with(Attribute::Nonce("abc".into()))
        .with(Attribute::ErrorCode {
            code: 438,
            reason: "Stale Nonce".into(),
        })
        .with(Attribute::Data(vec![1, 2, 3]));
    let bytes = message.encode(Some(&key), true);
    let decoded = Message::decode(&bytes).unwrap();
    assert_eq!(&decoded.attributes[..6], &message.attributes[..]);
    assert!(verify_integrity(&bytes, &key));
    assert_eq!(decoded.error_code(), Some((438, "Stale Nonce")));

    let mut corrupted = bytes.clone();
    corrupted[25] ^= 1;
    assert_eq!(Message::decode(&corrupted), Err(StunError::BadFingerprint));
    assert!(matches!(Message::decode(&bytes[..10]), Err(StunError::Truncated)));
}

#[test]
fn channel_data_pads_only_when_asked_and_rejects_bad_channels() {
    let message = ChannelData {
        channel: 0x4000,
        data: vec![1, 2, 3, 4, 5],
    };
    assert_eq!(message.encode(false).len(), 9);
    let padded = message.encode(true);
    assert_eq!(padded.len(), 12);
    assert_eq!(ChannelData::decode(&padded).unwrap(), message);
    assert_eq!(
        ChannelData::decode(&[0x50, 0, 0, 0]),
        Err(StunError::BadChannel(0x5000))
    );
}

#[test]
fn stream_decoder_splits_native_tcp_however_it_is_cut() {
    let stun = Message::new(Class::Indication, Method::Data, TransactionId([1; 12]))
        .with(Attribute::Data(vec![7; 5]))
        .encode(None, false);
    let channel = ChannelData {
        channel: 0x4002,
        data: vec![8; 3],
    }
    .encode(false);
    let mut stream = Framing::Native.frame(&stun).unwrap();
    stream.extend(Framing::Native.frame(&channel).unwrap());
    assert_eq!(stream.len(), stun.len() + 8, "ChannelData padded to 4 bytes on TCP");

    let mut decoder = StreamDecoder::new(Framing::Native);
    let mut messages = Vec::new();
    for byte in stream {
        decoder.push(&[byte]);
        while let Some(message) = decoder.next_message().unwrap() {
            messages.push(message);
        }
    }
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0], stun);
    assert_eq!(ChannelData::decode(&messages[1]).unwrap().data, vec![8; 3]);

    let mut junk = StreamDecoder::new(Framing::Native);
    junk.push(&[0x80, 0, 0, 0]);
    assert_eq!(junk.next_message(), Err(StunError::Desync));
}

#[test]
fn stream_decoder_splits_rfc4571_frames() {
    let first = vec![0x80, 1, 2];
    let second = vec![0; 300];
    let mut stream = Framing::Rfc4571.frame(&first).unwrap();
    stream.extend(Framing::Rfc4571.frame(&second).unwrap());
    assert_eq!(&stream[..2], &[0, 3]);
    let mut decoder = StreamDecoder::new(Framing::Rfc4571);
    decoder.push(&stream[..4]);
    assert_eq!(decoder.next_message().unwrap(), None);
    decoder.push(&stream[4..]);
    assert_eq!(decoder.next_message().unwrap(), Some(first));
    assert_eq!(decoder.next_message().unwrap(), Some(second));
    assert_eq!(decoder.next_message().unwrap(), None);
    assert_eq!(Framing::Rfc4571.frame(&vec![0; 70_000]), Err(StunError::TooLarge));
}

#[test]
fn credentials_debug_hides_the_password() {
    let credentials = Credentials {
        username: "lab-user".into(),
        password: "lab-password".into(),
    };
    let text = format!("{credentials:?}");
    assert!(!text.contains("lab-password"), "{text}");
    let key = IntegrityKey::long_term("lab-user", "turn.test", "lab-password");
    assert!(!format!("{key:?}").contains("lab-password"));
}
