//! The call's WebRTC peer: one libwebrtc `PeerConnection`, relayed through the call's TURN servers only.
//!
//! libwebrtc does everything below the SDP: ICE (relay-only: [`IceTransportsType::Relay`], so it gathers relayed
//! candidates alone and no local address goes into an offer), DTLS-SRTP, Opus and H.264 (its software encoder:
//! [`Peer::attach_video`]), the jitter buffer, bandwidth estimation, retransmissions and A/V sync. What stays ours:
//!
//! - **The lines** of a Teams call: an offer of ours has the lines the web client's has (a 1:1 call's audio, and its
//!   video line once the camera turns; a group call's audio, the main video line, the receiving lines, the sharing
//!   line and the data channel), each line's codecs restricted to Opus and H.264 as the web client offers them.
//! - **Candidates in the SDP**: Teams takes no trickled candidate, so an offer or answer goes out only once
//!   gathering ended (or [`GATHER_SETTLE`] after the first candidate), with the candidates written into its first line
//!   in use, as the web client writes them ([`Peer::local_sdp`]).
//! - **Which video lines are which**: line 0 the main video line, 1, 2, … a group call's receiving lines, read from
//!   the negotiated SDP ([`Peer::video_line`]); a sharing line (a screen, ours or the far end's: the dialect names
//!   them, [`Peer::set_sharing_lines`]) is none of them, its frames the share's ([`SHARING_LINE`]).
//! - **A screen shared** ([`Peer::share_screen`]): a line of its own, sending only, its track fed by the screen's
//!   frames; stopped ([`Peer::stop_screen`]), the next offer rejects that line, as the web client's does.
//! - **Sending**: the audio track carries what the session hands it ([`Peer::audio`]); the camera's track is on the
//!   main video line from the line's first negotiation on and sends only while the camera is on (its encoding active:
//!   [`Peer::set_sending`]), so turning a group call's camera changes no SDP.
//!
//! libwebrtc's own calls are asynchronous: each is run to completion on the asking thread ([`block_on`]), never on an
//! async runtime's. Its video source keeps a tokio task alive until the camera's first frame: [`runtime`] runs it.

use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use futures_executor::block_on;
use libwebrtc::MediaType;
use libwebrtc::audio_source::AudioSourceOptions;
use libwebrtc::audio_source::native::NativeAudioSource;
use libwebrtc::audio_track::RtcAudioTrack;
use libwebrtc::data_channel::{DataChannel, DataChannelInit, DataChannelState};
use libwebrtc::media_stream_track::MediaStreamTrack;
use libwebrtc::peer_connection::{
    AnswerOptions, BitrateSettings, IceConnectionState, IceGatheringState, OfferOptions, PeerConnection,
    PeerConnectionState, SignalingState, TrackEvent,
};
use libwebrtc::peer_connection_factory::native::PeerConnectionFactoryExt;
use libwebrtc::peer_connection_factory::{
    ContinualGatheringPolicy, IceServer, IceTransportsType, PeerConnectionFactory, RtcConfiguration,
};
use libwebrtc::rtp_parameters::{DegradationPreference, RtpCodecCapability};
use libwebrtc::rtp_sender::{RtpSender, VideoEncoderBackend};
use libwebrtc::rtp_transceiver::{RtpTransceiver, RtpTransceiverDirection, RtpTransceiverInit};
use libwebrtc::session_description::{SdpType, SessionDescription};
use libwebrtc::video_source::VideoResolution;
use libwebrtc::video_source::native::NativeVideoSource;
use libwebrtc::video_track::RtcVideoTrack;

use crate::Error;
use crate::devices::SAMPLE_RATE;
use crate::mcp::CHANNEL_LABEL;
use crate::relay::RelayConfig;
use crate::video::codec::EncoderSettings;

#[cfg(feature = "test-support")]
pub mod far_end;

/// How long gathering may take to bring a first candidate: the relay's allocation over UDP, TCP or TLS. With none by
/// then the offer or answer fails (`no relayed candidate`).
pub const GATHER_TIMEOUT: Duration = Duration::from_secs(15);
/// How long after the first candidate an offer or answer waits for gathering to end: the other URLs' allocations,
/// which add nothing once one allocation works.
pub const GATHER_SETTLE: Duration = Duration::from_secs(2);
/// The stream id our tracks name (`a=msid`).
const STREAM_ID: &str = "teams-media";
/// The stream id our screen's track names (`a=msid`): a stream of its own, as the web client's.
const SCREEN_STREAM_ID: &str = "teams-media-screen";
/// The video line a sharing line's frames are told as (`crate::session::RemoteSinks`): no camera's line.
pub const SHARING_LINE: usize = usize::MAX;
/// The most a screen is sent at, bits a second, and its frame rate: a still picture's detail before its motion.
const SCREEN_MAX_BITRATE: u64 = 2_500_000;
const SCREEN_FPS: f64 = 15.0;

