//! Teams' SDP dialect, `application/sdp-ngc-1.0`, to and from the standard SDP `str0m` speaks
//! (`docs/protocol/calling.md`, "The SDP dialect").
//!
//! The call controller and the media controller take and give offers and answers only in their own dialect (a
//! `mediaContent`'s `blob` beside its `contentType`); a group call's invitation carries the mixer's offer in an older
//! one, `application/sdp-ngc-0.5` ([`Dialect::Mixer`]), which is read the same way and never written. The web client
//! converts between the two at its `RTCPeerConnection`, and so does [`NgcSession`], line for line as the captures show
//! it:
//!
//! - **Our side, written** ([`NgcSession::write_offer`], [`NgcSession::write_answer`]): `RTP/SAVP` instead of
//!   `UDP/TLS/RTP/SAVPF`, the data channel's `m=application … webrtc-datachannel` as `m=x-data … RTP/SAVP 127 126`;
//!   the m-line's port and `c=` those of our relayed candidate, inline; `b=CT:4000`, `a=rtcp:<port>` in an offer,
//!   Teams' own `a=x-signaling-fb`, `a=x-ssrc-range` and `a=label`; the two extension URIs Teams writes with
//!   backslashes so written; a rejected line as `m=<kind> 0 RTP/SAVP …` with its label only, left out of the bundle.
//!   Only relayed candidates go out, with no related address: a host or server-reflexive candidate (a local address)
//!   is refused ([`SdpError::LocalCandidate`]).
//! - **Their side, read** ([`NgcSession::read_answer`], [`NgcSession::read_offer`]): back to the standard
//!   transports; Teams' own attributes, labels, `a=rtcp`, SDES keys (`a=crypto`, the mixer's) and every attribute
//!   `str0m` has no use for dropped; `a=rtcp-fb:*` written out per payload type (without Teams' `x-message`);
//!   backslashes in extension URIs turned back; candidates kept for component 1 over UDP only, Teams' own extensions
//!   (`MTURNID`) dropped; an `a=ssrc` (with a cname) added from `a=x-ssrc-range` or `a=ssrc-group` where the line
//!   names none, as the web client does; what a line leaves out (the mixer's lines name no `a=setup`) taken from the
//!   bundle's first line; an offer's `a=setup:actpass` (or none) is read as `passive`, so that
//!   `str0m` answers `active`, the DTLS client, as the web client does.
//! - **Mids**: `str0m` names its m-lines with mids of its own; Teams' lines are numbered (`0`, `1`, … in our offers;
//!   the mixer numbers its lines from `1` and out of line order). The session keeps the pairs for the whole call, so
//!   a renegotiation offered by either side names the same lines, and keeps each line's label; a line new to the call
//!   is numbered one past the highest number the session has.
//! - **Labels**: a line of ours takes the label the web client gives a line of its kind (`main-audio`, `main-video`,
//!   `data`), except the sharing line: the last video line of an offer with more than one, while the call has none,
//!   is `applicationsharing-video`, as in the web client's 1:1 and group offers.
//!
//! A browser's SDP (or `str0m`'s as it is) is refused where the dialect is required ([`NgcSdp::new`]).

use std::fmt;

use str0m::change::{SdpAnswer, SdpOffer};

/// The content type of Teams' SDP dialect: every offer and answer the native client sends, and what the call
/// controller and the media controller hand back.
pub const SDP_NGC: &str = "application/sdp-ngc-1.0";

/// The content type of the mixer's older dialect, which a group call's invitation carries (read only).
pub const SDP_NGC_MIXER: &str = "application/sdp-ngc-0.5";

/// The extension URIs Teams writes with backslashes for slashes (the others go as they are).
const BACKSLASHED: [&str; 2] = [
    "http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time",
    "http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01",
];

/// The attributes of an audio or video line the standard side keeps; everything else is Teams' own (or of no use to
/// `str0m`) and is dropped.
const KEPT_RTP: [&str; 21] = [
    "rtpmap",
    "fmtp",
    "rtcp-fb",
    "extmap",
    "setup",
    "mid",
    "msid",
    "sendrecv",
    "sendonly",
    "recvonly",
    "inactive",
    "ice-ufrag",
    "ice-pwd",
    "fingerprint",
    "candidate",
    "ice-options",
    "ssrc",
    "ssrc-group",
    "rtcp-mux",
    "rtcp-rsize",
    "ptime",
];

/// The attributes of a data line the standard side keeps.
const KEPT_DATA: [&str; 9] = [
    "setup",
    "mid",
    "ice-ufrag",
    "ice-pwd",
    "fingerprint",
    "candidate",
    "ice-options",
    "sctp-port",
    "max-message-size",
];

/// What attributes a line's transport takes from the bundle's first line when it leaves them out.
const INHERITED: [&str; 4] = ["ice-ufrag", "ice-pwd", "fingerprint", "setup"];

/// The SCTP port of a data channel whose line names none (the web client's choice for the media controller's).
const DEFAULT_SCTP_PORT: &str = "5000";

