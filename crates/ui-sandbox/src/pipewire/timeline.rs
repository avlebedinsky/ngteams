//! When a stream's samples went through it, by the wall clock: the speaker's recording ([`super::PrivatePipeWire`]'s
//! [`recording_timeline`](super::PrivatePipeWire::recording_timeline)) and a microphone added with a sound of its own
//! ([`super::nodes::AddedNode::timeline`]) mark each buffer as it passes (the index of its first sample, and the Unix
//! time in microseconds that sample was at the stream), so a test can say when a sample of the recording was heard or
//! a sample of the microphone was played, and line the two up across sandboxes on the one clock of this machine.
//!
//! A mark is as good as the graph's cycle: a recorded buffer is marked as it is handed over, its first sample one
//! buffer earlier; a played one as it is filled, about one cycle before the graph takes it. Either is within a cycle
//! (about 20 ms at the server's quantum) of the sample's time.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use teams_media::devices::SAMPLE_RATE;

/// One buffer: the index of its first sample, and when that sample was at the stream (Unix µs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    pub sample: u64,
    pub unix_us: i64,
}

/// A stream's marks, shared between the thread that marks them and the ones that read them.
#[derive(Clone, Debug, Default)]
pub struct Timeline {
    marks: Arc<Mutex<Vec<Mark>>>,
}

/// The time now, in Unix microseconds.
#[must_use]
pub fn now_us() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|since| i64::try_from(since.as_micros()).ok())
        .unwrap_or(i64::MAX)
}

/// How long `samples` last at the devices' rate, in microseconds.
#[must_use]
pub fn samples_to_us(samples: u64) -> i64 {
    i64::try_from(samples.saturating_mul(1_000_000) / u64::from(SAMPLE_RATE)).unwrap_or(i64::MAX)
}

/// How many samples `us` microseconds hold at the devices' rate.
fn us_to_samples(us: i64) -> u64 {
    u64::try_from(us).unwrap_or(0).saturating_mul(u64::from(SAMPLE_RATE)) / 1_000_000
}

impl Timeline {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks a buffer whose first sample is `sample`, at `unix_us`. A mark that doesn't go forward (a sample index or a
    /// time behind the last mark's) is dropped.
    pub fn mark(&self, sample: u64, unix_us: i64) {
        let mut marks = self.marks.lock().unwrap_or_else(PoisonError::into_inner);
        if marks
            .last()
            .is_some_and(|last| last.sample >= sample || last.unix_us > unix_us)
        {
            return;
        }
        marks.push(Mark { sample, unix_us });
    }

    /// Every mark so far.
    #[must_use]
    pub fn marks(&self) -> Vec<Mark> {
        self.marks.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// When sample `sample` was at the stream (Unix µs): from the last mark at or before it, at the devices' rate;
    /// `None` before the first mark.
    #[must_use]
    pub fn time_of(&self, sample: u64) -> Option<i64> {
        let marks = self.marks.lock().unwrap_or_else(PoisonError::into_inner);
        let at = marks.partition_point(|mark| mark.sample <= sample);
        let mark = marks.get(at.checked_sub(1)?)?;
        Some(mark.unix_us + samples_to_us(sample - mark.sample))
    }

    /// The sample at the stream at `unix_us`: from the last mark at or before it, at the devices' rate; `None` before
    /// the first mark.
    #[must_use]
    pub fn sample_at(&self, unix_us: i64) -> Option<u64> {
        let marks = self.marks.lock().unwrap_or_else(PoisonError::into_inner);
        let at = marks.partition_point(|mark| mark.unix_us <= unix_us);
        let mark = marks.get(at.checked_sub(1)?)?;
        Some(mark.sample + us_to_samples(unix_us - mark.unix_us))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sample_s_time_runs_on_from_the_last_mark_before_it() {
        let timeline = Timeline::new();
        assert_eq!(timeline.time_of(0), None);
        assert_eq!(timeline.sample_at(1_000_000), None);
        timeline.mark(0, 1_000_000);
        timeline.mark(480, 1_010_000);
        // a buffer's worth later, but the graph ran 2 ms late: the later mark wins from its sample on
        timeline.mark(960, 1_022_000);
        assert_eq!(timeline.time_of(0), Some(1_000_000));
        assert_eq!(timeline.time_of(240), Some(1_005_000));
        assert_eq!(timeline.time_of(960), Some(1_022_000));
        assert_eq!(timeline.time_of(48_960), Some(2_022_000));
        assert_eq!(timeline.sample_at(999_999), None);
        assert_eq!(timeline.sample_at(1_005_000), Some(240));
        assert_eq!(timeline.sample_at(1_021_999), Some(1_055));
        assert_eq!(timeline.sample_at(2_022_000), Some(48_960));
    }

    #[test]
    fn a_mark_that_goes_back_is_dropped() {
        let timeline = Timeline::new();
        timeline.mark(0, 1_000_000);
        timeline.mark(480, 1_010_000);
        timeline.mark(480, 1_020_000);
        timeline.mark(960, 1_005_000);
        assert_eq!(timeline.marks().len(), 2);
        assert_eq!(samples_to_us(48_000), 1_000_000);
    }
}
