//! The STUN codec on the RFC 5769 test vectors, and the sans-IO TURN client on synthetic exchanges: the test plays
//! the server, message by message, on a virtual clock. No sockets.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use echo_call::stun::{
    Attribute, ChannelData, Class, Framing, IntegrityKey, Message, Method, StreamDecoder, StunError, TransactionId,
    verify_integrity,
};
use echo_call::turn::{Credentials, Timing, Transport, TurnClient, TurnError, TurnEvent, TurnFailure};

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

// --- the client against a scripted server ---

const USER: &str = "lab-user";
const PASSWORD: &str = "lab-password";
const REALM: &str = "turn.test";
const RELAYED: &str = "198.51.100.7:50000";
const MAPPED: &str = "203.0.113.9:40000";
const PEER: &str = "192.0.2.44:6000";

fn key() -> IntegrityKey {
    IntegrityKey::long_term(USER, REALM, PASSWORD)
}

fn credentials(password: &str) -> Credentials {
    Credentials {
        username: USER.into(),
        password: password.into(),
    }
}

/// What the client sent: STUN messages (decoded, and as sent), and `ChannelData` as raw bytes.
type Sent = (Vec<(Message, Vec<u8>)>, Vec<Vec<u8>>);

/// The client and the virtual clock.
struct Script {
    client: TurnClient,
    now: Instant,
}

impl Script {
    fn new(transport: Transport, password: &str) -> Self {
        let timing = Timing {
            rto: Duration::from_millis(100),
            max_sends: 3,
            last_wait: Duration::from_millis(400),
        };
        let client = TurnClient::new(transport, credentials(password)).with_timing(timing);
        Self {
            client,
            now: Instant::now(),
        }
    }

    /// Every message the client queued.
    fn sent(&mut self) -> Sent {
        let mut stun = Vec::new();
        let mut channel = Vec::new();
        while let Some(bytes) = self.client.poll_transmit() {
            match Message::decode(&bytes) {
                Ok(message) => stun.push((message, bytes)),
                Err(_) => channel.push(bytes),
            }
        }
        (stun, channel)
    }

    fn one_request(&mut self) -> (Message, Vec<u8>) {
        let (mut stun, channel) = self.sent();
        assert!(channel.is_empty());
        assert_eq!(stun.len(), 1, "{stun:?}");
        stun.remove(0)
    }

    fn reply(&mut self, response: &Message, key: Option<&IntegrityKey>) {
        assert_eq!(self.client.handle_input(self.now, &response.encode(key, true)), Ok(()));
    }

    fn events(&mut self) -> Vec<TurnEvent> {
        std::iter::from_fn(|| self.client.poll_event()).collect()
    }

    fn advance(&mut self, by: Duration) {
        self.now += by;
        assert_eq!(self.client.handle_timeout(self.now), Ok(()));
    }

    /// Runs the 401 → authenticated Allocate → success exchange.
    fn allocated(transport: Transport) -> Self {
        let mut script = Self::new(transport, PASSWORD);
        assert_eq!(script.client.allocate(script.now), Ok(()));
        let (first, _) = script.one_request();
        script.reply(&challenge(&first, "nonce-1"), None);
        let (second, _) = script.one_request();
        script.reply(&allocate_success(&second, 600), Some(&key()));
        script.events();
        script
    }
}

fn challenge(request: &Message, nonce: &str) -> Message {
    Message::new(Class::Error, request.method, request.transaction_id)
        .with(Attribute::ErrorCode {
            code: 401,
            reason: "Unauthorized".into(),
        })
        .with(Attribute::Realm(REALM.into()))
        .with(Attribute::Nonce(nonce.into()))
}

fn success(request: &Message) -> Message {
    Message::new(Class::Success, request.method, request.transaction_id)
}