/// The cname given to a stream Teams names only by its SSRC.
const SYNTHESIZED_CNAME: &str = "teams-ngc";

/// The label of the screen-sharing video line: the last video line of the web client's offer.
const SHARING_LABEL: &str = "applicationsharing-video";

/// The bandwidth line of every session the web client writes.
const SESSION_BANDWIDTH: &str = "b=CT:4000";

/// Which of Teams' dialects an SDP is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    /// [`SDP_NGC`]: what the native client writes, and what the services hand back.
    Ngc,
    /// [`SDP_NGC_MIXER`]: a group call's mixer's offer in its invitation; read, never written.
    Mixer,
}

impl Dialect {
    /// The dialect a `mediaContent`'s `contentType` names.
    ///
    /// # Errors
    /// [`SdpError::ContentType`] for any other content type.
    pub fn from_content_type(content_type: &str) -> Result<Self, SdpError> {
        match content_type {
            SDP_NGC => Ok(Self::Ngc),
            SDP_NGC_MIXER => Ok(Self::Mixer),
            other => Err(SdpError::ContentType(other.to_owned())),
        }
    }

    /// The `contentType` that goes beside an SDP in this dialect.
    #[must_use]
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Ngc => SDP_NGC,
            Self::Mixer => SDP_NGC_MIXER,
        }
    }
}

/// Why an SDP can't be taken or written.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SdpError {
    /// The `contentType` names neither of Teams' dialects.
    #[error("not one of Teams' SDP dialects: content type {0:?}")]
    ContentType(String),
    /// The blob isn't an SDP in Teams' dialect (a browser's or `str0m`'s, or not an SDP at all).
    #[error("not an SDP in Teams' dialect: {0}")]
    NotNgc(&'static str),
    /// `str0m`'s SDP can't be written in Teams' dialect.
    #[error("str0m's SDP can't be written in Teams' dialect: {0}")]
    Unwritable(&'static str),
    /// An offer or answer of ours would carry a local address: only relayed candidates go out.
    #[error("only relayed candidates go out, not a {0} candidate (a local address)")]
    LocalCandidate(String),
    /// An answer names lines of an offer we never made, or our answer lines of an offer we never read.
    #[error("the m-lines don't match the offer: {0}")]
    Mismatch(&'static str),
    /// `str0m` refused the standard SDP the conversion produced.
    #[error("str0m refused the SDP: {0}")]
    Refused(String),
    /// No random source for the data line's SSRC.
    #[error("no random source: {0}")]
    Random(String),
}

/// An SDP in Teams' dialect, as a `mediaContent` carries it: checked to be one (never a browser's), with its dialect.
#[derive(Clone, PartialEq, Eq)]
pub struct NgcSdp {
    dialect: Dialect,
    text: String,
}

impl fmt::Debug for NgcSdp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // the text holds ICE credentials and addresses: never in a log
        f.debug_struct("NgcSdp")
            .field("dialect", &self.dialect)
            .field("media", &self.text.matches("\nm=").count())
            .finish_non_exhaustive()
    }
}

impl NgcSdp {
    /// Takes a `mediaContent`'s `blob` with its `contentType`.
    ///
    /// # Errors
    /// [`SdpError::ContentType`] when the content type is neither dialect's; [`SdpError::NotNgc`] when the blob
    /// isn't an SDP, has no media line, or has one that isn't Teams' (a browser's `UDP/TLS/RTP/SAVPF` or
    /// `m=application`, the placeholder port `9`, the mixer's `RTP/AVP` outside its dialect).
    pub fn new(content_type: &str, blob: &str) -> Result<Self, SdpError> {
        let dialect = Dialect::from_content_type(content_type)?;
        let parsed = Parsed::ngc(blob, dialect)?;
        if parsed.media.is_empty() {
            return Err(SdpError::NotNgc("no media line"));
        }
        Ok(Self {
            dialect,
            text: blob.to_owned(),
        })
    }

    /// Its dialect.
    #[must_use]
    pub fn dialect(&self) -> Dialect {
        self.dialect
    }

    /// The `contentType` that goes beside it.
    #[must_use]
    pub fn content_type(&self) -> &'static str {
        self.dialect.content_type()
    }

    /// The SDP text: the `blob`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }
}

/// A media line's kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Audio,
    Video,
    Data,
}

impl Kind {
    fn ngc(self) -> &'static str {
        match self {
            Self::Audio => "audio",
            Self::Video => "video",
            Self::Data => "x-data",
        }
    }

    fn standard(self) -> &'static str {
        match self {
            Self::Audio => "audio",
            Self::Video => "video",
            Self::Data => "application",
        }
    }

    /// The label the web client gives a line of the kind (a video line other than the sharing one,
    /// [`SHARING_LABEL`]).
    fn label(self) -> &'static str {
        match self {
            Self::Audio => "main-audio",
            Self::Video => "main-video",
            Self::Data => "data",
        }
    }
}

