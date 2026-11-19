//! The keyframes of a shared screen, as the far end asks for them.
//!
//! The Teams mixer asks a sharer for the screen with a `controlVideoStreaming` notice on the call's signaling, as a
//! share starts and as each viewer joins: Teams for the web makes a keyframe at each, and the mixer forwards the share
//! to a new viewer only from a keyframe. The call takes the notice and asks the session
//! ([`super::MediaSession::ask_screen_keyframe`]).
//!
//! The mixer also asks with an RTCP FIR, and while it waits it sends the same FIR again, with the same sequence
//! number. libwebrtc makes one keyframe for each sequence number and ignores the rest. libwebrtc counts each FIR and
//! PLI it gets, answered or not: [`KeyframeWatch`] compares those counts with the keyframes the encoder made, and
//! forces a keyframe when a request stays without one for [`ANSWER_WAIT`].
//!
//! A screen that holds still goes again only once a second (`super::video::SCREEN_IDLE_REPEAT`): each request also
//! sends the last picture again at once, for the encoder to make the keyframe of.

use std::time::{Duration, Instant};

use futures_executor::block_on;
use libwebrtc::rtp_sender::RtpSender;
use libwebrtc::stats::RtcStats;

/// How often the screen thread reads its sender's counts.
pub(super) const KEYFRAME_POLL: Duration = Duration::from_millis(250);
/// For how long after a share starts, and how often, a keyframe is forced unasked (see [`StartKeyframes`]).
pub const START_KEYFRAMES_FOR: Duration = Duration::from_secs(8);
pub(super) const START_KEYFRAME_EVERY: Duration = Duration::from_secs(1);
/// How long a request for a keyframe can stay without one before a keyframe is forced. libwebrtc encodes the next
/// frame as the keyframe it owes, and the screen thread sends one at the request: well within this.
pub(super) const ANSWER_WAIT: Duration = Duration::from_millis(500);

/// What libwebrtc counted of a video sender since it started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SenderCounts {
    /// The FIRs it got.
    pub firs: u32,
    /// The PLIs it got.
    pub plis: u32,
    /// The keyframes it encoded.
    pub keyframes: u32,
}

impl SenderCounts {
    /// The counts of `sender`, from libwebrtc's statistics; none when it has no statistics of a video stream yet.
    pub(super) fn read(sender: &RtpSender) -> Option<Self> {
        let stats = block_on(sender.get_stats()).ok()?;
        Self::of(&stats)
    }

    /// The counts of the video streams in `stats` (an encoding's each), added together.
    fn of(stats: &[RtcStats]) -> Option<Self> {
        stats
            .iter()
            .filter_map(|stats| match stats {
                RtcStats::OutboundRtp(stream) if stream.stream.kind == "video" => Some(Self {
                    firs: stream.outbound.fir_count,
                    plis: stream.outbound.pli_count,
                    keyframes: stream.outbound.key_frames_encoded,
                }),
                _ => None,
            })
            .reduce(|sum, counts| Self {
                firs: sum.firs.saturating_add(counts.firs),
                plis: sum.plis.saturating_add(counts.plis),
                keyframes: sum.keyframes.saturating_add(counts.keyframes),
            })
    }

    fn requests(self) -> u64 {
        u64::from(self.firs) + u64::from(self.plis)
    }
}

/// What the screen thread does after a read of its sender's counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Step {
    /// Nothing.
    Wait,
    /// A request came: the last picture goes again, for the keyframe libwebrtc makes of it.
    Resend,
    /// A request went unanswered: a keyframe is forced, of the last picture sent again.
    Force,
    /// A keyframe forced unasked in a share's first seconds ([`StartKeyframes`]), of the last picture sent again.
    Start,
    /// The call's media controller asked for the screen (`controlVideoStreaming`): a keyframe is forced, of the last
    /// picture sent again.
    Asked,
}

/// The requests for a keyframe that wait for one, read from the sender's counts ([`KeyframeWatch::read`]).
#[derive(Debug, Default)]
pub(super) struct KeyframeWatch {
    last: Option<SenderCounts>,
    /// Since when a request waits for a keyframe.
    waits: Option<Instant>,
}

impl KeyframeWatch {
    /// The counts of the last read.
    pub(super) fn last(&self) -> Option<SenderCounts> {
        self.last
    }

    /// Takes the counts read at `now`: what to do about the requests they show.
    pub(super) fn read(&mut self, counts: SenderCounts, now: Instant) -> Step {
        let Some(last) = self.last.replace(counts) else {
            return Step::Wait;
        };
        if counts.keyframes != last.keyframes {
            self.waits = None;
            return Step::Wait;
        }
        if counts.requests() > last.requests() {
            let since = *self.waits.get_or_insert(now);
            return self.answer(since, now).unwrap_or(Step::Resend);
        }
        self.waits
            .and_then(|since| self.answer(since, now))
            .unwrap_or(Step::Wait)
    }

    /// [`Step::Force`] once the request that waits since `since` waited [`ANSWER_WAIT`].
    fn answer(&mut self, since: Instant, now: Instant) -> Option<Step> {
        (now.duration_since(since) >= ANSWER_WAIT).then(|| {
            self.waits = None;
            Step::Force
        })
    }
}

/// The keyframes forced in a share's first seconds, unasked: one every [`START_KEYFRAME_EVERY`] for
/// [`START_KEYFRAMES_FOR`]. The mixer forwards a share to a viewer from a keyframe that comes after the viewer asked for it (its `sr`, a few
/// seconds after the share starts), and it does not always ask the sharer for one: the viewers then see nothing. Teams
/// for the web makes a keyframe about every second in a share's first seconds, unasked, as these do.
#[derive(Debug)]
pub(super) struct StartKeyframes {
    started: Instant,
    forced: u32,
}

