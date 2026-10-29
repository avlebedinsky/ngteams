//! An agent's phases, which only move forward, and the estimate of what is left.

use crate::signals::Signal;

/// How long a minute is, in milliseconds.
pub const MINUTE_MS: i64 = 60_000;

/// A phase passed through faster than this was skipped: it is no sample of the phase's length.
const SKIPPED_MS: i64 = 30_000;

/// Which steps an agent goes through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    /// An agent in a worktree: it edits, reviews, commits, lands and runs the gate.
    Edit,
    /// An agent without a worktree: it investigates, does the work, checks it.
    Scratch,
}

impl Flow {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "edit" => Some(Self::Edit),
            "scratch" => Some(Self::Scratch),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Edit => "edit",
            Self::Scratch => "scratch",
        }
    }

    /// The phases, `done` last.
    pub fn phases(self) -> &'static [&'static str] {
        match self {
            Self::Edit => &["investigate", "implement", "revmux", "commit", "land", "gate", "done"],
            Self::Scratch => &["investigate", "implement", "verify", "done"],
        }
    }

    /// The index of `done`.
    pub fn done(self) -> usize {
        self.phases().len() - 1
    }

    /// The typical minutes of each phase but `done`, until finished tasks give their own.
    pub fn default_minutes(self) -> &'static [i64] {
        match self {
            Self::Edit => &[10, 30, 25, 2, 6, 15],
            Self::Scratch => &[5, 20, 10],
        }
    }
}

/// Where an agent is: its phase and when it entered each phase it reached (a skipped one at the time of the next).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Progress {
    pub flow: Flow,
    pub phase: usize,
    pub entered: Vec<Option<i64>>,
}

impl Progress {
    /// The phase `signals` (with their times, in order) lead to from the agent's start at `first_ms`; `done_ms` is
    /// when it was done, if it is.
    pub fn new(flow: Flow, first_ms: i64, signals: &[(i64, Signal)], done_ms: Option<i64>) -> Self {
        let mut progress = Self {
            flow,
            phase: 0,
            entered: vec![None; flow.phases().len()],
        };
        progress.entered[0] = Some(first_ms);
        let mut landed = false;
        let mut edited = false;
        for &(at, signal) in signals {
            let target = match flow {
                Flow::Edit => edit_target(signal, &mut landed),
                Flow::Scratch => scratch_target(signal, progress.phase, &mut edited),
            };
            if let Some(target) = target {
                progress.reach(target, at);
            }
        }
        if let Some(done_ms) = done_ms {
            progress.reach(flow.done(), done_ms);
        }
        progress
    }

    fn reach(&mut self, target: usize, at: i64) {
        if target <= self.phase {
            return;
        }
        for entered in &mut self.entered[self.phase + 1..=target] {
            *entered = Some(at);
        }
        self.phase = target;
    }

    pub fn is_done(&self) -> bool {
        self.phase == self.flow.done()
    }

    /// When the current phase began.
    pub fn since(&self) -> Option<i64> {
        self.entered[self.phase]
    }

    /// How long each phase but `done` took, for a finished agent: `None` for one it skipped (under 30 s) or never
    /// left.
    pub fn lengths(&self) -> Vec<Option<i64>> {
        (0..self.flow.done())
            .map(|phase| {
                let start = self.entered[phase]?;
                let end = self.entered[phase + 1..].iter().flatten().next()?;
                Some(end - start).filter(|length| *length >= SKIPPED_MS)
            })
            .collect()
    }
}

fn edit_target(signal: Signal, landed: &mut bool) -> Option<usize> {
    match signal {
        Signal::Edit | Signal::Build => Some(1),
        Signal::Check => Some(if *landed { 5 } else { 1 }),
        Signal::Revmux => Some(2),
        Signal::Commit => Some(3),
        Signal::Land => {
            *landed = true;
            Some(4)
        }
        Signal::ScratchEdit | Signal::Probe => None,
    }
}

fn scratch_target(signal: Signal, phase: usize, edited: &mut bool) -> Option<usize> {
    match signal {
        Signal::Edit | Signal::ScratchEdit => {
            *edited = true;
            Some(1)
        }
        Signal::Build => Some(1),
        Signal::Check | Signal::Probe | Signal::Revmux => Some(if phase >= 1 && *edited { 2 } else { 1 }),
        Signal::Commit | Signal::Land => None,
    }
}

