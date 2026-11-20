//! The order of a call's tiles, as Teams keeps it on every page of the gallery and of a strip: the presenter first
//! (the far end that shares), then the people who spoke (the newest first), then the one pinned, then (View's Show
//! videos first) the cameras on, then the rest in the roster's order.
//!
//! The order holds still: a tile in sight keeps its place while its person speaks (its glow shows it), so only a
//! person out of sight (on another page) moves, to the front of the speakers. A person moves only after they spoke
//! for [`SETTLE`], so a short sound moves nothing, and the order changes at most once in [`HOLD`], so a quick talk
//! back and forth does not make the strip jump. The page the user turned to stays: the tiles move, the page does not.

use std::time::{Duration, Instant};

/// How long a person speaks before their tile moves up: a cough or a short sound moves nothing.
pub(super) const SETTLE: Duration = Duration::from_secs(1);
/// The shortest time between two changes of the order.
pub(super) const HOLD: Duration = Duration::from_secs(3);

/// Who spoke, and who the stage showed last (see the module docs).
#[derive(Default)]
pub(super) struct Speakers {
    /// A look again at who speaks, while a tile's move waits for its time.
    pub(super) look: Option<gpui::Task<()>>,
    /// Since when each one who speaks now speaks, by MRI.
    since: Vec<(String, Instant)>,
    /// Who moved up because they spoke, the newest first.
    floor: Vec<String>,
    /// When the order last changed.
    moved: Option<Instant>,
    /// Whose tiles the stage drew last, by MRI; `None` while it drew no page of tiles (Focus on content, nobody yet).
    sight: Option<Vec<String>>,
}

impl Speakers {
    /// Takes who speaks at `now` and moves up who spoke long enough out of sight; answers when to look again while a
    /// move waits for [`SETTLE`] or [`HOLD`].
    pub(super) fn follow<'a>(&mut self, speaking: impl IntoIterator<Item = &'a str>, now: Instant) -> Option<Instant> {
        let speaking: Vec<&str> = speaking.into_iter().collect();
        self.since.retain(|(mri, _)| speaking.contains(&mri.as_str()));
        for mri in speaking {
            if !self.since.iter().any(|(known, _)| known == mri) {
                self.since.push((mri.to_owned(), now));
            }
        }
        let ready = |since: Instant, moved: Option<Instant>| {
            let settled = since + SETTLE;
            moved.map_or(settled, |moved| settled.max(moved + HOLD))
        };
        let mut due: Vec<(String, Instant)> = self
            .since
            .iter()
            .filter(|(mri, _)| self.out_of_sight(mri))
            .map(|(mri, since)| (mri.clone(), *since))
            .collect();
        // the one who spoke first moves first, so the newest speaker ends up at the front
        due.sort_by_key(|(_, since)| *since);
        let moving: Vec<String> = due
            .iter()
            .filter(|(_, since)| ready(*since, self.moved) <= now)
            .map(|(mri, _)| mri.clone())
            .collect();
        if !moving.is_empty() {
            for mri in &moving {
                self.floor.retain(|known| known != mri);
                self.floor.insert(0, mri.clone());
            }
            self.moved = Some(now);
            // what moved is in sight from the next page drawn on
            self.sight = None;
        }
        due.iter()
            .filter(|(mri, _)| !moving.contains(mri))
            .map(|(_, since)| ready(*since, self.moved))
            .min()
    }

    /// Whether `mri`'s tile was not on the page the stage drew last; nobody is while it drew no page.
    fn out_of_sight(&self, mri: &str) -> bool {
        self.sight
            .as_ref()
            .is_some_and(|sight| !sight.iter().any(|shown| shown == mri))
    }

    /// The stage drew these tiles (`None`: no page of tiles).
    pub(super) fn drawn(&mut self, sight: Option<Vec<String>>) {
        self.sight = sight;
    }

    /// The stage drew `mri`'s tile too (the speaker view's large one).
    pub(super) fn also_drawn(&mut self, mri: Option<String>) {
        if let (Some(sight), Some(mri)) = (self.sight.as_mut(), mri) {
            sight.push(mri);
        }
    }

    /// Where the tile of `mri` goes: its group, then its place in the group.
    pub(super) fn rank(&self, mri: Option<&str>, place: Place) -> (u8, usize) {
        if place.presenter {
            return (0, 0);
        }
        if let Some(index) = mri.and_then(|mri| self.floor.iter().position(|known| known == mri)) {
            return (1, index);
        }
        if place.pinned {
            (2, 0)
        } else if place.video_first {
            (3, 0)
        } else {
            (4, 0)
        }
    }
}

