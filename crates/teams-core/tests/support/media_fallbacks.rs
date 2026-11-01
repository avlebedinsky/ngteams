//! Camera-off and malformed-content fallback behavior on an unconnected, device-free media session.

use super::*;
use teams_media::devices::file::{AudioInput, FileDevices, FrameCounter, Pacing, PatternCamera};
use teams_media::sdp::SDP_NGC;

struct DefaultOutput;

impl VideoOutput for DefaultOutput {
    fn remote(&self, _: usize) -> Box<dyn VideoSink> {
        Box::new(FrameCounter::new())
    }
    fn preview(&self) -> Option<Box<dyn VideoSink>> {
        None
    }
}

#[test]
fn default_video_output_and_media_debug_do_not_name_devices_or_credentials() {
    let output = Arc::new(DefaultOutput);
    assert!(output.enabled());
    assert!(matches!(
        output.background().get(),
        teams_media::video::effects::Background::None
    ));
    let video = crate::account::call_video(output);
    let devices = Arc::new(FileDevices {
        input: AudioInput::Silence,
        recording: None,
        camera: PatternCamera::default(),
        pacing: Pacing::Unpaced,
    });
    let media = CallMedia {
        devices,
        relay: Relay::Fixed(Vec::new()),
        session: SessionConfig::default(),
        video: Some(video.clone()),
    };
    assert!(format!("{video:?}").contains("CallVideo"));
    let debug = format!("{media:?}");
    assert!(debug.contains("CallMedia"));
    assert!(!debug.contains("devices") && !debug.contains("password"));
}

#[test]
fn a_camera_offer_without_a_started_codec_fails_and_leaves_its_dialect_receiving_only() {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let relay = RelayConfig {
        servers: vec![RelayServer::udp(socket.local_addr().unwrap())],
        credentials: relay::Credentials {
            username: "synthetic-only".into(),
            password: "synthetic-never-used".into(),
        },
    };
    // No remote SDP or device is supplied; the sole relay address belongs to our loopback socket.
    let session = MediaSession::start_without_devices(&relay, SessionConfig::default()).unwrap();
    let dialect = Mutex::new(NgcSession::new());
    assert!(!start_video(&session, None, VideoKind::OneToOne));
    assert!(
        offer_line(&session, &dialect, true)
            .unwrap_err()
            .contains("video didn't start")
    );
    assert_eq!(lock(&dialect).video(), VideoLines::Main { sending: false });
    assert!(!session.camera_on());
    let devices = FileDevices {
        input: AudioInput::Silence,
        recording: None,
        camera: PatternCamera::default(),
        pacing: Pacing::Unpaced,
    };
    assert!(turn_camera(&session, &dialect, &devices, false).is_err());
    assert!(!session.camera_on());
    session.close();
}

#[test]
fn content_wrappers_preserve_the_dialect_and_reject_an_unrecognized_content_type() {
    let original = NgcSdp::new(
        SDP_NGC,
        "v=0\r\ns=synthetic\r\nm=audio 3478 RTP/SAVP 111\r\na=mid:0\r\na=rtpmap:111 opus/48000/2\r\n",
    )
    .unwrap();
    let wrapped = content(&original);
    let parsed = ngc(&wrapped).unwrap();
    assert_eq!(parsed.as_str(), original.as_str());
    assert_eq!(parsed.content_type(), original.content_type());
    assert!(ngc(&MediaContent::new("synthetic", "application/not-sdp")).is_err());
    let bandwidth = bandwidth_seed();
    assert_eq!(bandwidth.mids, ["*"]);
    let parameter: serde_json::Value = serde_json::from_str(&bandwidth.media_parameter).unwrap();
    assert_eq!(parameter["sendSideBWSeed"]["seedValueBitsPerSec"], BANDWIDTH_SEED);
}
