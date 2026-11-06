//! The calling service's media (M3 Task 14): with the `media` feature, once a test turned it on
//! ([`crate::ChatServiceMock::start_media`]), a loopback media peer ([`crate::media::LoopbackPeer`]) stands behind
//! every answer and offer the mock's calling service gives a client in place of a party of its own: the test call's
//! media controller and a group call's mixer answer the client's offer, a caller the mock plays offers, and either
//! renegotiates. One peer per call and account, on the loopback TURN server the mock started, playing the test's tone
//! and recording what it hears to a file of its own. Without the feature (or before a test turned it on) the calling
//! service answers with the fixtures' SDP, as before: nothing of it connects.
//!
//! With group video on ([`crate::ChatServiceMock::start_group_video`]) a group call's party (its caller, each callee,
//! each joiner) has a mixer of its own instead ([`crate::mixer::MixerPeer`]): it offers to a callee in the invitation,
//! answers a caller's or a joiner's offer, and speaks the control plane; each one's lines counted from its own base,
//! which the roster the mock delivers names as that party's sources.

#[cfg(not(feature = "media"))]
use serde_json::Value;

#[cfg(not(feature = "media"))]
use crate::world::State;

#[cfg(feature = "media")]
pub(crate) use on::Peers;

#[cfg(not(feature = "media"))]
#[expect(
    clippy::unused_self,
    reason = "the same methods as with the media on, where they reach the peers through the state"
)]
impl State {
    pub(crate) fn peer_answer(&mut self, _call: &str, _account: &str, _offer: &Value) -> Option<Value> {
        None
    }

    pub(crate) fn peer_offer(&mut self, _call: &str, _account: &str) -> Option<Value> {
        None
    }

    pub(crate) fn peer_reoffer(&mut self, _call: &str, _account: &str) -> Option<Value> {
        None
    }

    pub(crate) fn peer_video_offer(&mut self, _call: &str, _account: &str) -> Option<Value> {
        None
    }

    pub(crate) fn peer_share_on(&mut self, _call: &str, _account: &str) -> Option<Value> {
        None
    }

    pub(crate) fn peer_share_off(&mut self, _call: &str, _account: &str) -> Option<Value> {
        None
    }

    pub(crate) fn peer_accept(&mut self, _call: &str, _account: &str, _answer: &Value) -> bool {
        false
    }

    /// No mixer, no sources.
    pub(crate) fn mixer_sources(&self, _call: &str, _account: &str) -> Option<(i64, i64)> {
        None
    }

    /// The TURN credentials `/trap/tokens` hands out: made up, for no relay.
    pub(crate) fn relay_credentials(&self) -> (&'static str, &'static str) {
        ("mock-relay-user", "mock-relay-password")
    }
}

#[cfg(feature = "media")]
mod on {
    use std::net::SocketAddr;
    use std::path::{Path, PathBuf};

    use serde_json::{Value, json};
    use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
    use teams_media::sdp::NgcSdp;
    use teams_media::session::SessionStats;
    use teams_media::test_turn::{PASSWORD, TurnServer, USER};
    use teams_media::tone::Tone;

    use std::time::Duration;

    use crate::media::{EchoDevices, LoopbackPeer};
    use crate::mixer::{MixerPeer, MixerStats};
    use crate::world::State;

    /// The first mixer's base stream id, and how far apart two mixers' are.
    const FIRST_BASE: u32 = 400;
    const BASE_STEP: u32 = 100;

    /// The media a test turned on: its TURN server, where the peers record, the tone they play, and the peers.
    #[derive(Default)]
    pub(crate) struct Peers {
        on: Option<On>,
    }

    struct On {
        turn: TurnServer,
        dir: PathBuf,
        tone: Tone,
        /// The peers play back what they hear this much later instead of the tone (the echo service's stand-in).
        echo: Option<Duration>,
        peers: Vec<((String, String), LoopbackPeer)>,
        /// A group call's parties have mixers of their own ([`MixerPeer`]).
        group_video: bool,
        mixers: Vec<((String, String), MixerPeer)>,
        next_base: u32,
    }

    impl Peers {
        /// Turns the media on: a TURN server on loopback, the peers recording into `dir` and playing `tone`. The
        /// relay's UDP address.
        pub(crate) fn start(&mut self, dir: &Path, tone: Tone) -> std::io::Result<SocketAddr> {
            self.start_with(dir, tone, None)
        }