fn allocate_success(request: &Message, lifetime: u32) -> Message {
    success(request)
        .with(Attribute::XorRelayedAddress(SocketAddr::from((
            [198, 51, 100, 7],
            50_000,
        ))))
        .with(Attribute::XorMappedAddress(SocketAddr::from((
            [203, 0, 113, 9],
            40_000,
        ))))
        .with(Attribute::Lifetime(lifetime))
}

fn nonce_of(message: &Message) -> Option<String> {
    message.find(|a| match a {
        Attribute::Nonce(n) => Some(n.clone()),
        _ => None,
    })
}

#[test]
fn allocate_learns_the_realm_from_a_401_and_retries_authenticated() {
    let mut script = Script::new(Transport::Udp, PASSWORD);
    script.client.allocate(script.now).unwrap();

    let (first, first_bytes) = script.one_request();
    assert_eq!((first.class, first.method), (Class::Request, Method::Allocate));
    assert!(
        first.attributes.contains(&Attribute::RequestedTransport(17)),
        "UDP relaying"
    );
    assert!(!first.has_integrity(), "the first Allocate carries no credentials");
    assert!(!verify_integrity(&first_bytes, &key()));

    script.reply(&challenge(&first, "nonce-1"), None);
    let (second, second_bytes) = script.one_request();
    assert_ne!(second.transaction_id, first.transaction_id, "a new transaction");
    assert!(second.attributes.contains(&Attribute::Username(USER.into())));
    assert!(second.attributes.contains(&Attribute::Realm(REALM.into())));
    assert_eq!(nonce_of(&second).as_deref(), Some("nonce-1"));
    assert!(
        verify_integrity(&second_bytes, &key()),
        "MESSAGE-INTEGRITY over the long-term key"
    );

    script.reply(&allocate_success(&second, 600), Some(&key()));
    assert_eq!(
        script.events(),
        [TurnEvent::Allocated {
            relayed: RELAYED.parse().unwrap(),
            mapped: Some(MAPPED.parse().unwrap()),
            lifetime: Duration::from_secs(600),
        }]
    );
    assert_eq!(script.client.relayed(), Some(RELAYED.parse().unwrap()));
}

#[test]
fn a_second_401_means_the_credentials_are_wrong() {
    let mut script = Script::new(Transport::Tcp, "wrong");
    script.client.allocate(script.now).unwrap();
    let (first, _) = script.one_request();
    script.reply(&challenge(&first, "nonce-1"), None);
    let (second, _) = script.one_request();
    script.reply(&challenge(&second, "nonce-1"), None);
    assert_eq!(script.events(), [TurnEvent::Failed(TurnFailure::Unauthorized)]);
    assert!(script.client.is_done());
}

#[test]
fn a_success_without_valid_integrity_is_dropped_and_the_request_retransmitted() {
    let mut script = Script::new(Transport::Udp, PASSWORD);
    script.client.allocate(script.now).unwrap();
    let (first, _) = script.one_request();
    script.reply(&challenge(&first, "nonce-1"), None);
    let (second, second_bytes) = script.one_request();

    script.reply(&allocate_success(&second, 600), None);
    script.reply(
        &allocate_success(&second, 600),
        Some(&IntegrityKey::long_term(USER, REALM, "forged")),
    );
    assert!(
        script.events().is_empty(),
        "no allocation from an unauthenticated answer"
    );

    script.advance(Duration::from_millis(100));
    let (again, again_bytes) = script.one_request();
    assert_eq!(again.transaction_id, second.transaction_id, "a retransmission");
    assert_eq!(again_bytes, second_bytes);
    script.reply(&allocate_success(&second, 600), Some(&key()));
    assert!(matches!(script.events()[..], [TurnEvent::Allocated { .. }]));
}