/// One media section: its m-line and the lines under it (`c=`, `b=`, `a=`).
#[derive(Clone, Debug)]
struct Section {
    kind: Kind,
    port: u16,
    formats: Vec<String>,
    lines: Vec<String>,
}

impl Section {
    /// The values of every `a=<name>:<value>` line.
    fn values<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.lines.iter().filter_map(move |line| attribute(line, name))
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.lines.iter().find_map(|line| attribute(line, name))
    }

    fn has(&self, name: &str) -> bool {
        self.lines.iter().any(|line| attribute_name(line) == Some(name))
    }

    fn mid(&self) -> Option<&str> {
        self.value("mid")
    }

    fn rejected(&self) -> bool {
        self.port == 0
    }

    fn connection(&self) -> Option<&str> {
        self.lines
            .iter()
            .find(|line| line.starts_with("c="))
            .map(String::as_str)
    }

    fn direction(&self) -> Option<&'static str> {
        ["sendrecv", "sendonly", "recvonly", "inactive"]
            .into_iter()
            .find(|direction| self.has(direction))
    }
}

/// An SDP split into its session part and its media sections.
#[derive(Clone, Debug)]
struct Parsed {
    session: Vec<String>,
    media: Vec<Section>,
}

/// An m-line split into its fields.
struct MediaLine<'a> {
    media: &'a str,
    port: &'a str,
    proto: &'a str,
    formats: Vec<String>,
}

fn media_line(line: &str) -> Option<MediaLine<'_>> {
    let mut fields = line.strip_prefix("m=")?.split(' ');
    let media = fields.next()?;
    let port = fields.next()?;
    let proto = fields.next()?;
    Some(MediaLine {
        media,
        port,
        proto,
        formats: fields.map(str::to_owned).collect(),
    })
}

/// The SDP's lines, CRLF or LF, without empty ones.
fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.split('\n')
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| !line.is_empty())
}

impl Parsed {
    /// Splits `text` into sections, each m-line read by `read`.
    fn split(text: &str, read: impl Fn(&MediaLine<'_>) -> Result<(Kind, u16), SdpError>) -> Result<Self, SdpError> {
        let mut all = lines(text);
        if all.next() != Some("v=0") {
            return Err(SdpError::NotNgc("no session part"));
        }
        let mut session = vec!["v=0".to_owned()];
        let mut media: Vec<Section> = Vec::new();
        for line in all {
            if let Some(fields) = media_line(line) {
                let (kind, port) = read(&fields)?;
                media.push(Section {
                    kind,
                    port,
                    formats: fields.formats,
                    lines: Vec::new(),
                });
            } else if let Some(section) = media.last_mut() {
                section.lines.push(line.to_owned());
            } else {
                session.push(line.to_owned());
            }
        }
        Ok(Self { session, media })
    }

    /// Reads an SDP in Teams' `dialect`.
    fn ngc(text: &str, dialect: Dialect) -> Result<Self, SdpError> {
        Self::split(text, |fields| {
            let proto_ok = fields.proto == "RTP/SAVP" || (dialect == Dialect::Mixer && fields.proto == "RTP/AVP");
            if !proto_ok {
                return Err(SdpError::NotNgc("a media line isn't on RTP/SAVP (a browser's SDP?)"));
            }
            let kind = match fields.media {
                "audio" => Kind::Audio,
                "video" => Kind::Video,
                "x-data" => Kind::Data,
                _ => return Err(SdpError::NotNgc("a media line of an unknown kind")),
            };
            let port = fields
                .port
                .parse::<u16>()
                .map_err(|_| SdpError::NotNgc("a media line's port isn't a number"))?;
            if port == 9 {
                return Err(SdpError::NotNgc(
                    "a media line on the placeholder port 9 (a browser's SDP?)",
                ));
            }
            Ok((kind, port))
        })
    }

    /// Reads `str0m`'s standard SDP.
    fn standard(text: &str) -> Result<Self, SdpError> {
        Self::split(text, |fields| {
            let kind = match (fields.media, fields.proto) {
                ("audio", "UDP/TLS/RTP/SAVPF") => Kind::Audio,
                ("video", "UDP/TLS/RTP/SAVPF") => Kind::Video,
                ("application", "UDP/DTLS/SCTP" | "DTLS/SCTP") => Kind::Data,
                _ => {
                    return Err(SdpError::Unwritable(
                        "a media line of a kind Teams' dialect has no form for",
                    ));
                }
            };
            let port = fields
                .port
                .parse::<u16>()
                .map_err(|_| SdpError::Unwritable("a media line's port isn't a number"))?;
            Ok((kind, port))
        })
    }

    fn session_value(&self, name: &str) -> Option<&str> {
        self.session.iter().find_map(|line| attribute(line, name))
    }

    /// The mids of the `a=group:BUNDLE` line.
    fn bundle(&self) -> Vec<&str> {
        self.session_value("group")
            .and_then(|group| group.strip_prefix("BUNDLE "))
            .map(|mids| mids.split(' ').collect())
            .unwrap_or_default()
    }
}

/// The value of `line` when it is `a=<name>:<value>`.
fn attribute<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    line.strip_prefix("a=")?.strip_prefix(name)?.strip_prefix(':')
}