        /// Turns the media on as [`Self::start`] does, the peers playing back what they hear `delay` later.
        pub(crate) fn start_echo(&mut self, dir: &Path, delay: Duration) -> std::io::Result<SocketAddr> {
            let silent = Tone {
                hz: 0.0,
                amplitude: 0.0,
            };
            self.start_with(dir, silent, Some(delay))
        }

        fn start_with(&mut self, dir: &Path, tone: Tone, echo: Option<Duration>) -> std::io::Result<SocketAddr> {
            let turn = TurnServer::start()?;
            let udp = turn.udp;
            self.on = Some(On {
                turn,
                dir: dir.to_path_buf(),
                tone,
                echo,
                peers: Vec::new(),
                group_video: false,
                mixers: Vec::new(),
                next_base: FIRST_BASE,
            });
            Ok(udp)
        }

        /// Turns the media on as [`Self::start`] does, a group call's parties each with a mixer of its own.
        pub(crate) fn start_group_video(&mut self, dir: &Path, tone: Tone) -> std::io::Result<SocketAddr> {
            let udp = self.start_with(dir, tone, None)?;
            if let Some(on) = self.on.as_mut() {
                on.group_video = true;
            }
            Ok(udp)
        }

        /// Whether a group call's parties have mixers.
        pub(crate) fn group_video(&self) -> bool {
            self.on.as_ref().is_some_and(|on| on.group_video)
        }

        /// The mixer of `call` and `account`, started on its first use.
        pub(crate) fn mixer(&mut self, call: &str, account: &str) -> Option<&MixerPeer> {
            let on = self.on.as_mut()?;
            let known = on.mixers.iter().position(|(key, _)| key.0 == call && key.1 == account);
            let at = if let Some(at) = known {
                at
            } else {
                let mixer = MixerPeer::start_at(on.turn.udp, on.next_base, on.tone).ok()?;
                on.next_base += BASE_STEP;
                on.mixers.push(((call.to_owned(), account.to_owned()), mixer));
                on.mixers.len() - 1
            };
            on.mixers.get(at).map(|(_, mixer)| mixer)
        }

        /// The mixer of `call` and `account`, if it was started.
        pub(crate) fn known_mixer(&self, call: &str, account: &str) -> Option<&MixerPeer> {
            self.on
                .as_ref()?
                .mixers
                .iter()
                .find(|(key, _)| key.0 == call && key.1 == account)
                .map(|(_, mixer)| mixer)
        }

        /// What the mixer of `call` and `account` saw and did so far.
        pub(crate) fn mixer_stats(&self, call: &str, account: &str) -> Option<MixerStats> {
            self.known_mixer(call, account).map(MixerPeer::stats)
        }

        /// Where the peer of `call` and `account` records.
        pub(crate) fn recording(&self, call: &str, account: &str) -> Option<PathBuf> {
            let on = self.on.as_ref()?;
            Some(on.dir.join(format!("{call}-{}.wav", suffix(account))))
        }

        /// The peer of `call` and `account`, closed: its counts.
        pub(crate) fn close(&mut self, call: &str, account: &str) -> Option<SessionStats> {
            let on = self.on.as_mut()?;
            let at = on.peers.iter().position(|(key, _)| key.0 == call && key.1 == account)?;
            Some(on.peers.remove(at).1.close())
        }

        /// The counts so far of the peer of `call` and `account`.
        pub(crate) fn stats(&self, call: &str, account: &str) -> Option<SessionStats> {
            let on = self.on.as_ref()?;
            on.peers
                .iter()
                .find(|(key, _)| key.0 == call && key.1 == account)
                .map(|(_, peer)| peer.stats())
        }

        /// The peer of `call` and `account`, started on its first use.
        fn peer(&mut self, call: &str, account: &str) -> Option<&mut LoopbackPeer> {
            let on = self.on.as_mut()?;
            let known = on.peers.iter().position(|(key, _)| key.0 == call && key.1 == account);
            let at = if let Some(at) = known {
                at
            } else {
                let devices = FileDevices {
                    input: AudioInput::Tone(on.tone),
                    recording: Some(on.dir.join(format!("{call}-{}.wav", suffix(account)))),
                    camera: PatternCamera::default(),
                    pacing: Pacing::RealTime,
                };
                let peer = match on.echo {
                    Some(delay) => LoopbackPeer::start_at(on.turn.udp, &EchoDevices::new(delay, devices)),
                    None => LoopbackPeer::start_at(on.turn.udp, &devices),
                }
                .ok()?;
                on.peers.push(((call.to_owned(), account.to_owned()), peer));
                on.peers.len() - 1
            };
            on.peers.get_mut(at).map(|(_, peer)| peer)
        }

        fn is_on(&self) -> bool {
            self.on.is_some()
        }
    }

