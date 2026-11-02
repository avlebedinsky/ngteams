//! An agent's phases and the estimate of what is left. Within one reading of a transcript the phase only moves
//! forward, but for one step kept truthful: an editing agent's `check` lasts only until its next edit, so an agent
//! gone back to fixing after a failed check shows `implement` again.

use crate::signals::Signal;

/// How long a minute is, in milliseconds.
pub const MINUTE_MS: i64 = 60_000;

/// A phase passed through faster than this was skipped: it is no sample of the phase's length.
const SKIPPED_MS: i64 = 30_000;

/// Which steps an agent goes through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    /// A task's agent in a worktree: it edits, checks that it compiles, commits and lands, unreviewed and ungated.
    Edit,
    /// The review-and-gate pass over what the tasks landed: revmux rounds and their fixes, the gate, the fixes landed.
    Review,
    /// An agent without a worktree: it investigates, does the work, checks it.
    Scratch,
}

impl Flow {
    /// Every flow, in the order of [`Flow::index`].
    pub const ALL: [Self; 3] = [Self::Edit, Self::Review, Self::Scratch];

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "edit" => Some(Self::Edit),
            "review" => Some(Self::Review),
            "scratch" => Some(Self::Scratch),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Edit => "edit",
            Self::Review => "review",
            Self::Scratch => "scratch",
        }
    }

    /// Where this flow is in [`Flow::ALL`].
    pub fn index(self) -> usize {
        match self {
            Self::Edit => 0,
            Self::Review => 1,
            Self::Scratch => 2,
        }
    }

    /// The phases, `done` last.
    pub fn phases(self) -> &'static [&'static str] {
        match self {
            Self::Edit => &["investigate", "implement", "check", "commit", "land", "done"],
            Self::Review => &["review", "gate", "land", "done"],
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
            Self::Edit => &[10, 30, 4, 1, 6],
            Self::Review => &[45, 30, 8],
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

/// What an agent's signals so far showed.
#[derive(Default)]
struct Seen {
    /// It edited a file (in the repo, or for an agent without a worktree anywhere).
    edited: bool,
    /// It ran a revmux round.
    reviewed: bool,
    /// Since when it has only been building or testing, with no edit since.
    checking: Option<i64>,
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
        let mut seen = Seen::default();
        for &(at, signal) in signals {
            match flow {
                Flow::Edit => progress.edit_step(signal, at, &mut seen),
                Flow::Review => progress.review_step(signal, at, &mut seen),
                Flow::Scratch => progress.scratch_step(signal, at, &mut seen),
            }
        }
        // An editing agent building or testing since its last edit is checking it, until it edits again.
        if flow == Flow::Edit
            && progress.phase == 1
            && let Some(checking) = seen.checking
        {
            progress.reach(2, checking);
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

    /// investigate, implement, check, commit, land: the check is the builds and tests after the last edit before
    /// the commit.
    fn edit_step(&mut self, signal: Signal, at: i64, seen: &mut Seen) {
        match signal {
            Signal::Edit => {
                seen.edited = true;
                seen.checking = None;
                self.reach(1, at);
            }
            Signal::Build | Signal::Check => {
                self.reach(1, at);
                if seen.edited && self.phase == 1 {
                    seen.checking.get_or_insert(at);
                }
            }
            Signal::Commit => {
                if let Some(checking) = seen.checking.take() {
                    self.reach(2, checking);
                }
                self.reach(3, at);
            }
            Signal::Land => self.reach(4, at),
            Signal::ScratchEdit | Signal::Probe | Signal::Revmux => {}
        }
    }

    /// review, gate, land: the fixes between revmux rounds belong to the review, and the gate begins with the first
    /// clippy, test or lint run after a round.
    fn review_step(&mut self, signal: Signal, at: i64, seen: &mut Seen) {
        match signal {
            Signal::Revmux => seen.reviewed = true,
            Signal::Check if seen.reviewed => self.reach(1, at),
            Signal::Land => self.reach(2, at),
            _ => {}
        }
    }

    /// investigate, implement, verify: a check or probe verifies only once the agent has edited something.
    fn scratch_step(&mut self, signal: Signal, at: i64, seen: &mut Seen) {
        match signal {
            Signal::Edit | Signal::ScratchEdit => {
                seen.edited = true;
                self.reach(1, at);
            }
            Signal::Build => self.reach(1, at),
            Signal::Check | Signal::Probe | Signal::Revmux => {
                let target = if self.phase >= 1 && seen.edited { 2 } else { 1 };
                self.reach(target, at);
            }
            Signal::Commit | Signal::Land => {}
        }
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

    fn phase_of(flow: Flow, signals: &[(i64, Signal)]) -> (&'static str, Option<i64>) {
        let progress = Progress::new(flow, 0, signals, None);
        (flow.phases()[progress.phase], progress.since())
    }

    #[test]
    fn an_edit_checks_commits_and_lands() {
        let signals = [
            (2 * M, Signal::Edit),
            (10 * M, Signal::Build),
            (20 * M, Signal::Edit),
            (30 * M, Signal::Build),
            (35 * M, Signal::Check),
            (40 * M, Signal::Commit),
            (41 * M, Signal::Land),
            (45 * M, Signal::Edit),
            (46 * M, Signal::Build),
        ];
        assert_eq!(phase_of(Flow::Edit, &signals[..2]), ("check", Some(10 * M)));
        // An edit after a check is implementing again.
        assert_eq!(phase_of(Flow::Edit, &signals[..3]), ("implement", Some(2 * M)));
        assert_eq!(phase_of(Flow::Edit, &signals[..5]), ("check", Some(30 * M)));
        assert_eq!(phase_of(Flow::Edit, &signals[..6]), ("commit", Some(40 * M)));
        // A fix after landing (`land.sh`'s exit 3) stays in `land`.
        assert_eq!(phase_of(Flow::Edit, &signals), ("land", Some(41 * M)));
        let done = Progress::new(Flow::Edit, 0, &signals, Some(50 * M));
        assert!(done.is_done());
        assert_eq!(
            done.lengths(),
            vec![Some(2 * M), Some(28 * M), Some(10 * M), Some(M), Some(9 * M)]
        );
    }

    #[test]
    fn a_commit_without_a_check_skips_it() {
        let signals = [(M, Signal::Build), (2 * M, Signal::Edit), (5 * M, Signal::Commit)];
        // A build before any edit is no check of an edit.
        assert_eq!(phase_of(Flow::Edit, &signals[..1]), ("implement", Some(M)));
        let progress = Progress::new(Flow::Edit, 0, &signals, None);
        assert_eq!(Flow::Edit.phases()[progress.phase], "commit");
        assert_eq!(progress.entered[2], Some(5 * M));
    }

    #[test]
    fn a_review_pass_reviews_gates_and_lands() {
        let signals = [
            (M, Signal::Check),
            (2 * M, Signal::Revmux),
            (20 * M, Signal::Edit),
            (21 * M, Signal::Build),
            (22 * M, Signal::Commit),
            (25 * M, Signal::Revmux),
            (40 * M, Signal::Check),
            (60 * M, Signal::Commit),
            (61 * M, Signal::Land),
        ];
        // A test before the first round is no gate.
        assert_eq!(phase_of(Flow::Review, &signals[..1]), ("review", Some(0)));
        assert_eq!(phase_of(Flow::Review, &signals[..6]), ("review", Some(0)));
        assert_eq!(phase_of(Flow::Review, &signals[..8]), ("gate", Some(40 * M)));
        assert_eq!(phase_of(Flow::Review, &signals), ("land", Some(61 * M)));
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
        let typical = [10 * M, 30 * M, 4 * M, M, 6 * M];
        let progress = Progress::new(Flow::Edit, 0, &[(5 * M, Signal::Edit)], None);
        assert_eq!(estimate(&progress, &typical, 15 * M), Some((20 * M + 11 * M, false)));
        assert_eq!(estimate(&progress, &typical, 60 * M), Some((6 * M + 11 * M, true)));
    }

    #[test]
    fn every_flow_has_a_default_for_each_phase() {
        for (index, flow) in Flow::ALL.into_iter().enumerate() {
            assert_eq!(flow.index(), index);
            assert_eq!(flow.default_minutes().len(), flow.done());
            assert_eq!(Flow::parse(flow.name()), Some(flow));
        }
    }
}
