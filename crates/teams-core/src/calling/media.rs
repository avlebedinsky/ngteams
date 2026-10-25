//! A call's media in the engine: set up before the account's start, join or answer goes ([`prepare`]: the relay's
//! credentials read, an allocation on the relay, the session started without its devices, our offer or our answer
//! written in Teams' dialect), its devices opened once that start, join or answer passed the fence and was taken
//! ([`open_devices`]), the far end's SDP applied and renegotiations answered or offered while it runs ([`apply`],
//! [`answer`], [`reoffer`]), the camera turned on or off with the renegotiation it takes ([`camera`]; offered again
//! once a renegotiation of ours gave way to the far end's, [`offer_camera`]), and what the session reports turned into
//! what the call does ([`LiveMedia::poll`]: a camera whose device went away or whose encoder failed is off).
//!
//! **Video** (M3 Task 18): a 1:1 call's media takes video when the engine has its codec ([`CallVideo`]): the session's
//! video starts with it (received frames to the account's sink, [`VideoOutput`]), and the dialect takes the main video
//! line of what the far end offers, receiving until the camera is on. The camera is off when a call starts; only the
//! account's action turns it on. A group call's, a join's and the test call's media carry no video (every video line
//! offered read rejected, as before), and neither does a call whose codec doesn't load (Cisco's library missing). Every
//! step that blocks (the allocation, opening a device, an SDP command, the session's close) runs on tokio's blocking
//! pool, never on the engine's thread; the session's own threads carry the media.

use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use teams_call::CallId;
use teams_media::devices::{Devices, VideoSink};
use teams_media::peer::RelayedPeer;
use teams_media::relay::{RelayOptions, RelayServer, allocate_any, teams_relay};
use teams_media::sdp::{NgcSdp, NgcSession, VideoLines};
use teams_media::session::{MediaSession, SessionConfig, SessionDevice, SessionEvent, VideoParts};
use teams_media::turn::{self, TEAMS_RELAY_HOST, TEAMS_RELAY_UDP_PORT};
use teams_media::video::codec::{EncoderSettings, H264Library};
use teams_media::video::convert::MaxSize;
use teams_proto::{Client, Credentials, HttpTransport, MediaContent, RelayTokens};

/// How long the media may take to connect once the far end's SDP was applied before the call fails.
pub const CONNECT_WITHIN: Duration = Duration::from_secs(30);
/// How long the media may stay lost (ICE disconnected) before the call fails.
pub const RECOVER_WITHIN: Duration = Duration::from_secs(15);
/// How long after the far end closed the media the call's own end may still come (a hang-up closes both) before the
/// call fails.
pub const CLOSED_GRACE: Duration = Duration::from_secs(2);

/// Where a call's media is relayed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Relay {
    /// Teams' relay host ([`TEAMS_RELAY_HOST`]), resolved when a call's media is set up: every address over UDP 3478,
    /// then over TCP 443 ([`teams_relay`]).
    Teams,
    /// These servers, in order: a test's loopback TURN server. Tests only (`test-support`).
    #[cfg(feature = "test-support")]
    Fixed(Vec<RelayServer>),
}

impl Relay {
    /// The servers to try, in order (resolving Teams' relay host blocks).
    fn servers(&self) -> Result<Vec<RelayServer>, String> {
        match self {
            Self::Teams => {
                let addresses: Vec<IpAddr> = (TEAMS_RELAY_HOST, TEAMS_RELAY_UDP_PORT)
                    .to_socket_addrs()
                    .map_err(|error| format!("the relay's host doesn't resolve: {error}"))?
                    .map(|address| address.ip())
                    .collect();
                Ok(teams_relay(&addresses))
            }
            #[cfg(feature = "test-support")]
            Self::Fixed(servers) => Ok(servers.clone()),
        }
    }
}

/// What a call's media is made of: the devices it opens (the microphone and the speaker, only once the account's
/// start, join or answer passed the fence and was taken; the camera only on the account's action), the relay it goes
/// through, how the allocation is made, how the session runs, and its video (`None`: calls without video).
#[derive(Clone)]
pub struct CallMedia {
    pub devices: Arc<dyn Devices + Send + Sync>,
    pub relay: Relay,
    pub relay_options: RelayOptions,
    pub session: SessionConfig,
    pub video: Option<CallVideo>,
}