/// The name of an `a=` line.
fn attribute_name(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("a=")?;
    Some(rest.split_once(':').map_or(rest, |(name, _)| name))
}

/// One m-line of a call in both namings: `str0m`'s mid, Teams' mid, and the line's label.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Line {
    local: String,
    remote: String,
    label: String,
}

/// One call's media session as Teams' dialect sees it: the pairs of `str0m`'s and Teams' mids for every m-line and
/// each line's label, kept for the call's life so that a renegotiation, offered by either side, names the same
/// lines. On the caller's side our offer numbers the lines; on the callee's side the offer it reads does.
#[derive(Clone, Debug, Default)]
pub struct NgcSession {
    lines: Vec<Line>,
}

/// Which half of an exchange an SDP is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Offer,
    Answer,
}

impl NgcSession {
    /// A session with no m-line yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn by_local(&self, mid: &str) -> Option<&Line> {
        self.lines.iter().find(|line| line.local == mid)
    }

    fn by_remote(&self, mid: &str) -> Option<&Line> {
        self.lines.iter().find(|line| line.remote == mid)
    }

    /// The Teams mid of a line new to the call: one past the highest number the session has. Neither the count of
    /// lines nor a line's place will do: the mixer numbers its lines from `1` and out of line order.
    fn next_remote(&self) -> String {
        self.lines
            .iter()
            .filter_map(|line| line.remote.parse::<u32>().ok())
            .max()
            .map_or(0, |highest| highest + 1)
            .to_string()
    }

    /// The place among `parsed`'s sections of the line that becomes the sharing line when `str0m` adds it: the last
    /// video line of an offer with more than one, while the call has no sharing line yet.
    fn sharing_place(&self, parsed: &Parsed) -> Option<usize> {
        if self.lines.iter().any(|line| line.label == SHARING_LABEL) {
            return None;
        }
        let video: Vec<usize> = parsed
            .media
            .iter()
            .enumerate()
            .filter(|(_, section)| section.kind == Kind::Video)
            .map(|(place, _)| place)
            .collect();
        if video.len() > 1 { video.last().copied() } else { None }
    }

    /// Our offer (`str0m`'s, at a call's start or in a renegotiation) in Teams' dialect, for the start's, a join's or
    /// a `mediaNegotiation`'s `mediaContent`. A line `str0m` adds is numbered one past the highest number the session
    /// has, and labelled as the web client labels a line of its kind (the last video line of an offer with more than
    /// one, while the call has no sharing line, is the sharing line).
    ///
    /// # Errors
    /// [`SdpError::LocalCandidate`] when the offer carries a candidate that isn't relayed, [`SdpError::Unwritable`]
    /// when a line has no relayed candidate, no mid, or a kind Teams' dialect has no form for, [`SdpError::Random`]
    /// when the data line's SSRC can't be drawn.
    pub fn write_offer(&mut self, offer: &SdpOffer) -> Result<NgcSdp, SdpError> {
        let parsed = Parsed::standard(&offer.to_sdp_string())?;
        let sharing = self.sharing_place(&parsed);
        let mut names = Vec::new();
        for (place, section) in parsed.media.iter().enumerate() {
            let local = section.mid().ok_or(SdpError::Unwritable("an m-line without a mid"))?;
            if self.by_local(local).is_none() {
                let label = if sharing == Some(place) {
                    SHARING_LABEL
                } else {
                    section.kind.label()
                };
                self.lines.push(Line {
                    local: local.to_owned(),
                    remote: self.next_remote(),
                    label: label.to_owned(),
                });
            }
            names.push(
                self.by_local(local)
                    .cloned()
                    .ok_or(SdpError::Unwritable("an m-line without a mid"))?,
            );
        }
        write(&parsed, &names, Role::Offer)
    }

    /// The answer to our offer (the media controller's, or a callee's in a 1:1 call), for `str0m`. A line of an offer
    /// we never made (the media controller's `m=x-data` to an offer without one) is left out.
    ///
    /// # Errors
    /// [`SdpError::Mismatch`] when no line of the answer is one of ours, [`SdpError::Refused`] when `str0m` refuses
    /// what the conversion made.
    pub fn read_answer(&self, answer: &NgcSdp) -> Result<SdpAnswer, SdpError> {
        let parsed = Parsed::ngc(answer.as_str(), answer.dialect())?;
        let mut kept = Vec::new();
        for (index, section) in parsed.media.iter().enumerate() {
            let line = match section.mid() {
                Some(mid) => self.by_remote(mid),
                // a rejected line names no mid: it is the offer's line at its place
                None if section.rejected() => self.lines.get(index),
                None => None,
            };
            if let Some(line) = line {
                kept.push((section, line.local.clone()));
            }
        }
        if kept.is_empty() {
            return Err(SdpError::Mismatch("the answer has none of the offer's m-lines"));
        }
        let text = read(&parsed, &kept, Role::Answer, |mid| {
            self.by_remote(mid).map(|line| line.local.clone())
        });
        SdpAnswer::from_sdp_string(&text).map_err(|error| SdpError::Refused(error.to_string()))
    }

    /// An offer we received (the caller's or the mixer's in an invitation, or a renegotiation), for `str0m`. Lines the
    /// session knows keep their `str0m` mids; a new line keeps Teams' mid on our side too. A rejected line that names
    /// no mid is the session's line at its place (or, new to the call, numbered as a line we add).
    ///
    /// # Errors
    /// [`SdpError::NotNgc`] when a line that isn't rejected names no mid, or one too long for `str0m`;
    /// [`SdpError::Refused`] when `str0m` refuses what the conversion made.
    pub fn read_offer(&mut self, offer: &NgcSdp) -> Result<SdpOffer, SdpError> {
        let parsed = Parsed::ngc(offer.as_str(), offer.dialect())?;
        let mut kept = Vec::new();
        for (index, section) in parsed.media.iter().enumerate() {
            let remote = match section.mid() {
                Some(mid) => mid.to_owned(),
                // the m-lines keep their order for the call: a rejected line is the one at its place
                None if section.rejected() => self
                    .lines
                    .get(index)
                    .map_or_else(|| self.next_remote(), |line| line.remote.clone()),
                None => return Err(SdpError::NotNgc("an offered m-line without a mid")),
            };
            if remote.len() > 16 {
                return Err(SdpError::NotNgc("a mid longer than 16 characters"));
            }
            if self.by_remote(&remote).is_none() {
                self.lines.push(Line {
                    local: remote.clone(),
                    remote: remote.clone(),
                    label: section.value("label").unwrap_or(section.kind.label()).to_owned(),
                });
            }
            let local = self.by_remote(&remote).map(|line| line.local.clone()).unwrap_or(remote);
            kept.push((section, local));
        }
        let text = read(&parsed, &kept, Role::Offer, |mid| {
            self.by_remote(mid).map(|line| line.local.clone())
        });
        SdpOffer::from_sdp_string(&text).map_err(|error| SdpError::Refused(error.to_string()))
    }

    /// Our answer (`str0m`'s) to an offer [`NgcSession::read_offer`] read, in Teams' dialect, for the `accept`'s or a
    /// `mediaAnswer`'s `mediaContent`.
    ///
    /// # Errors
    /// [`SdpError::Mismatch`] when the answer has a line the offer didn't, [`SdpError::LocalCandidate`] and
    /// [`SdpError::Unwritable`] as for [`NgcSession::write_offer`].
    pub fn write_answer(&self, answer: &SdpAnswer) -> Result<NgcSdp, SdpError> {
        let parsed = Parsed::standard(&answer.to_sdp_string())?;
        let names = parsed
            .media
            .iter()
            .map(|section| {
                section
                    .mid()
                    .and_then(|mid| self.by_local(mid))
                    .cloned()
                    .ok_or(SdpError::Mismatch("the answer has an m-line the offer didn't"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        write(&parsed, &names, Role::Answer)
    }
}

// ---- standard → Teams' dialect ----

/// Writes `parsed` (`str0m`'s) in Teams' dialect, each section named by its entry of `names`.
fn write(parsed: &Parsed, names: &[Line], role: Role) -> Result<NgcSdp, SdpError> {
    let mut foundations = Vec::new();
    let candidates = parsed
        .media
        .iter()
        .map(|section| relayed_candidates(section, &mut foundations))
        .collect::<Result<Vec<_>, _>>()?;
    // a bundled line carries no candidate of its own: it goes where the first one does, as the web client writes it
    let bundle_default = candidates
        .iter()
        .find_map(|ours| ours.first.clone())
        .ok_or(SdpError::Unwritable("no relayed candidate"))?;
    let mut out = Vec::new();
    write_session(parsed, names, &mut out)?;
    for ((section, name), ours) in parsed.media.iter().zip(names).zip(candidates) {
        if section.rejected() {
            out.push(format!(
                "m={} 0 RTP/SAVP {}",
                section.kind.ngc(),
                section.formats.join(" ")
            ));
            out.push(format!("a=label:{}", name.label));
        } else {
            let at = ours.first.unwrap_or_else(|| bundle_default.clone());
            let written = Written {
                candidates: ours.lines,
                at,
            };
            write_section(section, name, role, written, &mut out)?;
        }
    }
    let mut text = out.join("\r\n");
    text.push_str("\r\n");
    NgcSdp::new(SDP_NGC, &text)
}

/// A section's candidates as Teams writes them, and the address and port its m-line and `c=` go to.
struct Written {
    candidates: Vec<String>,
    at: (String, String),
}

fn write_session(parsed: &Parsed, names: &[Line], out: &mut Vec<String>) -> Result<(), SdpError> {
    let origin = parsed
        .session
        .iter()
        .find_map(|line| line.strip_prefix("o="))
        .ok_or(SdpError::Unwritable("no origin line"))?;
    let fields: Vec<&str> = origin.split(' ').collect();
    let (Some(id), Some(version)) = (fields.get(1), fields.get(2)) else {
        return Err(SdpError::Unwritable("an origin line without a session id"));
    };
    out.extend([
        "v=0".to_owned(),
        format!("o=- {id} {version} IN IP4 127.0.0.1"),
        "s=-".to_owned(),
        SESSION_BANDWIDTH.to_owned(),
        "t=0 0".to_owned(),
    ]);
    if parsed.session.iter().any(|line| line == "a=extmap-allow-mixed") {
        out.push("a=extmap-allow-mixed".to_owned());
    }
    out.push("a=msid-semantic: WMS *".to_owned());
    let bundled: Vec<&str> = parsed
        .media
        .iter()
        .zip(names)
        .filter(|(section, _)| !section.rejected())
        .map(|(_, name)| name.remote.as_str())
        .collect();
    if !bundled.is_empty() {
        out.push(format!("a=group:BUNDLE {}", bundled.join(" ")));
    }
    Ok(())
}

/// A section's relayed candidates in Teams' form, and the address and port of the first, if it has any.
struct Relayed {
    lines: Vec<String>,
    first: Option<(String, String)>,
}

/// Our relayed candidates of a section, rewritten as Teams' candidates (numbered foundations, no related address,
/// no extension), with the address and port of the first, if it has any.
fn relayed_candidates(section: &Section, foundations: &mut Vec<String>) -> Result<Relayed, SdpError> {
    let mut written = Vec::new();
    let mut default = None;
    for candidate in section.values("candidate") {
        let fields: Vec<&str> = candidate.split(' ').collect();
        let [
            foundation,
            component,
            transport,
            priority,
            address,
            port,
            "typ",
            kind,
            ..,
        ] = fields[..]
        else {
            return Err(SdpError::Unwritable("a candidate that doesn't read"));
        };
        if kind != "relay" {
            return Err(SdpError::LocalCandidate(kind.to_owned()));
        }
        if component != "1" {
            continue;
        }
        if !foundations.iter().any(|known| known == foundation) {
            foundations.push(foundation.to_owned());
        }
        let number = foundations
            .iter()
            .position(|known| known == foundation)
            .unwrap_or_default()
            + 1;
        written.push(format!(
            "a=candidate:{number} 1 {transport} {priority} {address} {port} typ relay raddr 0.0.0.0 rport 0"
        ));
        default.get_or_insert((address.to_owned(), port.to_owned()));
    }
    Ok(Relayed {
        lines: written,
        first: default,
    })
}

fn write_section(
    section: &Section,
    name: &Line,
    role: Role,
    written: Written,
    out: &mut Vec<String>,
) -> Result<(), SdpError> {
    let Written {
        candidates,
        at: (address, port),
    } = written;
    let family = if address.contains(':') { "IP6" } else { "IP4" };
    let formats = match section.kind {
        Kind::Data => "127 126".to_owned(),
        Kind::Audio | Kind::Video => section.formats.join(" "),
    };
    out.push(format!("m={} {port} RTP/SAVP {formats}", section.kind.ngc()));
    out.push(format!("c=IN {family} {address}"));
    if section.kind == Kind::Data {
        write_data_payload(out)?;
    } else {
        write_rtp_payload(section, &port, role, out);
    }
    write_transport(section, name, candidates, out);
    if section.kind == Kind::Data {
        // the web client writes it on its data line too
        out.push("a=rtcp-mux".to_owned());
    }
    for kept in [["ssrc", "ssrc-group"].as_slice(), &["rtcp-mux", "rtcp-rsize"]] {
        out.extend(
            section
                .lines
                .iter()
                .filter(|line| attribute_name(line).is_some_and(|name| kept.contains(&name)))
                .cloned(),
        );
    }
    out.push(format!("a=label:{}", name.label));
    if section.kind == Kind::Data {
        out.push(format!(
            "a=sctp-port:{}",
            section.value("sctp-port").unwrap_or(DEFAULT_SCTP_PORT)
        ));
        if let Some(size) = section.value("max-message-size") {
            out.push(format!("a=max-message-size:{size}"));
        }
    }
    Ok(())
}

/// The payload lines of an audio or video section, from Teams' feedback attribute to the extensions.
fn write_rtp_payload(section: &Section, port: &str, role: Role, out: &mut Vec<String>) {
    out.push(match section.kind {
        Kind::Audio => "a=x-signaling-fb:* x-message app recv:dsh".to_owned(),
        Kind::Video | Kind::Data => "a=x-signaling-fb:* x-message app send:src recv:src,vc".to_owned(),
    });
    if let Some(ssrc) = section.values("ssrc").next().and_then(|ssrc| ssrc.split(' ').next()) {
        out.push(format!("a=x-ssrc-range:{ssrc}-{ssrc}"));
    }
    for name in ["rtpmap", "fmtp"] {
        out.extend(section.values(name).map(|value| format!("a={name}:{value}")));
    }
    if role == Role::Offer {
        out.push(format!("a=rtcp:{port}"));
    }
    out.extend(section.values("rtcp-fb").map(|value| format!("a=rtcp-fb:{value}")));
    out.extend(
        section
            .values("extmap")
            .map(|value| format!("a=extmap:{}", backslashed(value))),
    );
}

/// The payload lines of a data section, as the web client writes them (an SSRC of its own, drawn at random).
fn write_data_payload(out: &mut Vec<String>) -> Result<(), SdpError> {
    let mut bytes = [0; 4];
    getrandom::fill(&mut bytes).map_err(|error| SdpError::Random(error.to_string()))?;
    let ssrc = (u32::from_be_bytes(bytes) & 0x7fff_ffff).max(1);
    out.extend([
        "a=x-data-protocol:sctp".to_owned(),
        format!("a=x-ssrc-range:{ssrc}-{ssrc}"),
        "a=rtpmap:127 x-data/90000".to_owned(),
        "a=rtpmap:126 rtx/90000".to_owned(),
        "a=fmtp:126 apt=127".to_owned(),
    ]);
    Ok(())
}

/// The transport lines: setup, mid, stream, direction, ICE and DTLS, the candidates.
fn write_transport(section: &Section, name: &Line, candidates: Vec<String>, out: &mut Vec<String>) {
    if let Some(setup) = section.value("setup") {
        out.push(format!("a=setup:{setup}"));
    }
    out.push(format!("a=mid:{}", name.remote));
    if let Some(msid) = section.value("msid") {
        out.push(format!("a=msid:{msid}"));
    }
    out.push(format!("a={}", section.direction().unwrap_or("sendrecv")));
    for name in ["ice-ufrag", "ice-pwd", "fingerprint"] {
        if let Some(value) = section.value(name) {
            out.push(format!("a={name}:{value}"));
        }
    }
    out.extend(candidates);
    if let Some(options) = section.value("ice-options") {
        out.push(format!("a=ice-options:{options}"));
    }
}

/// An `a=extmap` value with the URI written as Teams writes it.
fn backslashed(value: &str) -> String {
    match value.split_once(' ') {
        Some((id, uri)) if BACKSLASHED.contains(&uri) => format!("{id} {}", uri.replace('/', "\\")),
        _ => value.to_owned(),
    }
}

// ---- Teams' dialect → standard ----

/// Writes the `kept` sections of `parsed` (Teams') as standard SDP, each named by its `str0m` mid; `local` maps a
/// Teams mid of the bundle to ours.
fn read(parsed: &Parsed, kept: &[(&Section, String)], role: Role, local: impl Fn(&str) -> Option<String>) -> String {
    let mut out = read_session(parsed, &local);
    let first = parsed
        .bundle()
        .first()
        .and_then(|mid| parsed.media.iter().find(|section| section.mid() == Some(mid)))
        .or_else(|| parsed.media.iter().find(|section| !section.rejected()));
    for (section, mid) in kept {
        if section.rejected() {
            read_rejected(section, mid, &mut out);
        } else {
            read_section(parsed, section, first, mid, role, &mut out);
        }
    }
    let mut text = out.join("\r\n");
    text.push_str("\r\n");
    text
}

fn read_session(parsed: &Parsed, local: &impl Fn(&str) -> Option<String>) -> Vec<String> {
    let mut out = vec!["v=0".to_owned()];
    out.extend(parsed.session.iter().find(|line| line.starts_with("o=")).cloned());
    out.extend(["s=-".to_owned(), "t=0 0".to_owned()]);
    let bundled: Vec<String> = parsed.bundle().into_iter().filter_map(local).collect();
    if !bundled.is_empty() {
        out.push(format!("a=group:BUNDLE {}", bundled.join(" ")));
    }
    if parsed.session.iter().any(|line| line == "a=extmap-allow-mixed") {
        out.push("a=extmap-allow-mixed".to_owned());
    }
    out
}

fn read_rejected(section: &Section, mid: &str, out: &mut Vec<String>) {
    out.push(match section.kind {
        Kind::Data => "m=application 0 UDP/DTLS/SCTP webrtc-datachannel".to_owned(),
        // payload type 0 needs no rtpmap, as the web client writes a rejected line
        Kind::Audio | Kind::Video => format!("m={} 0 UDP/TLS/RTP/SAVPF 0", section.kind.standard()),
    });
    out.push("c=IN IP4 0.0.0.0".to_owned());
    out.push(format!("a=mid:{mid}"));
    out.push("a=inactive".to_owned());
}

fn read_section(
    parsed: &Parsed,
    section: &Section,
    first: Option<&Section>,
    mid: &str,
    role: Role,
    out: &mut Vec<String>,
) {
    out.push(match section.kind {
        Kind::Data => format!("m=application {} UDP/DTLS/SCTP webrtc-datachannel", section.port),
        Kind::Audio | Kind::Video => format!(
            "m={} {} UDP/TLS/RTP/SAVPF {}",
            section.kind.standard(),
            section.port,
            section.formats.join(" ")
        ),
    });
    let connection = section
        .connection()
        .or_else(|| first.and_then(Section::connection))
        .or_else(|| {
            parsed
                .session
                .iter()
                .find(|line| line.starts_with("c="))
                .map(String::as_str)
        })
        .unwrap_or("c=IN IP4 0.0.0.0");
    out.push(connection.to_owned());
    out.extend(section.lines.iter().filter(|line| line.starts_with("b=AS:")).cloned());
    for name in INHERITED {
        let value = section
            .value(name)
            .or_else(|| first.and_then(|first| first.value(name)));
        match (name, value, role) {
            // an offer leaving the DTLS role to us (`actpass`, or no setup: the mixer's) is read as `passive`, so
            // that `str0m` takes the client's part as the web client does (RFC 5763 asks it of an answerer); its own
            // choice for `actpass` would be the server's
            ("setup", None | Some("actpass"), Role::Offer) => out.push("a=setup:passive".to_owned()),
            (_, Some(value), _) => out.push(format!("a={name}:{value}")),
            (_, None, _) => {}
        }
    }
    out.push(format!("a=mid:{mid}"));
    out.extend(section.values("candidate").filter_map(standard_candidate));
    let kept: &[&str] = if section.kind == Kind::Data {
        &KEPT_DATA
    } else {
        &KEPT_RTP
    };
    for line in &section.lines {
        let Some(name) = attribute_name(line) else { continue };
        if !kept.contains(&name) || INHERITED.contains(&name) || ["mid", "candidate"].contains(&name) {
            continue;
        }
        out.extend(standard_attribute(section, name, line));
    }
    if section.kind != Kind::Data && section.direction().is_none() {
        out.push("a=sendrecv".to_owned());
    }
    if section.kind == Kind::Data && !section.has("sctp-port") {
        out.push(format!("a=sctp-port:{DEFAULT_SCTP_PORT}"));
    }
    if section.kind != Kind::Data && !section.has("ssrc") {
        out.extend(synthesized_ssrcs(section));
    }
}

/// One kept attribute line in its standard form (none, one, or one per payload type).
fn standard_attribute(section: &Section, name: &str, line: &str) -> Vec<String> {
    match name {
        "rtcp-fb" => {
            let Some((pt, feedback)) = attribute(line, "rtcp-fb").and_then(|value| value.split_once(' ')) else {
                return Vec::new();
            };
            if feedback.starts_with("x-message") {
                Vec::new()
            } else if pt == "*" {
                section
                    .formats
                    .iter()
                    .map(|pt| format!("a=rtcp-fb:{pt} {feedback}"))
                    .collect()
            } else {
                vec![line.to_owned()]
            }
        }
        "extmap" => vec![line.replace('\\', "/")],
        _ => vec![line.to_owned()],
    }
}

/// A Teams candidate in its standard form: component 1 over UDP only (the native client's media goes through its
/// UDP relay; a TCP candidate is out of its reach), Teams' own extensions left out.
fn standard_candidate(candidate: &str) -> Option<String> {
    let fields: Vec<&str> = candidate.split(' ').collect();
    let [
        foundation,
        "1",
        transport,
        priority,
        address,
        port,
        "typ",
        kind,
        ref rest @ ..,
    ] = fields[..]
    else {
        return None;
    };
    if !transport.eq_ignore_ascii_case("udp") {
        return None;
    }
    let related = match rest {
        ["raddr", raddr, "rport", rport, ..] => format!(" raddr {raddr} rport {rport}"),
        _ => String::new(),
    };
    Some(format!(
        "a=candidate:{foundation} 1 udp {priority} {address} {port} typ {kind}{related}"
    ))
}

/// `a=ssrc` lines for a line that names its streams only by Teams' `a=ssrc-group` or `a=x-ssrc-range`.
fn synthesized_ssrcs(section: &Section) -> Vec<String> {
    let grouped: Vec<&str> = section
        .value("ssrc-group")
        .and_then(|group| group.strip_prefix("FID "))
        .map(|ssrcs| ssrcs.split(' ').collect())
        .unwrap_or_default();
    let ssrcs = if grouped.is_empty() {
        section
            .value("x-ssrc-range")
            .and_then(|range| range.split('-').next())
            .into_iter()
            .collect()
    } else {
        grouped
    };
    ssrcs
        .into_iter()
        .filter(|ssrc| ssrc.parse::<u32>().is_ok())
        .map(|ssrc| format!("a=ssrc:{ssrc} cname:{SYNTHESIZED_CNAME}"))
        .collect()
}
