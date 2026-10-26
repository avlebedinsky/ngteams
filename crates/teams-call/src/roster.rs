//! Who is in a call, as its roster updates tell it (`docs/protocol/calling.md`, "The roster"): each `rosterUpdate`
//! delivered at the conversation's callback lists the participants that changed, each with a version that counts up;
//! the call's participants are the ones whose latest entry is active and in the call, in the order they were first
//! seen. The account's own entry is left out. Nothing is sent for it: the roster is read only to show who is in the
//! call, whose video goes where, and who speaks (the fence reads no roster).

use teams_fence::Mri;

use crate::read::RosterEntry;

/// One participant of a call, the account aside.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Participant {
    pub mri: Mri,
    /// As the roster gives it.
    pub name: Option<String>,
    /// The source id of its main audio: who a group call's mixer says spoke.
    pub audio_source: Option<i64>,
    /// The source id of its main video: what a source request names to receive it.
    pub video_source: Option<i64>,
    /// Whether it sends video now (its main video line `sendrecv` or `sendonly`).
    pub video_on: bool,
}

/// A call's roster as delivered so far.
#[derive(Clone, Debug, Default)]
pub(crate) struct Roster {
    /// Each participant's latest entry, in the order first seen.
    entries: Vec<RosterEntry>,
    /// The participants last told.
    told: Vec<Participant>,
}

impl Roster {
    /// The participants as last told.
    pub(crate) fn participants(&self) -> &[Participant] {
        &self.told
    }

    /// This roster for another call: its entries kept, nobody told yet (the next [`Roster::apply`] tells them all).
    pub(crate) fn seeded(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            told: Vec::new(),
        }
    }

    /// Takes an update's entries (an entry no newer than the one kept changes nothing); the participants now, when
    /// they changed since last told. `me` is the account, left out.
    pub(crate) fn apply(&mut self, update: Vec<RosterEntry>, me: &Mri) -> Option<Vec<Participant>> {
        for entry in update {
            match self.entries.iter_mut().find(|kept| kept.mri == entry.mri) {
                Some(kept) if kept.version < entry.version => *kept = entry,
                Some(_) => {}
                None => self.entries.push(entry),
            }
        }
        let now: Vec<Participant> = self
            .entries
            .iter()
            .filter(|entry| entry.active && &entry.mri != me)
            .map(|entry| Participant {
                mri: entry.mri.clone(),
                name: entry.name.clone(),
                audio_source: entry.audio_source,
                video_source: entry.video_source,
                video_on: entry.video_on,
            })
            .collect();
        if now == self.told {
            return None;
        }
        self.told.clone_from(&now);
        Some(now)
    }
}
