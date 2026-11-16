//! A meeting's own steps on its group call: who hosts it (its organizer and its presenters), waiting in its lobby, and
//! being turned away from it or taken out of it. Being let in is a join of the seat that waits ([`Call::join`]), with
//! the leg made from the offer it waited with.

use serde_json::Value;

use super::{At, Call, KEEP_ALIVE, KEEP_ALIVE_SLACK, Kind, Out, Party, Phase, Refusal, Seat, Status};
use crate::calling::wire::{self, Links};

/// What someone turned away from the lobby is told at its `end` callback.
const TURNED_AWAY: (i64, &str) = (403, "Participant was denied entry to the meeting.");
/// What someone a host takes out of the meeting is told.
const TAKEN_OUT: (i64, &str) = (0, "Participant was removed from the meeting.");

impl Call {
    /// Whether the party at `seat` hosts the meeting: it is in the call as its organizer or a presenter, who let people
    /// in from the lobby, turn them away and mute the others.
    pub(crate) fn hosts(&self, seat: Seat) -> Result<(), Refusal> {
        self.in_call(seat)?;
        match self.role_of(self.party(seat)) {
            Some("organizer" | "presenter") => Ok(()),
            _ => Err(Refusal::Forbidden),
        }
    }

    /// A meeting's call made by a joiner who waits in its lobby: nobody is in it yet, the joiner waits with its offer.
    pub(crate) fn started_waiting(&mut self, offer: &Value, at: At<'_>, out: &mut Out) {
        self.kind = Kind::Group;
        self.phase = Phase::Connected;
        self.connected_at = Some(at.now);
        self.alive_due = at.seconds() + KEEP_ALIVE + KEEP_ALIVE_SLACK;
        let party = self.party_mut(Seat::CALLER);
        party.status = Status::Lobby;
        party.waiting = Some(offer.clone());
        self.reschedule();
        self.tell_group(at.origin, &[Seat::CALLER], Some(Seat::CALLER), out);
    }

    /// `account` waits in the meeting's lobby from its endpoint `endpoint` with `offer` (its join's): the roster tells
    /// everyone it waits (the account the whole roster). Someone in the call already is refused.
    pub(crate) fn wait(
        &mut self,
        account: &Party,
        (endpoint, links): (&str, Links),
        offer: &Value,
        at: At<'_>,
        out: &mut Out,
    ) -> Result<Seat, Refusal> {
        if self.phase == Phase::Ended || self.meeting.is_none() {
            return Err(Refusal::Gone);
        }
        let seat = match self.seat_of(&account.id) {
            Some(seat) if self.party(seat).status == Status::InCall => return Err(Refusal::Forbidden),
            Some(seat) => seat,
            None => {
                self.parties.push(Party {
                    status: Status::Left,
                    ..account.clone()
                });
                Seat(self.parties.len() - 1)
            }
        };
        let party = self.party_mut(seat);
        party.participant.clone_from(&account.participant);
        party.name.clone_from(&account.name);
        party.endpoints.clear();
        party.keep(endpoint, links);
        party.active = Some(endpoint.to_owned());
        party.status = Status::Lobby;
        party.ring_due = None;
        party.waiting = Some(offer.clone());
        self.reschedule();
        self.tell_group(at.origin, &[seat], Some(seat), out);
        Ok(seat)
    }

    /// The host at `by` turns away the party at `seat` waiting in the lobby, or takes it out of the call: its endpoint
    /// is told, its leg goes and the roster tells everyone. Anyone else changes nothing.
    pub(crate) fn remove(&mut self, by: Seat, seat: Seat, at: At<'_>, out: &mut Out) -> Result<(), Refusal> {
        self.hosts(by)?;
        let (code, phrase) = match self.party(seat).status {
            Status::Lobby => TURNED_AWAY,
            Status::InCall if seat != by => TAKEN_OUT,
            _ => return Ok(()),
        };
        let was_in = self.party(seat).status == Status::InCall;
        let sender = self.party(by).participant.clone();
        let end = wire::end(code, phrase, Some(&sender));
        out.call_back(at.origin, self.party(seat).link("end"), end);
        self.take_out(seat, out);
        self.tell_group(at.origin, &[seat], None, out);
        if was_in {
            self.tell_presence(at.origin, wire::LEFT, seat, out);
        }
        Ok(())
    }
}