impl StartKeyframes {
    pub(super) fn new(started: Instant) -> Self {
        Self { started, forced: 0 }
    }

    /// Whether a keyframe is due at `now` (and counts it as forced).
    pub(super) fn due(&mut self, now: Instant) -> bool {
        let next = START_KEYFRAME_EVERY * (self.forced + 1);
        let due = next <= START_KEYFRAMES_FOR && now.duration_since(self.started) >= next;
        if due {
            self.forced += 1;
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use libwebrtc::stats::OutboundRtpStats;

    use super::*;

    fn counts(firs: u32, plis: u32, keyframes: u32) -> SenderCounts {
        SenderCounts { firs, plis, keyframes }
    }

    fn outbound(kind: &str, firs: u32, plis: u32, keyframes: u32) -> RtcStats {
        let mut stream = OutboundRtpStats::default();
        stream.stream.kind = kind.to_owned();
        stream.outbound.fir_count = firs;
        stream.outbound.pli_count = plis;
        stream.outbound.key_frames_encoded = keyframes;
        RtcStats::OutboundRtp(stream)
    }

    #[test]
    fn the_counts_add_the_video_streams_and_leave_out_the_rest() {
        let stats = [
            outbound("video", 2, 1, 3),
            outbound("audio", 9, 9, 9),
            outbound("video", 1, 0, 1),
        ];
        assert_eq!(SenderCounts::of(&stats), Some(counts(3, 1, 4)));
        assert_eq!(SenderCounts::of(&[outbound("audio", 1, 1, 1)]), None);
        assert_eq!(SenderCounts::of(&[]), None);
    }

    #[test]
    fn a_request_answered_with_a_keyframe_asks_nothing_more() {
        let start = Instant::now();
        let mut watch = KeyframeWatch::default();
        assert_eq!(
            watch.read(counts(0, 0, 1), start),
            Step::Wait,
            "the first read only counts"
        );
        assert_eq!(watch.read(counts(1, 0, 1), start + KEYFRAME_POLL), Step::Resend);
        assert_eq!(watch.read(counts(1, 0, 2), start + KEYFRAME_POLL * 2), Step::Wait);
        assert_eq!(watch.read(counts(1, 0, 2), start + KEYFRAME_POLL * 8), Step::Wait);
        // a request and its keyframe within one read: answered
        assert_eq!(watch.read(counts(1, 1, 3), start + KEYFRAME_POLL * 9), Step::Wait);
    }

    #[test]
    fn a_fir_sent_again_and_ignored_gets_a_forced_keyframe() {
        // the mixer's FIR again and again (the same number, libwebrtc's keyframe gone already): no keyframe follows
        let start = Instant::now();
        let mut watch = KeyframeWatch::default();
        watch.read(counts(1, 0, 1), start);
        assert_eq!(watch.read(counts(2, 0, 1), start + KEYFRAME_POLL), Step::Resend);
        assert_eq!(watch.read(counts(3, 0, 1), start + KEYFRAME_POLL * 2), Step::Resend);
        assert_eq!(watch.read(counts(4, 0, 1), start + KEYFRAME_POLL * 3), Step::Force);
        // the next request waits its own time
        assert_eq!(watch.read(counts(5, 0, 1), start + KEYFRAME_POLL * 4), Step::Resend);
        assert_eq!(watch.read(counts(6, 0, 2), start + KEYFRAME_POLL * 5), Step::Wait);
    }

    #[test]
    fn a_request_not_sent_again_is_forced_once_it_waited() {
        let start = Instant::now();
        let mut watch = KeyframeWatch::default();
        watch.read(counts(0, 0, 1), start);
        assert_eq!(watch.read(counts(0, 1, 1), start + KEYFRAME_POLL), Step::Resend);
        assert_eq!(watch.read(counts(0, 1, 1), start + KEYFRAME_POLL * 2), Step::Wait);
        assert_eq!(watch.read(counts(0, 1, 1), start + KEYFRAME_POLL * 3), Step::Force);
        assert_eq!(
            watch.read(counts(0, 1, 1), start + KEYFRAME_POLL * 9),
            Step::Wait,
            "once only"
        );
    }

    #[test]
    fn a_share_s_first_seconds_force_a_keyframe_each_second() {
        let start = Instant::now();
        let mut keyframes = StartKeyframes::new(start);
        assert!(!keyframes.due(start + START_KEYFRAME_EVERY / 2));
        let forced: u32 = (1..=40)
            .map(|step| start + KEYFRAME_POLL * step)
            .map(|now| u32::from(keyframes.due(now)))
            .sum();
        assert_eq!(forced, 8, "one a second for the first seconds, then none");
        assert!(!keyframes.due(start + START_KEYFRAME_EVERY * 60));
    }

    #[test]
    fn the_last_read_is_kept() {
        let mut watch = KeyframeWatch::default();
        assert_eq!(watch.last(), None);
        watch.read(counts(1, 2, 3), Instant::now());
        assert_eq!(watch.last(), Some(counts(1, 2, 3)));
    }

    #[test]
    fn counts_of_another_sender_start_again() {
        // a new session's sender counts from zero: no request in that
        let start = Instant::now();
        let mut watch = KeyframeWatch::default();
        watch.read(counts(5, 5, 5), start);
        assert_eq!(watch.read(counts(0, 0, 0), start + KEYFRAME_POLL), Step::Wait);
        assert_eq!(watch.read(counts(1, 0, 0), start + KEYFRAME_POLL * 2), Step::Resend);
    }
}