/// The typical length of each phase of `flow` but `done`: the median of the finished agents' lengths for it, or its
/// default while none has one.
pub fn typical(flow: Flow, finished: &[&Progress]) -> Vec<i64> {
    flow.default_minutes()
        .iter()
        .enumerate()
        .map(|(phase, minutes)| {
            let mut samples: Vec<i64> = finished
                .iter()
                .filter(|progress| progress.flow == flow)
                .filter_map(|progress| progress.lengths()[phase])
                .collect();
            median(&mut samples).unwrap_or(minutes * MINUTE_MS)
        })
        .collect()
}

fn median(samples: &mut [i64]) -> Option<i64> {
    if samples.is_empty() {
        return None;
    }
    samples.sort_unstable();
    let middle = samples.len() / 2;
    Some(if samples.len() % 2 == 1 {
        samples[middle]
    } else {
        i64::midpoint(samples[middle - 1], samples[middle])
    })
}

/// The estimate of what is left: the rest of the current phase's typical length (at least a fifth of it) and the
/// typical lengths of the phases after it. Also whether the current phase is over its typical length.
pub fn estimate(progress: &Progress, typical: &[i64], now_ms: i64) -> Option<(i64, bool)> {
    let current = *typical.get(progress.phase)?;
    let spent = now_ms - progress.since()?;
    let left = (current - spent).max(current / 5);
    let after: i64 = typical[progress.phase + 1..].iter().sum();
    Some((left + after, spent > current))
}

#[cfg(test)]
mod tests {
    use super::{Flow, MINUTE_MS, Progress, estimate, typical};
    use crate::signals::Signal;

    const M: i64 = MINUTE_MS;

    #[test]
    fn an_edit_moves_forward_only() {
        let signals = [
            (2 * M, Signal::Edit),
            (40 * M, Signal::Revmux),
            (50 * M, Signal::Build),
            (60 * M, Signal::Check),
            (65 * M, Signal::Commit),
            (66 * M, Signal::Land),
            (70 * M, Signal::Check),
            (80 * M, Signal::Commit),
        ];
        let progress = Progress::new(Flow::Edit, 0, &signals, None);
        assert_eq!(Flow::Edit.phases()[progress.phase], "gate");
        assert_eq!(progress.since(), Some(70 * M));
        let landed = Progress::new(Flow::Edit, 0, &signals[..6], None);
        assert_eq!(Flow::Edit.phases()[landed.phase], "land");
    }

    #[test]
    fn a_check_before_landing_is_no_gate() {
        let signals = [(M, Signal::Check), (2 * M, Signal::Revmux), (3 * M, Signal::Check)];
        let progress = Progress::new(Flow::Edit, 0, &signals, None);
        assert_eq!(Flow::Edit.phases()[progress.phase], "revmux");
    }

    #[test]
    fn a_scratch_agent_verifies_after_editing() {
        let early = Progress::new(Flow::Scratch, 0, &[(M, Signal::Check)], None);
        assert_eq!(early.phase, 1);
        let signals = [(M, Signal::ScratchEdit), (5 * M, Signal::Probe)];
        let late = Progress::new(Flow::Scratch, 0, &signals, Some(9 * M));
        assert!(late.is_done());
        assert_eq!(late.lengths(), vec![Some(M), Some(4 * M), Some(4 * M)]);
    }

    #[test]
    fn typical_lengths_are_the_finished_medians() {
        let quick = Progress::new(Flow::Scratch, 0, &[(2 * M, Signal::Edit)], Some(4 * M));
        let slow = Progress::new(Flow::Scratch, 0, &[(4 * M, Signal::Edit)], Some(10 * M));
        let lengths = typical(Flow::Scratch, &[&quick, &slow]);
        // investigate: 2 and 4; implement: 2 and 6 (verify skipped in 0 s, so its default stays).
        assert_eq!(lengths, vec![3 * M, 4 * M, 10 * M]);
    }

    #[test]
    fn estimates_what_is_left() {
        let typical = [10 * M, 30 * M, 25 * M, 2 * M, 6 * M, 15 * M];
        let progress = Progress::new(Flow::Edit, 0, &[(5 * M, Signal::Edit)], None);
        assert_eq!(estimate(&progress, &typical, 15 * M), Some((20 * M + 48 * M, false)));
        assert_eq!(estimate(&progress, &typical, 60 * M), Some((6 * M + 48 * M, true)));
    }
}