/// The process's libwebrtc: one factory (its signaling, worker and network threads) for every call, and one idle
/// peer connection of its own kept for the process's life.
///
/// The idle peer works around the bindings' audio device: libwebrtc terminates its audio device when the last peer
/// connection goes and initializes it again with the next, and the bindings' proxy of their synthetic device (the one
/// that pumps playout every 10 ms) makes that initialization a no-op, so every call after the first would play
/// nothing.
/// While a peer of the factory's own lives, the device is never terminated. It has no ICE server and no line: it
/// gathers nothing and sends nothing.
fn factory() -> &'static PeerConnectionFactory {
    struct Factory {
        factory: PeerConnectionFactory,
        _keeper: Option<PeerConnection>,
    }
    static FACTORY: OnceLock<Factory> = OnceLock::new();
    &FACTORY
        .get_or_init(|| {
            let factory = PeerConnectionFactory::default();
            let mut config = RtcConfiguration::default();
            config.ice_transport_type = IceTransportsType::Relay;
            let keeper = factory.create_peer_connection(config).ok();
            Factory {
                factory,
                _keeper: keeper,
            }
        })
        .factory
}

unsafe extern "C" {
    /// libwebrtc's `webrtc::InitializePipeWire()`: loads libpipewire into the stubs of its functions the prebuilt
    /// carries (once; later calls answer the first's result).
    #[link_name = "_ZN6webrtc18InitializePipeWireEv"]
    fn webrtc_initialize_pipewire() -> bool;
}

/// Loads libpipewire into libwebrtc's stubs of its functions, before anything calls PipeWire: the prebuilt carries
/// weak stubs of `pw_init`, `pw_stream_new` and the rest for its screen capture, which stand in for libpipewire's own
/// in a binary linking both (the device seam's PipeWire binding then calls the stubs) and call through null pointers
/// until libwebrtc loads the library. Whether it loaded.
#[must_use]
pub fn load_pipewire() -> bool {
    // SAFETY: a C++ function of no arguments returning `bool`, which the Itanium C++ ABI passes as C does; it only
    // dlopens libpipewire and fills the stubs' pointers, once, behind a function-local static libwebrtc guards
    unsafe { webrtc_initialize_pipewire() }
}

/// Starts the process's libwebrtc now (its factory's threads take most of a second to start, once) rather than when
/// the first call's media is set up.
pub fn warm_up() {
    let _ = factory();
}

/// The runtime libwebrtc's video source keeps its track alive on, until the camera's first frame.
fn runtime() -> Result<&'static tokio::runtime::Runtime, Error> {
    static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .thread_name("teams-media-rt")
                .enable_time()
                .build()
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(|error| Error::Rtc(format!("the media's runtime: {error}")))
}

/// What waiting on one of libwebrtc's streams came to.
pub(crate) enum Next<T> {
    /// Its next item.
    Item(T),
    /// Nothing came within the wait.
    Quiet,
    /// The stream ended.
    Ended,
}

/// The stream's next item within `wait`, waited for on the media's runtime (libwebrtc's streams wake their reader from
/// its threads; the timer is the runtime's).
///
/// # Errors
/// [`Error::Rtc`] when the media's runtime can't start.
pub(crate) fn next_within<S>(stream: &mut S, wait: Duration) -> Result<Next<S::Item>, Error>
where
    S: futures_util::Stream + Unpin,
{
    use futures_util::StreamExt as _;
    let next = runtime()?.block_on(async { tokio::time::timeout(wait, stream.next()).await });
    Ok(match next {
        Ok(Some(item)) => Next::Item(item),
        Ok(None) => Next::Ended,
        Err(_) => Next::Quiet,
    })
}

/// An SDP offer in libwebrtc's standard form.
#[derive(Clone, PartialEq, Eq)]
pub struct SdpOffer(String);

/// An SDP answer in libwebrtc's standard form.
#[derive(Clone, PartialEq, Eq)]
pub struct SdpAnswer(String);

/// Parser text and the rejected line can contain ICE credentials or other SDP payload. Only its structural location
/// is diagnostic; even the parser's prose may quote an invalid field value.
fn sdp_parse_error(line: &str) -> String {
    let field = match line.split_once('=') {
        Some(("v", _)) => "version line",
        Some(("o", _)) => "origin line",
        Some(("s", _)) => "session name line",
        Some(("t", _)) => "timing line",
        Some(("m", _)) => "media line",
        Some(("c", _)) => "connection line",
        Some(("a", value)) => match value.split(':').next() {
            Some("ice-pwd" | "ice-ufrag") => "ICE credential attribute",
            Some("candidate") => "ICE candidate attribute",
            Some("fingerprint") => "DTLS fingerprint attribute",
            Some("rtpmap" | "fmtp") => "codec attribute",
            _ => "attribute",
        },
        _ => "session description",
    };
    format!("invalid SDP {field}")
}