#[test]
fn udp_requests_are_retransmitted_then_time_out() {
    let mut script = Script::new(Transport::Udp, PASSWORD);
    script.client.allocate(script.now).unwrap();
    script.one_request();
    script.advance(Duration::from_millis(100));
    script.one_request();
    script.advance(Duration::from_millis(200));
    script.one_request();
    script.advance(Duration::from_millis(399));
    assert!(script.events().is_empty());
    script.advance(Duration::from_millis(1));
    assert_eq!(
        script.events(),
        [TurnEvent::Failed(TurnFailure::Timeout(Method::Allocate))]
    );
}

#[test]
fn allocation_refusals_and_redirects_are_final() {
    let alternate: SocketAddr = "192.0.2.200:3478".parse().unwrap();
    for (code, extra, expected) in [
        (
            486,
            None,
            TurnFailure::Rejected {
                code: 486,
                reason: "Quota".into(),
            },
        ),
        (
            300,
            Some(Attribute::AlternateServer(alternate)),
            TurnFailure::TryAlternate(alternate),
        ),
    ] {
        let mut script = Script::new(Transport::Tcp, PASSWORD);
        script.client.allocate(script.now).unwrap();
        let (first, _) = script.one_request();
        let mut refusal =
            Message::new(Class::Error, Method::Allocate, first.transaction_id).with(Attribute::ErrorCode {
                code,
                reason: "Quota".into(),
            });
        if let Some(extra) = extra {
            refusal = refusal.with(extra);
        }
        script.reply(&refusal, None);
        assert_eq!(script.events(), [TurnEvent::Failed(expected)]);
    }
}

#[test]
fn the_allocation_is_refreshed_before_it_runs_out_and_a_stale_nonce_is_renewed() {
    let mut script = Script::allocated(Transport::Udp);
    script.advance(Duration::from_secs(539));
    assert!(script.sent().0.is_empty(), "nothing before lifetime - 60 s");
    script.advance(Duration::from_secs(1));
    let (refresh, _) = script.one_request();
    assert_eq!(refresh.method, Method::Refresh);

    let stale = Message::new(Class::Error, Method::Refresh, refresh.transaction_id)
        .with(Attribute::ErrorCode {
            code: 438,
            reason: "Stale Nonce".into(),
        })
        .with(Attribute::Nonce("nonce-2".into()));
    script.reply(&stale, None);
    let (again, again_bytes) = script.one_request();
    assert_eq!(nonce_of(&again).as_deref(), Some("nonce-2"));
    assert!(verify_integrity(&again_bytes, &key()));

    script.reply(&success(&again).with(Attribute::Lifetime(300)), Some(&key()));
    assert_eq!(
        script.events(),
        [TurnEvent::Refreshed {
            lifetime: Duration::from_secs(300)
        }]
    );
}

#[test]
fn the_first_send_asks_for_a_permission_and_a_channel_and_goes_by_indication() {
    let mut script = Script::allocated(Transport::Udp);
    let peer: SocketAddr = PEER.parse().unwrap();
    script.client.send_to(script.now, peer, b"stun check").unwrap();

    let (stun, channel) = script.sent();
    assert!(channel.is_empty());
    let methods: Vec<(Class, Method)> = stun.iter().map(|(m, _)| (m.class, m.method)).collect();
    assert_eq!(
        methods,
        [
            (Class::Request, Method::CreatePermission),
            (Class::Request, Method::ChannelBind),
            (Class::Indication, Method::Send),
        ]
    );
    let (permission, bind, send) = (&stun[0], &stun[1], &stun[2]);
    assert!(
        permission
            .0
            .attributes
            .contains(&Attribute::XorPeerAddress(SocketAddr::new(peer.ip(), 0)))
    );
    assert!(permission.0.has_integrity() && bind.0.has_integrity());
    assert!(bind.0.attributes.contains(&Attribute::ChannelNumber(0x4000)));
    assert!(send.0.attributes.contains(&Attribute::Data(b"stun check".to_vec())));
    assert!(!send.0.has_integrity(), "indications aren't authenticated");

    script.reply(&success(&permission.0), Some(&key()));
    script.reply(&success(&bind.0), Some(&key()));
    assert_eq!(
        script.events(),
        [
            TurnEvent::PermissionCreated(peer.ip()),
            TurnEvent::ChannelBound { peer, channel: 0x4000 }
        ]
    );

    script.client.send_to(script.now, peer, b"rtp").unwrap();
    let (stun, channel) = script.sent();
    assert!(stun.is_empty(), "no new request once bound");
    assert_eq!(
        ChannelData::decode(&channel[0]).unwrap(),
        ChannelData {
            channel: 0x4000,
            data: b"rtp".to_vec()
        }
    );
}