/// A 1:1 call's video: the H.264 library it runs on, how the camera is encoded, the largest picture sent, and where
/// the frames are shown.
#[derive(Clone)]
pub struct CallVideo {
    pub library: H264Library,
    pub encoder: EncoderSettings,
    pub max_size: MaxSize,
    pub output: Arc<dyn VideoOutput>,
}

impl std::fmt::Debug for CallVideo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallVideo")
            .field("library", &self.library)
            .field("encoder", &self.encoder)
            .field("max_size", &self.max_size)
            .finish_non_exhaustive()
    }
}

/// Where a call's video is shown (one call at a time): the far end's decoded frames, and our camera's as sent.
pub trait VideoOutput: Send + Sync {
    /// The sink of the far end's frames, for a call whose video starts.
    fn remote(&self) -> Box<dyn VideoSink>;
    /// The sink of our camera's frames (a self view), or `None`.
    fn preview(&self) -> Option<Box<dyn VideoSink>>;

    /// Whether the calls may use the video codec now (the user's switch for Cisco's OpenH264): a call set up while it
    /// is off starts no video.
    fn enabled(&self) -> bool {
        true
    }
}

impl std::fmt::Debug for CallMedia {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallMedia")
            .field("relay", &self.relay)
            .field("session", &self.session)
            .field("video", &self.video)
            .finish_non_exhaustive()
    }
}

/// A call's media set up: its session, its dialect, and our SDP (an offer, or our answer to the far end's) in Teams'
/// dialect.
pub(crate) struct Prepared {
    pub(super) session: MediaSession,
    pub(super) dialect: NgcSession,
    pub(super) sdp: NgcSdp,
    /// The account's tenant, read from its token for a test call whose engine didn't know it.
    pub(super) tenant: Option<String>,
    /// The call's media takes video (its session's video started).
    pub(super) video: bool,
}

/// What a call's media is set up for: the far end's offer to answer (none for a start or a join), whether to read
/// the account's tenant first (a test call's `add` names it; the setup is refused without one), and whether the call
/// takes video (a 1:1 call's).
pub(super) struct Setup {
    pub(super) offer: Option<MediaContent>,
    pub(super) tenant: bool,
    pub(super) video: bool,
}

/// Sets up a call's media (see the module docs): our offer, or our answer to the far end's.
pub(super) async fn prepare<C: Credentials + 'static>(
    client: Arc<Client<HttpTransport<C>>>,
    media: CallMedia,
    setup: Setup,
) -> Result<Prepared, String> {
    let Setup { offer, tenant, video } = setup;
    let tenant = if tenant {
        Some(
            client
                .transport()
                .account_tenant()
                .await
                .ok_or("the account's tenant can't be read from its token")?,
        )
    } else {
        None
    };
    let mut prepared = set_up_with(&client, media, offer, video).await?;
    prepared.tenant = tenant;
    Ok(prepared)
}

/// [`prepare`]'s media part.
async fn set_up_with<C: Credentials + 'static>(
    client: &Client<HttpTransport<C>>,
    media: CallMedia,
    offer: Option<MediaContent>,
    video: bool,
) -> Result<Prepared, String> {
    let answer = client
        .read(&RelayTokens)
        .await
        .map_err(|error| format!("the relay's credentials can't be read: {error}"))?;
    let tokens = RelayTokens::read_answer(answer.body()).map_err(|error| error.to_string())?;
    let credential = tokens
        .credentials
        .into_iter()
        .next()
        .ok_or("the relay handed out no credential")?;
    let credentials = turn::Credentials {
        username: credential.username,
        password: credential.password.expose().to_owned(),
    };
    tokio::task::spawn_blocking(move || set_up(&media, &credentials, offer.as_ref(), video))
        .await
        .map_err(|error| format!("the media's setup stopped: {error}"))?
}