macro_rules! sdp_text {
    ($name:ident, $kind:expr) => {
        impl $name {
            /// Takes standard SDP text, checked to be one libwebrtc reads.
            ///
            /// # Errors
            /// The text isn't an SDP libwebrtc reads.
            pub fn from_sdp_string(text: &str) -> Result<Self, String> {
                SessionDescription::parse(text, $kind).map_err(|error| sdp_parse_error(&error.line))?;
                Ok(Self(text.to_owned()))
            }

            /// The SDP text.
            #[must_use]
            pub fn to_sdp_string(&self) -> String {
                self.0.clone()
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            fn description(self) -> Result<SessionDescription, Error> {
                SessionDescription::parse(&self.0, $kind).map_err(|error| Error::Rtc(sdp_parse_error(&error.line)))
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                // the text holds ICE credentials and addresses: never in a log
                f.debug_struct(stringify!($name))
                    .field("media", &self.0.matches("\nm=").count())
                    .finish_non_exhaustive()
            }
        }
    };
}

sdp_text!(SdpOffer, SdpType::Offer);
sdp_text!(SdpAnswer, SdpType::Answer);

/// What the peer tells the session.
pub(crate) enum PeerEvent {
    /// ICE and DTLS are up (again).
    Connected,
    /// ICE lost the far end; it may come back.
    Disconnected,
    /// The connection failed for good before it was ever up.
    Failed(String),
    /// The far end went away once the media was up. Final.
    Closed,
    /// The far end's audio track: what the speaker plays.
    Audio(RtcAudioTrack),
    /// A video track the far end sends on, by the transceiver it came on.
    Video(RtcVideoTrack, RtpTransceiver),
    /// The data channel opened.
    ChannelOpen,
    /// A message arrived on the data channel.
    ChannelMessage(Vec<u8>),
}

/// What the peer's ICE went through.
#[derive(Default)]
struct Ice {
    connected: bool,
    /// It was connected once.
    ever: bool,
    /// Its end was told.
    ended: bool,
}

impl Ice {
    /// Its end, told once: the far end's close once it was up, a failure before.
    fn end(&mut self, why: &str) -> PeerEvent {
        if std::mem::replace(&mut self.ended, true) {
            return PeerEvent::Disconnected;
        }
        if self.ever {
            PeerEvent::Closed
        } else {
            PeerEvent::Failed(why.to_owned())
        }
    }
}

/// Where the peer's events go: the session's (which drops them once it is gone), from libwebrtc's threads.
pub(crate) type Events = Arc<dyn Fn(PeerEvent) + Send + Sync>;

fn report(events: &Events, event: PeerEvent) {
    events(event);
}

/// The candidates gathered so far, and whether gathering ended.
#[derive(Default)]
struct Gathering {
    state: Mutex<Gathered>,
    changed: Condvar,
}

#[derive(Default)]
struct Gathered {
    candidates: Vec<String>,
    first: Option<Instant>,
    complete: bool,
}

impl Gathering {
    fn lock(&self) -> MutexGuard<'_, Gathered> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn add(&self, candidate: String) {
        let mut state = self.lock();
        if !state.candidates.contains(&candidate) {
            state.candidates.push(candidate);
        }
        state.first.get_or_insert_with(Instant::now);
        self.changed.notify_all();
    }

    fn set_complete(&self, complete: bool) {
        self.lock().complete = complete;
        self.changed.notify_all();
    }

    /// The candidates once gathering ended, or [`GATHER_SETTLE`] after the first one; none within [`GATHER_TIMEOUT`]
    /// is an error.
    fn wait(&self) -> Result<Vec<String>, Error> {
        let deadline = Instant::now() + GATHER_TIMEOUT;
        let mut state = self.lock();
        loop {
            let now = Instant::now();
            let settled = state.first.is_some_and(|first| now >= first + GATHER_SETTLE);
            if (state.complete || settled) && !state.candidates.is_empty() {
                return Ok(state.candidates.clone());
            }
            if state.complete || now >= deadline {
                return Err(Error::Rtc(
                    "no relayed candidate: the relay allocated nothing (credentials, or the relay out of reach)".into(),
                ));
            }
            let until = state
                .first
                .map_or(deadline, |first| (first + GATHER_SETTLE).min(deadline));
            let wait = until.saturating_duration_since(now).max(Duration::from_millis(10));
            state = self
                .changed
                .wait_timeout(state, wait)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

/// The camera's track and what sends it.
struct Video {
    source: NativeVideoSource,
    track: RtcVideoTrack,
    sender: Option<RtpSender>,
    settings: EncoderSettings,
    /// The camera is on: the main video line's encoding is active.
    sending: bool,
    /// An offer of ours was rolled back since the last negotiation: the send stream libwebrtc makes again for the line
    /// keeps its SSRC and gets no source, so the track is put on the sender again ([`Peer::negotiated`]).
    rolled_back: bool,
}

/// The call's video lines by mid, as the negotiated SDP names them: the camera's (the main one first, then the
/// receiving ones), and the sharing lines.
#[derive(Default)]
pub(crate) struct LineMap {
    video: Vec<String>,
    sharing: Vec<String>,
}

/// Our screen's track and the line it goes on, while the account shares it.
struct Screen {
    source: NativeVideoSource,
    transceiver: RtpTransceiver,
}

/// The call's peer.
pub(crate) struct Peer {
    pc: PeerConnection,
    audio: NativeAudioSource,
    audio_track: RtcAudioTrack,
    audio_attached: Mutex<bool>,
    video: Mutex<Option<Video>>,
    channel: Arc<Mutex<Option<DataChannel>>>,
    gathering: Arc<Gathering>,
    /// The video lines by mid: the main one first, then the receiving ones; and the sharing lines.
    lines: Arc<Mutex<LineMap>>,
    /// The sharing line of a group offer of ours: no line of ours.
    sharing: Mutex<Option<RtpTransceiver>>,
    /// Our screen, while it is shared.
    screen: Mutex<Option<Screen>>,
    events: Events,
}

impl Peer {
    /// A peer relayed through `relay` only, telling `events`.
    ///
    /// # Errors
    /// [`Error::Rtc`] when libwebrtc refuses the configuration.
    pub(crate) fn new(relay: &RelayConfig, events: Events) -> Result<Self, Error> {
        let mut config = RtcConfiguration::default();
        config.ice_transport_type = IceTransportsType::Relay;
        config.continual_gathering_policy = ContinualGatheringPolicy::GatherOnce;
        config.ice_servers = vec![IceServer {
            urls: relay.servers.iter().map(crate::relay::RelayServer::url).collect(),
            username: relay.credentials.username.clone(),
            password: relay.credentials.password.clone(),
        }];
        let pc = factory().create_peer_connection(config).map_err(rtc)?;
        let audio = NativeAudioSource::new(AudioSourceOptions::default(), SAMPLE_RATE, 1, 0);
        let audio_track = factory().create_audio_track("teams-media-audio", audio.clone());
        let peer = Self {
            pc,
            audio,
            audio_track,
            audio_attached: Mutex::new(false),
            video: Mutex::new(None),
            channel: Arc::new(Mutex::new(None)),
            gathering: Arc::new(Gathering::default()),
            lines: Arc::new(Mutex::new(LineMap::default())),
            sharing: Mutex::new(None),
            screen: Mutex::new(None),
            events,
        };
        peer.observe();
        Ok(peer)
    }

    /// Hands libwebrtc's callbacks to the session.
    fn observe(&self) {
        let gathering = Arc::clone(&self.gathering);
        self.pc.on_ice_candidate(Some(Box::new(move |candidate| {
            let text = candidate.candidate();
            let text = text.strip_prefix("candidate:").unwrap_or(&text).to_owned();
            gathering.add(text);
        })));
        let gathering = Arc::clone(&self.gathering);
        self.pc.on_ice_gathering_state_change(Some(Box::new(move |state| {
            gathering.set_complete(state == IceGatheringState::Complete);
        })));
        // the far end gone once the media was up (its peer closed, or its path died) is the far end's close: libwebrtc
        // tells neither a DTLS close nor an RTCP goodbye, only ICE losing it for good; before, it is a failure
        let events = self.events.clone();
        let state = Arc::new(Mutex::new(Ice::default()));
        let ice = Arc::clone(&state);
        self.pc.on_ice_connection_state_change(Some(Box::new(move |changed| {
            let mut ice = ice.lock().unwrap_or_else(PoisonError::into_inner);
            match changed {
                IceConnectionState::Connected | IceConnectionState::Completed if !ice.connected => {
                    ice.connected = true;
                    ice.ever = true;
                    report(&events, PeerEvent::Connected);
                }
                IceConnectionState::Disconnected if ice.connected => {
                    ice.connected = false;
                    report(&events, PeerEvent::Disconnected);
                }
                IceConnectionState::Failed => report(&events, ice.end("ICE failed")),
                _ => {}
            }
        })));
        let events = self.events.clone();
        self.pc.on_connection_state_change(Some(Box::new(move |changed| {
            if changed == PeerConnectionState::Failed {
                let mut ice = state.lock().unwrap_or_else(PoisonError::into_inner);
                report(&events, ice.end("the connection failed (ICE or DTLS)"));
            }
        })));
        let events = self.events.clone();
        self.pc
            .on_track(Some(Box::new(move |event: TrackEvent| match event.track {
                MediaStreamTrack::Audio(track) => report(&events, PeerEvent::Audio(track)),
                MediaStreamTrack::Video(track) => report(&events, PeerEvent::Video(track, event.transceiver)),
            })));
        // a channel the far end opens is taken only when it is the client's (a tests' mixer takes the client's in
        // place of the one it made to offer a data line)
        let (channel, events) = (Arc::clone(&self.channel), self.events.clone());
        self.pc.on_data_channel(Some(Box::new(move |opened: DataChannel| {
            if opened.label() != CHANNEL_LABEL {
                return;
            }
            let has_clients =
                |ours: &Option<DataChannel>| ours.as_ref().is_some_and(|channel| channel.label() == CHANNEL_LABEL);
            if has_clients(&channel.lock().unwrap_or_else(PoisonError::into_inner)) {
                return;
            }
            watch_channel(&opened, &events);
            let open = opened.state() == DataChannelState::Open;
            {
                let mut ours = channel.lock().unwrap_or_else(PoisonError::into_inner);
                if has_clients(&ours) {
                    return;
                }
                *ours = Some(opened);
            }
            if open {
                report(&events, PeerEvent::ChannelOpen);
            }
        })));
    }

    /// The audio source the session's capture thread hands the microphone's frames to.
    pub(crate) fn audio(&self) -> &NativeAudioSource {
        &self.audio
    }

    /// The video line of the transceiver `mid` (0 the main one, 1, 2, … a group call's receiving lines, once the SDP
    /// negotiated it; [`SHARING_LINE`] a sharing line).
    pub(crate) fn video_line(lines: &Mutex<LineMap>, mid: &str) -> Option<usize> {
        let lines = lines.lock().unwrap_or_else(PoisonError::into_inner);
        if lines.sharing.iter().any(|line| line == mid) {
            return Some(SHARING_LINE);
        }
        lines.video.iter().position(|line| line == mid)
    }

    /// The video lines by mid, shared with whoever shows what they bring.
    pub(crate) fn lines(&self) -> Arc<Mutex<LineMap>> {
        Arc::clone(&self.lines)
    }

    /// The call's sharing lines by mid, as the dialect names them: none of them is a camera's line.
    pub(crate) fn set_sharing_lines(&self, mids: Vec<String>) {
        self.lines.lock().unwrap_or_else(PoisonError::into_inner).sharing = mids;
    }

    /// Puts our screen on a line of its own, sending only (its encoder software H.264, its rate a still picture's): the
    /// source its frames go to. The line is in our next offer ([`Peer::offer`]), its mid then [`Peer::screen_mid`].
    ///
    /// # Errors
    /// [`Error::Rtc`] when the media's runtime can't start or libwebrtc refuses the line.
    pub(crate) fn share_screen(&self) -> Result<NativeVideoSource, Error> {
        let mut screen = self.screen.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(screen) = screen.as_ref() {
            return Ok(screen.source.clone());
        }
        let source = {
            let _runtime = runtime()?.enter();
            NativeVideoSource::new(VideoResolution::default(), true)
        };
        let track = factory().create_video_track("teams-media-screen", source.clone());
        let transceiver = self
            .pc
            .add_transceiver(
                MediaStreamTrack::Video(track),
                RtpTransceiverInit {
                    direction: RtpTransceiverDirection::SendOnly,
                    stream_ids: vec![SCREEN_STREAM_ID.to_owned()],
                    send_encodings: Vec::new(),
                },
            )
            .map_err(rtc)?;
        let sender = transceiver.sender();
        sender.set_video_encoder_backend(VideoEncoderBackend::Software);
        screen_encoding(&sender);
        *screen = Some(Screen {
            source: source.clone(),
            transceiver,
        });
        Ok(source)
    }

    /// The mid of our screen's line, once an offer named it.
    pub(crate) fn screen_mid(&self) -> Option<String> {
        self.screen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .and_then(|screen| screen.transceiver.mid())
    }

    /// Stops our screen's line: the next offer rejects it. Whether one was shared.
    pub(crate) fn stop_screen(&self) -> bool {
        let Some(screen) = self.screen.lock().unwrap_or_else(PoisonError::into_inner).take() else {
            return false;
        };
        if let Err(error) = screen.transceiver.stop() {
            log::debug!("the screen's line can't be stopped: {}", error.message);
        }
        true
    }

    /// Makes the camera's track (sent at most as `settings` say, only once [`Peer::set_sending`] turned it on).
    ///
    /// # Errors
    /// [`Error::Rtc`] when the media's runtime can't start.
    pub(crate) fn start_video(&self, settings: EncoderSettings) -> Result<(), Error> {
        let mut video = self.video();
        if video.is_some() {
            return Ok(());
        }
        let source = {
            let _runtime = runtime()?.enter();
            NativeVideoSource::new(VideoResolution::default(), false)
        };
        let track = factory().create_video_track("teams-media-video", source.clone());
        let _ = self.pc.set_bitrate(BitrateSettings {
            start_bitrate_bps: Some(u64::from(settings.start_bitrate)),
            ..BitrateSettings::default()
        });
        *video = Some(Video {
            source,
            track,
            sender: None,
            settings,
            sending: false,
            rolled_back: false,
        });
        Ok(())
    }

    /// The camera's source, once the video started.
    pub(crate) fn video_source(&self) -> Option<NativeVideoSource> {
        self.video().as_ref().map(|video| video.source.clone())
    }

    /// Whether the camera's track goes out (its encoding active): the camera is on.
    pub(crate) fn set_sending(&self, on: bool) {
        if let Some(video) = self.video().as_mut() {
            video.sending = on;
            apply_sending(video);
        }
    }

    fn video(&self) -> MutexGuard<'_, Option<Video>> {
        self.video.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Puts the audio track on the audio line (a new one, or the far end's offer's).
    fn attach_audio(&self) -> Result<(), Error> {
        let mut attached = self.audio_attached.lock().unwrap_or_else(PoisonError::into_inner);
        if !*attached {
            self.pc
                .add_track(MediaStreamTrack::Audio(self.audio_track.clone()), &[STREAM_ID])
                .map_err(rtc)?;
            *attached = true;
        }
        Ok(())
    }

    /// Puts the camera's track on the main video line (a new one, or the far end's offer's first), once the video
    /// started; whether it is there. It is encoded in software: the prebuilt's VA-API encoder renders no slice in a
    /// real call (`vaRenderPicture` refuses its packed slice header), so nothing of it would ever be sent.
    fn attach_video(&self) -> Result<bool, Error> {
        let mut video = self.video();
        let Some(video) = video.as_mut() else {
            return Ok(false);
        };
        if video.sender.is_none() {
            let sender = self
                .pc
                .add_track(MediaStreamTrack::Video(video.track.clone()), &[STREAM_ID])
                .map_err(rtc)?;
            sender.set_video_encoder_backend(VideoEncoderBackend::Software);
            video.sender = Some(sender);
            apply_sending(video);
        }
        Ok(true)
    }

    /// A 1:1 call's offer: its audio line (and its video line, when the camera's track is on one already).
    pub(crate) fn offer_audio(&self) -> Result<SdpOffer, Error> {
        self.attach_audio()?;
        self.offer()
    }

    /// A group call's offer, as the web client's group offer is made: audio, the main video line, `slots` receiving
    /// lines (both ways, nothing sent on them), the sharing line (received on, never shown) and the data channel.
    pub(crate) fn offer_group(&self, slots: usize) -> Result<SdpOffer, Error> {
        self.attach_audio()?;
        if !self.attach_video()? {
            self.add_video(RtpTransceiverDirection::SendRecv)?;
        }
        for _ in 0..slots {
            self.add_video(RtpTransceiverDirection::SendRecv)?;
        }
        let sharing = self.add_video(RtpTransceiverDirection::RecvOnly)?;
        *self.sharing.lock().unwrap_or_else(PoisonError::into_inner) = Some(sharing);
        self.open_channel()?;
        self.offer()
    }

    fn add_video(&self, direction: RtpTransceiverDirection) -> Result<RtpTransceiver, Error> {
        self.pc
            .add_transceiver_for_media(
                MediaType::Video,
                RtpTransceiverInit {
                    direction,
                    stream_ids: vec![STREAM_ID.to_owned()],
                    send_encodings: Vec::new(),
                },
            )
            .map_err(rtc)
    }

    /// An offer with the main video line on (added when there is none: the camera's first turn in a 1:1 call).
    pub(crate) fn offer_video(&self) -> Result<SdpOffer, Error> {
        self.attach_video()?;
        self.offer()
    }

    /// Our offer of the session as it is (a renegotiation that changes no line).
    pub(crate) fn offer(&self) -> Result<SdpOffer, Error> {
        self.prefer_codecs();
        // the bindings pass the legacy receive options as given, `false` taking receiving off every line of the kind:
        // audio is always received, video once the call takes video (a receiving line added when it has none)
        let video_lines = self
            .pc
            .transceivers()
            .iter()
            .any(|transceiver| kind_of(transceiver) == Some(MediaType::Video));
        let options = OfferOptions {
            ice_restart: false,
            offer_to_receive_audio: true,
            offer_to_receive_video: video_lines || self.video().is_some(),
        };
        let offer = block_on(self.pc.create_offer(options)).map_err(rtc)?;
        let text = offer.to_string();
        block_on(self.pc.set_local_description(offer)).map_err(rtc)?;
        Ok(SdpOffer(self.local_sdp(&text)?))
    }

    /// Answers the far end's `offer`: its audio line takes the audio track, its main video line the camera's.
    pub(crate) fn accept_offer(&self, offer: SdpOffer) -> Result<SdpAnswer, Error> {
        self.roll_back()?;
        block_on(self.pc.set_remote_description(offer.description()?)).map_err(rtc)?;
        self.attach_audio()?;
        if self.has_video_line() {
            self.attach_video()?;
        }
        self.prefer_codecs();
        let answer = block_on(self.pc.create_answer(AnswerOptions::default())).map_err(rtc)?;
        let text = answer.to_string();
        block_on(self.pc.set_local_description(answer)).map_err(rtc)?;
        self.negotiated();
        Ok(SdpAnswer(self.local_sdp(&text)?))
    }

    /// Withdraws an offer of ours still waiting for its answer, so the far end's offer can be taken: an offer of ours
    /// turned away, or one that gave way to the far end's renegotiation.
    fn roll_back(&self) -> Result<(), Error> {
        if self.pc.signaling_state() != SignalingState::HaveLocalOffer {
            return Ok(());
        }
        let rollback = SessionDescription::parse("", SdpType::Rollback)
            .map_err(|error| Error::Rtc(sdp_parse_error(&error.line)))?;
        block_on(self.pc.set_local_description(rollback)).map_err(rtc)?;
        if let Some(video) = self.video().as_mut() {
            video.rolled_back = true;
        }
        Ok(())
    }

    /// Applies the far end's answer to our offer.
    pub(crate) fn accept_answer(&self, answer: SdpAnswer) -> Result<(), Error> {
        block_on(self.pc.set_remote_description(answer.description()?)).map_err(rtc)?;
        self.negotiated();
        Ok(())
    }

    /// Whether the far end's offer has a video line in use.
    fn has_video_line(&self) -> bool {
        self.pc.transceivers().iter().any(|transceiver| {
            transceiver
                .receiver()
                .track()
                .is_some_and(|track| matches!(track, MediaStreamTrack::Video(_)))
                && transceiver.direction() != RtpTransceiverDirection::Stopped
                && transceiver.mid().is_some()
        })
    }

    /// Opens the data channel ([`CHANNEL_LABEL`]), once.
    ///
    /// The channel's lock is never held across a call into libwebrtc: its signaling thread, which those calls may wait
    /// on, takes the same lock when the far end opens a channel (`on_data_channel`).
    pub(crate) fn open_channel(&self) -> Result<(), Error> {
        if self.channel().is_some() {
            return Ok(());
        }
        let opened = self
            .pc
            .create_data_channel(CHANNEL_LABEL, DataChannelInit::default())
            .map_err(rtc)?;
        watch_channel(&opened, &self.events);
        let surplus = {
            let mut channel = self.channel.lock().unwrap_or_else(PoisonError::into_inner);
            if channel.is_none() {
                *channel = Some(opened);
                None
            } else {
                // the far end's came first and was taken meanwhile
                Some(opened)
            }
        };
        if let Some(surplus) = surplus {
            surplus.close();
        }
        Ok(())
    }

    /// The data channel, out of its lock (see [`Peer::open_channel`]).
    fn channel(&self) -> Option<DataChannel> {
        self.channel.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Whether the data channel is open.
    pub(crate) fn channel_open(&self) -> bool {
        self.channel()
            .is_some_and(|channel| channel.state() == DataChannelState::Open)
    }

    /// Sends `data` on the data channel (binary); whether it went (`false`: not open yet).
    pub(crate) fn send_channel(&self, data: &[u8]) -> bool {
        self.channel()
            .filter(|channel| channel.state() == DataChannelState::Open)
            .is_some_and(|channel| channel.send(data, true).is_ok())
    }

    /// Restricts every line's codecs to what the web client offers: Opus, and constrained baseline H.264 in
    /// packetization mode 1 with its retransmissions.
    fn prefer_codecs(&self) {
        let audio = preferred(MediaType::Audio, |codec| {
            codec.mime_type.eq_ignore_ascii_case("audio/opus")
        });
        let video = preferred(MediaType::Video, |codec| {
            let mime = codec.mime_type.to_ascii_lowercase();
            let constrained_baseline = |fmtp: &str| {
                fmtp.contains("packetization-mode=1") && fmtp.to_ascii_lowercase().contains("profile-level-id=42e0")
            };
            (mime == "video/h264" && codec.sdp_fmtp_line.as_deref().is_some_and(constrained_baseline))
                || mime == "video/rtx"
        });
        for transceiver in self.pc.transceivers() {
            if transceiver.direction() == RtpTransceiverDirection::Stopped {
                continue;
            }
            let codecs = match kind_of(&transceiver) {
                Some(MediaType::Audio) => &audio,
                Some(MediaType::Video) => &video,
                _ => continue,
            };
            if !codecs.is_empty()
                && let Err(error) = transceiver.set_codec_preferences(codecs.clone())
            {
                log::debug!("a line's codecs can't be preferred: {}", error.message);
            }
        }
    }

    /// The negotiated SDP named the lines anew: line 0 the main video line, then the receiving lines (the sharing
    /// line of a group offer of ours left out); the camera's track put on its sender again after a rollback, and its
    /// encoding set again.
    fn negotiated(&self) {
        let Some(local) = self.pc.current_local_description() else {
            return;
        };
        let sharing = self
            .sharing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .and_then(RtpTransceiver::mid);
        let screen = self.screen_mid();
        let text = local.to_string();
        let lines = {
            let mut map = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
            map.video = video_mids(&text)
                .into_iter()
                .filter(|mid| {
                    Some(mid) != sharing.as_ref() && Some(mid) != screen.as_ref() && !map.sharing.contains(mid)
                })
                .collect();
            map.video.clone()
        };
        let main_sends = lines.first().is_some_and(|mid| line_sends(&text, mid));
        if let Some(video) = self.video().as_mut() {
            // the send stream is made again once the line sends: an answer that only receives makes none
            if video.rolled_back && main_sends {
                video.rolled_back = false;
                reattach(video);
            }
            apply_sending(video);
        }
    }

    /// `text` (an offer or answer libwebrtc made) with the gathered candidates written into its first line in use.
    fn local_sdp(&self, text: &str) -> Result<String, Error> {
        let candidates = self.gathering.wait()?;
        Ok(with_candidates(text, &candidates))
    }

    /// Hangs up: the far end hears the DTLS close, the allocations are released.
    pub(crate) fn close(&self) {
        let channel = self.channel.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(channel) = channel {
            channel.close();
        }
        self.pc.close();
    }
}

/// Which kind of media a line carries.
fn kind_of(transceiver: &RtpTransceiver) -> Option<MediaType> {
    transceiver.receiver().track().map(|track| match track {
        MediaStreamTrack::Audio(_) => MediaType::Audio,
        MediaStreamTrack::Video(_) => MediaType::Video,
    })
}

/// Watches the data channel: its opening and its messages, told as events.
fn watch_channel(channel: &DataChannel, events: &Events) {
    let opened = events.clone();
    channel.on_state_change(Some(Box::new(move |state| {
        if state == DataChannelState::Open {
            report(&opened, PeerEvent::ChannelOpen);
        }
    })));
    let received = events.clone();
    channel.on_message(Some(Box::new(move |buffer| {
        report(&received, PeerEvent::ChannelMessage(buffer.data.to_vec()));
    })));
}

/// The camera's track put on its sender again (the bindings' `set_track` takes no `None`: it dereferences the track),
/// so the line's send stream takes it as its source again, and the software encoder asked for again: the sender hands
/// its encoder choice to a stream only when its SSRC changes, and the remade stream keeps the old SSRC.
fn reattach(video: &Video) {
    let Some(sender) = video.sender.as_ref() else {
        return;
    };
    let track = MediaStreamTrack::Video(video.track.clone());
    if let Err(error) = sender.set_track(Some(track)) {
        log::debug!("the camera's track can't be put on its line again: {}", error.message);
    }
    sender.set_video_encoder_backend(VideoEncoderBackend::Software);
}

/// A screen's encoding: its detail kept before its motion, at a still picture's rate.
fn screen_encoding(sender: &RtpSender) {
    let mut parameters = sender.parameters();
    for encoding in &mut parameters.encodings {
        encoding.active = true;
        encoding.max_bitrate = Some(SCREEN_MAX_BITRATE);
        encoding.max_framerate = Some(SCREEN_FPS);
    }
    parameters.set_degradation_preference(DegradationPreference::MaintainResolution);
    if let Err(error) = sender.set_parameters(parameters) {
        log::debug!("the screen's encoding can't be set: {}", error.message);
    }
}

/// The camera's encoding active as the camera is, within its bounds.
fn apply_sending(video: &mut Video) {
    let Some(sender) = video.sender.as_ref() else {
        return;
    };
    let mut parameters = sender.parameters();
    if parameters.encodings.is_empty() {
        return;
    }
    for encoding in &mut parameters.encodings {
        encoding.active = video.sending;
        encoding.max_bitrate = Some(u64::from(video.settings.max_bitrate));
        encoding.max_framerate = Some(f64::from(video.settings.fps));
    }
    // short of bandwidth or of encoder, both the frame rate and the size give a little, as Chrome's camera tracks do:
    // keeping the size alone dropped about a quarter of a movie's frames in software (a choppy picture)
    parameters.set_degradation_preference(DegradationPreference::Balanced);
    if let Err(error) = sender.set_parameters(parameters) {
        log::debug!("the camera's encoding can't be set: {}", error.message);
    }
}

/// The codecs of `kind` libwebrtc sends that `keep` takes.
fn preferred(kind: MediaType, keep: impl Fn(&RtpCodecCapability) -> bool) -> Vec<RtpCodecCapability> {
    factory()
        .get_rtp_sender_capabilities(kind)
        .codecs
        .into_iter()
        .filter(keep)
        .collect()
}

/// The mids of an SDP's video lines in use (port not 0), in line order.
fn video_mids(text: &str) -> Vec<String> {
    let mut mids = Vec::new();
    let mut in_use_video = false;
    for line in text.lines().map(|line| line.trim_end_matches('\r')) {
        if let Some(media) = line.strip_prefix("m=") {
            let mut fields = media.split(' ');
            in_use_video = fields.next() == Some("video") && fields.next() != Some("0");
        } else if in_use_video && let Some(mid) = line.strip_prefix("a=mid:") {
            mids.push(mid.to_owned());
        }
    }
    mids
}

/// Whether the line `mid` of `text` sends (`sendrecv` or `sendonly`).
fn line_sends(text: &str, mid: &str) -> bool {
    let mut this_line = false;
    let mut found = false;
    for line in text.lines().map(|line| line.trim_end_matches('\r')) {
        if line.starts_with("m=") {
            if found {
                return false;
            }
            this_line = false;
        } else if line.strip_prefix("a=mid:") == Some(mid) {
            this_line = true;
            found = true;
        } else if this_line && matches!(line, "a=sendrecv" | "a=sendonly") {
            return true;
        }
    }
    false
}

/// `text` with `candidates` (`a=candidate:` values) written into its first line in use, after its ICE credentials: the
/// bundle's lines go where it goes, and the web client writes candidates on that line alone.
fn with_candidates(text: &str, candidates: &[String]) -> String {
    let mut out = Vec::new();
    let (mut in_use, mut written) = (false, false);
    for line in text.lines().map(|line| line.trim_end_matches('\r')) {
        if line.is_empty() || line.starts_with("a=candidate:") || line == "a=end-of-candidates" {
            continue;
        }
        if let Some(media) = line.strip_prefix("m=") {
            in_use = media.split(' ').nth(1) != Some("0");
        }
        out.push(line.to_owned());
        if in_use && !written && line.starts_with("a=ice-pwd:") {
            out.extend(candidates.iter().map(|candidate| format!("a=candidate:{candidate}")));
            written = true;
        }
    }
    let mut text = out.join("\r\n");
    text.push_str("\r\n");
    text
}

fn rtc(error: libwebrtc::RtcError) -> Error {
    Error::Rtc(error.message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_go_into_the_first_line_in_use_and_video_lines_are_read_by_mid() {
        let text = "v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
                    a=ice-ufrag:u\r\na=ice-pwd:p\r\na=mid:0\r\nm=video 0 UDP/TLS/RTP/SAVPF 96\r\na=mid:1\r\n\
                    m=video 9 UDP/TLS/RTP/SAVPF 96\r\na=ice-ufrag:u\r\na=ice-pwd:p\r\na=mid:2\r\n";
        let written = with_candidates(text, &["1 1 udp 2 192.0.2.1 5000 typ relay".to_owned()]);
        assert_eq!(written.matches("a=candidate:1 1 udp").count(), 1, "{written}");
        assert_eq!(video_mids(&written), ["2"]);
    }

    #[test]
    fn a_line_sends_when_its_own_direction_says_so() {
        let text = "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=mid:0\r\na=sendrecv\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\n\
                    a=mid:1\r\na=recvonly\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\na=mid:2\r\na=sendonly\r\n";
        assert!(!line_sends(text, "1"));
        assert!(line_sends(text, "2"));
        assert!(!line_sends(text, "3"));
    }
}

#[cfg(test)]
#[path = "../tests/support/sdp_diagnostics.rs"]
mod diagnostic_tests;