/// What places a tile apart from the roster's order, besides its speech.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Place {
    /// Its person shares the screen the stage shows.
    pub(super) presenter: bool,
    /// The account pinned it.
    pub(super) pinned: bool,
    /// Its camera is on and View's Show videos first is on.
    pub(super) video_first: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "8:orgid:a";
    const B: &str = "8:orgid:b";
    const C: &str = "8:orgid:c";

    fn at(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    fn floor(speakers: &Speakers) -> Vec<&str> {
        speakers.floor.iter().map(String::as_str).collect()
    }

    #[test]
    fn a_speaker_out_of_sight_moves_up_after_settling() {
        let start = Instant::now();
        let mut speakers = Speakers::default();
        speakers.drawn(Some(vec![A.to_owned()]));
        // B (on another page) starts to speak: nothing moves yet, a look is due when it settled
        assert_eq!(speakers.follow([B], start), Some(start + SETTLE));
        assert!(floor(&speakers).is_empty());
        assert_eq!(speakers.follow([B], at(start, 999)), Some(start + SETTLE));
        assert!(floor(&speakers).is_empty());
        assert_eq!(speakers.follow([B], start + SETTLE), None);
        assert_eq!(floor(&speakers), vec![B]);
    }

    #[test]
    fn a_short_sound_moves_nothing() {
        let start = Instant::now();
        let mut speakers = Speakers::default();
        speakers.drawn(Some(vec![A.to_owned()]));
        speakers.follow([B], start);
        assert_eq!(speakers.follow([], at(start, 500)), None);
        assert_eq!(speakers.follow([], at(start, 5_000)), None);
        assert!(floor(&speakers).is_empty());
    }

    #[test]
    fn a_speaker_in_sight_keeps_its_place() {
        let start = Instant::now();
        let mut speakers = Speakers::default();
        speakers.drawn(Some(vec![A.to_owned(), B.to_owned()]));
        assert_eq!(speakers.follow([B], start), None);
        assert_eq!(speakers.follow([B], at(start, 10_000)), None);
        assert!(floor(&speakers).is_empty());
    }

    #[test]
    fn nobody_moves_while_no_page_of_tiles_shows() {
        let start = Instant::now();
        let mut speakers = Speakers::default();
        assert_eq!(speakers.follow([B], start), None);
        assert_eq!(speakers.follow([B], at(start, 10_000)), None);
        assert!(floor(&speakers).is_empty());
    }

    #[test]
    fn the_order_changes_at_most_once_in_the_hold() {
        let start = Instant::now();
        let mut speakers = Speakers::default();
        speakers.drawn(Some(vec![A.to_owned()]));
        speakers.follow([B], start);
        speakers.follow([B], start + SETTLE);
        assert_eq!(floor(&speakers), vec![B]);
        speakers.drawn(Some(vec![B.to_owned(), A.to_owned()]));
        // C (out of sight) speaks right after: it waits for the hold to end
        let c_from = at(start, 1_100);
        assert_eq!(speakers.follow([C], c_from), Some(start + SETTLE + HOLD));
        assert_eq!(speakers.follow([C], at(start, 3_000)), Some(start + SETTLE + HOLD));
        assert_eq!(floor(&speakers), vec![B]);
        assert_eq!(speakers.follow([C], start + SETTLE + HOLD), None);
        assert_eq!(floor(&speakers), vec![C, B]);
    }

    #[test]
    fn the_newest_speaker_leads_and_one_who_spoke_before_comes_back_to_the_front() {
        let start = Instant::now();
        let mut speakers = Speakers::default();
        speakers.drawn(Some(vec![]));
        // A and B start together, A stops before it settled
        speakers.follow([A, B], start);
        speakers.follow([B], at(start, 500));
        speakers.follow([B], start + SETTLE);
        assert_eq!(floor(&speakers), vec![B]);
        speakers.drawn(Some(vec![B.to_owned()]));
        speakers.follow([A], at(start, 5_000));
        speakers.follow([A], at(start, 6_000));
        assert_eq!(floor(&speakers), vec![A, B]);
        // B fell out of sight and speaks again: back to the front
        speakers.drawn(Some(vec![A.to_owned()]));
        speakers.follow([B], at(start, 10_000));
        speakers.follow([B], at(start, 11_000));
        assert_eq!(floor(&speakers), vec![B, A]);
    }

    #[test]
    fn speakers_that_settle_together_move_in_one_change_the_newest_first() {
        let start = Instant::now();
        let mut speakers = Speakers::default();
        speakers.drawn(Some(vec![]));
        speakers.follow([A], start);
        assert_eq!(speakers.follow([A, B], at(start, 200)), Some(start + SETTLE));
        assert_eq!(speakers.follow([A, B], at(start, 1_200)), None);
        assert_eq!(floor(&speakers), vec![B, A]);
    }

    #[test]
    fn the_presenter_leads_then_the_speakers_the_pinned_the_cameras_and_the_rest() {
        let start = Instant::now();
        let mut speakers = Speakers::default();
        speakers.drawn(Some(vec![]));
        speakers.follow([B], start);
        speakers.follow([B], start + SETTLE);
        let none = Place::default();
        let presenter = Place {
            presenter: true,
            ..none
        };
        let pinned = Place { pinned: true, ..none };
        let camera = Place {
            video_first: true,
            ..none
        };
        assert_eq!(speakers.rank(Some(A), presenter), (0, 0));
        assert_eq!(speakers.rank(Some(B), presenter), (0, 0));
        assert_eq!(speakers.rank(Some(B), none), (1, 0));
        assert_eq!(speakers.rank(Some(C), pinned), (2, 0));
        assert_eq!(speakers.rank(Some(C), camera), (3, 0));
        assert_eq!(speakers.rank(Some(C), none), (4, 0));
        assert_eq!(speakers.rank(None, none), (4, 0));
    }
}