/// The blocking part of [`prepare`].
fn set_up(
    media: &CallMedia,
    credentials: &turn::Credentials,
    offer: Option<&MediaContent>,
    video: bool,
) -> Result<Prepared, String> {
    let text = |error: &dyn std::fmt::Display| error.to_string();
    let servers = media.relay.servers()?;
    let (link, failed) = allocate_any(&servers, credentials, &media.relay_options).map_err(|error| text(&error))?;
    for (server, why) in &failed {
        log::info!("a relay allocation over {:?} failed: {why}", server.transport);
    }
    // which way the media goes: the transport, the family, how many allocations failed first
    let server = link.server();
    let family = if server.addr.is_ipv4() { "IPv4" } else { "IPv6" };
    log::info!(
        "the call's media is relayed over {:?} ({family}), after {} failed",
        server.transport,
        failed.len()
    );
    let peer = RelayedPeer::new(Instant::now(), link).map_err(|error| text(&error))?;
    // no device opens before the call's start, join or answer passed the fence (`open_devices`)
    let session = MediaSession::start_without_devices(peer, media.session).map_err(|error| text(&error))?;
    let mut dialect = NgcSession::new();
    let video = video && start_video(&session, media.video.as_ref());
    if video {
        // the far end's main video line is taken, receiving until the camera is on
        dialect.set_video(VideoLines::Main { sending: false });
    }
    let sdp = match offer {
        None => {
            let ours = session.offer_audio().map_err(|error| text(&error))?;
            dialect.write_offer(&ours).map_err(|error| text(&error))?
        }
        Some(offer) => {
            let theirs = ngc(offer)?;
            let theirs = dialect.read_offer(&theirs).map_err(|error| text(&error))?;
            let answer = session.accept_offer(theirs).map_err(|error| text(&error))?;
            dialect.write_answer(&answer).map_err(|error| text(&error))?
        }
    };
    Ok(Prepared {
        session,
        dialect,
        sdp,
        tenant: None,
        video,
    })
}

/// Starts the session's video when the engine has a codec for it; whether it started (a library that doesn't load
/// leaves the call without video, its audio as it is).
fn start_video(session: &MediaSession, video: Option<&CallVideo>) -> bool {
    let Some(video) = video.filter(|video| video.output.enabled()) else {
        return false;
    };
    let parts = VideoParts {
        library: video.library.clone(),
        encoder: video.encoder,
        max_size: video.max_size,
        remote: video.output.remote(),
        preview: video.output.preview(),
    };
    match session.start_video(parts) {
        Ok(()) => true,
        Err(error) => {
            log::warn!("the call has no video: {error}");
            false
        }
    }
}

/// A `mediaContent` of Teams' dialect, read.
fn ngc(content: &MediaContent) -> Result<NgcSdp, String> {
    NgcSdp::new(&content.content_type, &content.blob).map_err(|error| error.to_string())
}

/// Our SDP as a call write's `mediaContent`.
pub(super) fn content(sdp: &NgcSdp) -> MediaContent {
    MediaContent::new(sdp.as_str(), sdp.content_type())
}

/// What a media step of a running call came to.
pub(crate) enum MediaDone {
    /// The far end's answer applied (or not).
    Applied { call: CallId, result: Result<(), String> },
    /// Our answer to the far end's renegotiation (or why there is none).
    Answered {
        call: CallId,
        result: Result<NgcSdp, String>,
    },
    /// The camera turned on or off, with our renegotiation's offer (none when our video line already went that way),
    /// or why it didn't.
    Camera {
        call: CallId,
        on: bool,
        result: Result<Option<NgcSdp>, String>,
        reply: super::Waiting,
    },
    /// Our renegotiation's offer (or why there is none).
    Reoffered {
        call: CallId,
        result: Result<NgcSdp, String>,
        reply: super::Waiting,
    },
    /// The call's devices opened (or not).
    Opened { call: CallId, result: Result<(), String> },
}

/// A call's running media.
pub(super) struct LiveMedia {
    pub(super) call: CallId,
    pub(super) session: Arc<MediaSession>,
    pub(super) dialect: Arc<Mutex<NgcSession>>,
    /// When the far end's SDP was applied: the media connects within [`CONNECT_WITHIN`] of it.
    applied: Option<Instant>,
    /// The session reports the media connected now.
    pub(super) connected: bool,
    /// Since when the media is lost (ICE disconnected).
    lost: Option<Instant>,
    /// When the far end closed the media.
    closed: Option<Instant>,
    /// Its devices were asked to open ([`open_devices`]).
    pub(super) devices_opened: bool,
    /// Its camera, as the account last turned it, when it takes video (a 1:1 call whose codec loaded); `None`: no
    /// video.
    pub(super) camera: Option<bool>,
    /// A renegotiation of ours gave way to the far end's: the far end's is answered with our camera's line only
    /// receiving, and our camera's line offered again once it was ([`offer_camera`]).
    pub(super) reoffer_owed: bool,
}

