//! A call's media in the engine: set up before the account's start, join or answer goes ([`prepare`]: the relay's
//! credentials read, an allocation on the relay, the session started without its devices, our offer or our answer
//! written in Teams' dialect), its devices opened once that start, join or answer passed the fence and was taken
//! ([`open_devices`]), the far end's SDP applied and renegotiations answered or offered while it runs ([`apply`],
//! [`answer`], [`reoffer`]), and what the session reports turned into what the call does ([`LiveMedia::poll`]). Every
//! step that blocks (the allocation, opening a device, an SDP command, the session's close) runs on tokio's blocking
//! pool, never on the engine's thread; the session's own threads carry the media.

use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use teams_call::CallId;
use teams_media::devices::Devices;
use teams_media::peer::RelayedPeer;
use teams_media::relay::{RelayOptions, RelayServer, allocate_any, teams_relay};
use teams_media::sdp::{NgcSdp, NgcSession};
use teams_media::session::{MediaSession, SessionConfig, SessionEvent};
use teams_media::turn::{self, TEAMS_RELAY_HOST, TEAMS_RELAY_UDP_PORT};
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
/// start, join or answer passed the fence and was taken), the relay it goes through, how the allocation is made and
/// how the session runs.
#[derive(Clone)]
pub struct CallMedia {
    pub devices: Arc<dyn Devices + Send + Sync>,
    pub relay: Relay,
    pub relay_options: RelayOptions,
    pub session: SessionConfig,
}

impl std::fmt::Debug for CallMedia {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallMedia")
            .field("relay", &self.relay)
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

/// A call's media set up: its session, its dialect, and our SDP (an offer, or our answer to the far end's) in Teams'
/// dialect.
pub(crate) struct Prepared {
    pub(super) session: MediaSession,
    pub(super) dialect: NgcSession,
    pub(super) sdp: NgcSdp,
}

/// Sets up a call's media (see the module docs): our offer, or with `offer` our answer to it.
pub(super) async fn prepare<C: Credentials + 'static>(
    client: Arc<Client<HttpTransport<C>>>,
    media: CallMedia,
    offer: Option<MediaContent>,
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
    tokio::task::spawn_blocking(move || set_up(&media, &credentials, offer.as_ref()))
        .await
        .map_err(|error| format!("the media's setup stopped: {error}"))?
}

/// The blocking part of [`prepare`].
fn set_up(
    media: &CallMedia,
    credentials: &turn::Credentials,
    offer: Option<&MediaContent>,
) -> Result<Prepared, String> {
    let text = |error: &dyn std::fmt::Display| error.to_string();
    let servers = media.relay.servers()?;
    let (link, _) = allocate_any(&servers, credentials, &media.relay_options).map_err(|error| text(&error))?;
    let peer = RelayedPeer::new(Instant::now(), link).map_err(|error| text(&error))?;
    // no device opens before the call's start, join or answer passed the fence (`open_devices`)
    let session = MediaSession::start_without_devices(peer, media.session).map_err(|error| text(&error))?;
    let mut dialect = NgcSession::new();
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
    Ok(Prepared { session, dialect, sdp })
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
}

/// What the session's reports since the last look mean for the call.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum MediaChange {
    Connected,
    Lost,
    /// The media failed, for good: the call fails.
    Failed(String),
}

impl LiveMedia {
    pub(super) fn new(call: CallId, prepared: Prepared) -> Self {
        Self {
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
                SessionEvent::DeviceClosed(device) => log::warn!("call {}: the {device:?} went away", self.call.get()),
            }
        }
        if let Some(why) = self.overdue(now) {
            changes.push(MediaChange::Failed(why.to_owned()));
        }
        changes
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

/// Answers the far end's renegotiation `offer`, off the engine's thread.
pub(super) fn answer(live: &LiveMedia, offer: MediaContent) -> impl FnOnce() -> MediaDone + Send + use<> {
    let (call, session, dialect) = (live.call, Arc::clone(&live.session), Arc::clone(&live.dialect));
    move || {
        let result = ngc(&offer).and_then(|offer| {
            let mut dialect = lock(&dialect);
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

fn lock(dialect: &Mutex<NgcSession>) -> std::sync::MutexGuard<'_, NgcSession> {
    dialect.lock().unwrap_or_else(PoisonError::into_inner)
}