#[test]
fn incoming_data_comes_by_indication_or_on_a_bound_channel_only() {
    let mut script = Script::allocated(Transport::Udp);
    let peer: SocketAddr = PEER.parse().unwrap();
    let data = Message::new(Class::Indication, Method::Data, TransactionId([3; 12]))
        .with(Attribute::XorPeerAddress(peer))
        .with(Attribute::Data(b"hi".to_vec()));
    script.reply(&data, None);
    assert_eq!(
        script.events(),
        [TurnEvent::Data {
            peer,
            payload: b"hi".to_vec()
        }]
    );

    let unknown = ChannelData {
        channel: 0x4ABC,
        data: b"who".to_vec(),
    };
    script.client.handle_input(script.now, &unknown.encode(false)).unwrap();
    assert!(
        script.events().is_empty(),
        "ChannelData on an unbound channel is dropped"
    );

    assert_eq!(
        script.client.handle_input(script.now, &[0xC0, 1, 2, 3]),
        Err(TurnError::Malformed(StunError::NotStun))
    );
}

#[test]
fn permissions_and_channels_are_refreshed_before_they_expire() {
    let mut script = Script::allocated(Transport::Tcp);
    let peer: SocketAddr = PEER.parse().unwrap();
    script.client.send_to(script.now, peer, b"x").unwrap();
    let (stun, _) = script.sent();
    script.reply(&success(&stun[0].0), Some(&key()));
    script.reply(&success(&stun[1].0), Some(&key()));
    script.events();

    script.advance(Duration::from_secs(240));
    let (permission, _) = script.one_request();
    assert_eq!(permission.method, Method::CreatePermission);
    script.reply(&success(&permission), Some(&key()));
    assert!(script.events().is_empty(), "a refresh reports nothing new");

    script.advance(Duration::from_secs(300));
    let methods: Vec<Method> = script.sent().0.iter().map(|(m, _)| m.method).collect();
    assert!(methods.contains(&Method::ChannelBind), "{methods:?}");
}

#[test]
fn close_releases_the_allocation() {
    let mut script = Script::allocated(Transport::Udp);
    script.client.close(script.now).unwrap();
    let (refresh, _) = script.one_request();
    assert!(refresh.attributes.contains(&Attribute::Lifetime(0)));
    script.reply(&success(&refresh).with(Attribute::Lifetime(0)), Some(&key()));
    assert_eq!(script.events(), [TurnEvent::Closed]);
    assert!(script.client.is_done());
    assert_eq!(
        script.client.send_to(script.now, PEER.parse().unwrap(), b"late"),
        Err(TurnError::NotAllocated)
    );
}

#[test]
fn nothing_is_sent_before_the_allocation() {
    let mut script = Script::new(Transport::Udp, PASSWORD);
    let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
    assert_eq!(script.client.permit(script.now, ip), Err(TurnError::NotAllocated));
    assert_eq!(script.client.close(script.now), Err(TurnError::NotAllocated));
    assert!(script.sent().0.is_empty());
}

#[test]
fn credentials_debug_hides_the_password() {
    let text = format!("{:?}", credentials(PASSWORD));
    assert!(text.contains(USER) && !text.contains(PASSWORD), "{text}");
    assert!(!format!("{:?}", key()).contains(PASSWORD));
}