/// What the session's reports since the last look mean for the call.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum MediaChange {
    Connected,
    Lost,
    /// The media failed, for good: the call fails.
    Failed(String),
    /// The camera went off by itself (its device went away, or its encoder failed): nothing more is sent on our video
    /// line until the account turns it on again.
    CameraOff,
}

impl LiveMedia {
    pub(super) fn new(call: CallId, prepared: Prepared) -> Self {
        Self {
            camera: prepared.video.then_some(false),
            reoffer_owed: false,
            call,
            session: Arc::new(prepared.session),
            dialect: Arc::new(Mutex::new(prepared.dialect)),
            applied: None,
            connected: false,
            lost: None,
            closed: None,
            devices_opened: false,
        }
    }

    /// The far end's SDP was applied at `now`: the connect deadline runs from here (it already does for an answer we
    /// made, which applied theirs).
    pub(super) fn applied(&mut self, now: Instant) {
        self.applied.get_or_insert(now);
    }

    /// What the session reported since the last look, and the deadlines at `now`.
    pub(super) fn poll(&mut self, now: Instant) -> Vec<MediaChange> {
        let mut changes = Vec::new();
        while let Some(event) = self.session.next_event(Duration::ZERO) {
            match event {
                SessionEvent::Connected => {
                    self.connected = true;
                    self.lost = None;
                    changes.push(MediaChange::Connected);
                }
                SessionEvent::Disconnected => {
                    self.connected = false;
                    self.lost.get_or_insert(now);
                    changes.push(MediaChange::Lost);
                }
                SessionEvent::Closed => {
                    self.connected = false;
                    self.closed.get_or_insert(now);
                }
                SessionEvent::Failed(why) => changes.push(MediaChange::Failed(why)),
                SessionEvent::DeviceClosed(device) => {
                    log::warn!("call {}: the {device:?} went away", self.call.get());
                    if device == SessionDevice::Camera {
                        changes.extend(self.camera_stopped());
                    }
                }
                SessionEvent::VideoFailed(why) => {
                    log::warn!("call {}: its camera's video stopped: {why}", self.call.get());
                    changes.extend(self.camera_stopped());
                }
            }
        }
        if let Some(why) = self.overdue(now) {
            changes.push(MediaChange::Failed(why.to_owned()));
        }
        changes
    }

    /// The camera stopped by itself: off, told once while it was on.
    fn camera_stopped(&mut self) -> Option<MediaChange> {
        (self.camera == Some(true)).then(|| {
            self.camera = Some(false);
            MediaChange::CameraOff
        })
    }

    /// A deadline that passed at `now`, if one did.
    fn overdue(&self, now: Instant) -> Option<&'static str> {
        let past = |since: Option<Instant>, within: Duration| since.is_some_and(|since| now >= since + within);
        if past(self.closed, CLOSED_GRACE) {
            Some("the far end closed the media")
        } else if past(self.lost, RECOVER_WITHIN) {
            Some("the media was lost and didn't come back")
        } else if !self.connected && self.lost.is_none() && past(self.applied, CONNECT_WITHIN) {
            Some("the media didn't connect")
        } else {
            None
        }
    }
}

/// Opens the microphone and the speaker of `devices` on the running media, off the engine's thread.
pub(super) fn open_devices(
    live: &LiveMedia,
    devices: Arc<dyn Devices + Send + Sync>,
) -> impl FnOnce() -> MediaDone + Send + use<> {
    let (call, session) = (live.call, Arc::clone(&live.session));
    move || MediaDone::Opened {
        call,
        result: session.open_devices(&*devices).map_err(|error| error.to_string()),
    }
}

/// Applies the far end's `answer` to our offer, off the engine's thread.
pub(super) fn apply(live: &LiveMedia, answer: MediaContent) -> impl FnOnce() -> MediaDone + Send + use<> {
    let (call, session, dialect) = (live.call, Arc::clone(&live.session), Arc::clone(&live.dialect));
    move || {
        let result = ngc(&answer).and_then(|answer| {
            let answer = lock(&dialect).read_answer(&answer).map_err(|error| error.to_string())?;
            session.accept_answer(answer).map_err(|error| error.to_string())
        });
        MediaDone::Applied { call, result }
    }
}