    /// The last part of an MRI, for a file name.
    fn suffix(account: &str) -> &str {
        account.rsplit(':').next().unwrap_or(account)
    }

    /// A `mediaContent` as the calling service hands it out.
    fn content(sdp: &NgcSdp) -> Value {
        json!({"blob": sdp.as_str(), "contentType": sdp.content_type()})
    }

    /// The SDP a `mediaContent` carries, if it is Teams' dialect.
    fn sdp(content: &Value) -> Option<NgcSdp> {
        let blob = content.get("blob")?.as_str()?;
        let content_type = content.get("contentType")?.as_str()?;
        NgcSdp::new(content_type, blob).ok()
    }

    impl State {
        /// Whether call `call` is a group call whose parties have mixers.
        fn mixed(&self, call: &str) -> bool {
            self.peers.group_video()
                && self
                    .calling
                    .calls
                    .iter()
                    .any(|known| known.id == call && known.group_chat.is_some())
        }

        /// The peer's answer to `offer` (a `mediaContent`) on `account`'s side of call `call`; `None` without media.
        pub(crate) fn peer_answer(&mut self, call: &str, account: &str, offer: &Value) -> Option<Value> {
            let offer = sdp(offer)?;
            if self.mixed(call) {
                return Some(content(&self.peers.mixer(call, account)?.answer(&offer).ok()?));
            }
            let answer = self.peers.peer(call, account)?.answer(&offer).ok()?;
            Some(content(&answer))
        }

        /// The peer's offer to `account` in call `call`; `None` without media.
        pub(crate) fn peer_offer(&mut self, call: &str, account: &str) -> Option<Value> {
            if self.mixed(call) {
                return Some(content(&self.peers.mixer(call, account)?.offer().ok()?));
            }
            Some(content(&self.peers.peer(call, account)?.offer().ok()?))
        }

        /// The audio and main video source ids of `account` in call `call`, as its mixer counts them.
        pub(crate) fn mixer_sources(&self, call: &str, account: &str) -> Option<(i64, i64)> {
            let mixer = self.peers.known_mixer(call, account)?;
            Some((mixer.audio_source(), mixer.video_source()))
        }

        /// The peer's renegotiation of its call with `account`; `None` without media or a peer.
        pub(crate) fn peer_reoffer(&mut self, call: &str, account: &str) -> Option<Value> {
            if self.mixed(call) {
                return Some(content(&self.peers.known_mixer(call, account)?.reoffer().ok()?));
            }
            self.peers.stats(call, account)?;
            Some(content(&self.peers.peer(call, account)?.reoffer().ok()?))
        }

        /// The peer's renegotiation offering its video (its camera on) to `account` in call `call`; `None` without
        /// media or a peer.
        pub(crate) fn peer_video_offer(&mut self, call: &str, account: &str) -> Option<Value> {
            self.peers.stats(call, account)?;
            Some(content(&self.peers.peer(call, account)?.video_offer().ok()?))
        }

        /// The peer's renegotiation sharing its screen with `account` in call `call`; `None` without media or a peer.
        pub(crate) fn peer_share_on(&mut self, call: &str, account: &str) -> Option<Value> {
            self.peers.stats(call, account)?;
            Some(content(&self.peers.peer(call, account)?.share_offer(true).ok()?))
        }

        /// The peer's renegotiation that stops sharing its screen; `None` without media, a peer or a share.
        pub(crate) fn peer_share_off(&mut self, call: &str, account: &str) -> Option<Value> {
            self.peers.stats(call, account)?;
            Some(content(&self.peers.peer(call, account)?.share_offer(false).ok()?))
        }

        /// The peer takes `account`'s answer to its offer; whether it did.
        pub(crate) fn peer_accept(&mut self, call: &str, account: &str, answer: &Value) -> bool {
            let Some(answer) = sdp(answer) else { return false };
            if self.mixed(call) {
                return self
                    .peers
                    .known_mixer(call, account)
                    .is_some_and(|mixer| mixer.accept_answer(&answer).is_ok());
            }
            self.peers.stats(call, account).is_some()
                && self
                    .peers
                    .peer(call, account)
                    .is_some_and(|peer| peer.accept_answer(&answer).is_ok())
        }

        /// The TURN credentials `/trap/tokens` hands out: the loopback TURN server's once the media is on.
        pub(crate) fn relay_credentials(&self) -> (&'static str, &'static str) {
            if self.peers.is_on() {
                (USER, PASSWORD)
            } else {
                ("mock-relay-user", "mock-relay-password")
            }
        }
    }
}