/// Answers the far end's renegotiation `offer`, off the engine's thread: with our camera's line only receiving when
/// a renegotiation of ours gave way to it (that line is offered again next, [`offer_camera`]).
pub(super) fn answer(live: &LiveMedia, offer: MediaContent) -> impl FnOnce() -> MediaDone + Send + use<> {
    let (call, session, dialect) = (live.call, Arc::clone(&live.session), Arc::clone(&live.dialect));
    let receive_only = live.reoffer_owed;
    move || {
        let result = ngc(&offer).and_then(|offer| {
            let mut dialect = lock(&dialect);
            if receive_only && dialect.video() != VideoLines::None {
                dialect.set_video(VideoLines::Main { sending: false });
            }
            let offer = dialect.read_offer(&offer).map_err(|error| error.to_string())?;
            let answer = session.accept_offer(offer).map_err(|error| error.to_string())?;
            dialect.write_answer(&answer).map_err(|error| error.to_string())
        });
        MediaDone::Answered { call, result }
    }
}

/// Offers the running media again (an ICE restart), off the engine's thread; `reply` is answered once the machine
/// took it.
pub(super) fn reoffer(live: &LiveMedia, reply: super::Waiting) -> impl FnOnce() -> MediaDone + Send + use<> {
    let (call, session, dialect) = (live.call, Arc::clone(&live.session), Arc::clone(&live.dialect));
    move || {
        let result = session
            .reoffer()
            .map_err(|error| error.to_string())
            .and_then(|offer| lock(&dialect).write_offer(&offer).map_err(|error| error.to_string()));
        MediaDone::Reoffered { call, result, reply }
    }
}

/// Turns the camera on (opened from `devices`) or off, then offers our video line going that way, off the engine's
/// thread; `reply` is answered once the machine took the offer (or at once, with none to make).
pub(super) fn camera(
    live: &LiveMedia,
    devices: Arc<dyn Devices + Send + Sync>,
    on: bool,
    reply: super::Waiting,
) -> impl FnOnce() -> MediaDone + Send + use<> {
    let (call, session, dialect) = (live.call, Arc::clone(&live.session), Arc::clone(&live.dialect));
    move || {
        let result = turn_camera(&session, &dialect, &*devices, on);
        MediaDone::Camera {
            call,
            on,
            result,
            reply,
        }
    }
}

/// Offers our camera's line again going as the camera does (`on`, the camera left as it is), off the engine's thread:
/// once a renegotiation of ours gave way to the far end's, which was answered with that line only receiving. `reply`
/// is answered as [`camera`]'s is.
pub(super) fn offer_camera(
    live: &LiveMedia,
    on: bool,
    reply: super::Waiting,
) -> impl FnOnce() -> MediaDone + Send + use<> {
    let (call, session, dialect) = (live.call, Arc::clone(&live.session), Arc::clone(&live.dialect));
    move || MediaDone::Camera {
        call,
        on,
        result: offer_line(&session, &dialect, on),
        reply,
    }
}

/// [`camera`]'s blocking part: a camera that doesn't open, or an offer that can't be made, leaves it off.
fn turn_camera(
    session: &MediaSession,
    dialect: &Mutex<NgcSession>,
    devices: &dyn Devices,
    on: bool,
) -> Result<Option<NgcSdp>, String> {
    let source = if on {
        Some(devices.camera().map_err(|error| error.to_string())?)
    } else {
        None
    };
    session.set_camera(source).map_err(|error| error.to_string())?;
    offer_line(session, dialect, on)
}

/// Offers our video line going as the camera does (`on`); an offer that can't be made turns the camera off.
fn offer_line(session: &MediaSession, dialect: &Mutex<NgcSession>, on: bool) -> Result<Option<NgcSdp>, String> {
    let text = |error: &dyn std::fmt::Display| error.to_string();
    let mut dialect = lock(dialect);
    dialect.set_video(VideoLines::Main { sending: on });
    let offered = session.offer_video(on).map_err(|error| text(&error)).and_then(|offer| {
        offer
            .map(|offer| dialect.write_offer(&offer).map_err(|error| text(&error)))
            .transpose()
    });
    if offered.is_err() && on {
        dialect.set_video(VideoLines::Main { sending: false });
        let _ = session.set_camera(None);
    }
    offered
}

fn lock(dialect: &Mutex<NgcSession>) -> std::sync::MutexGuard<'_, NgcSession> {
    dialect.lock().unwrap_or_else(PoisonError::into_inner)
}
