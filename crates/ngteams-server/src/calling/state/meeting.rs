//! A meeting's own steps on its group call: who hosts it (its organizer and its presenters), waiting in its lobby, and
//! being turned away from it or taken out of it. Being let in is a join of the seat that waits ([`Call::join`]), with
//! the leg made from the offer it waited with.

use serde_json::Value;

use serde_json::json;

use super::{At, Call, Controls, Hand, KEEP_ALIVE, KEEP_ALIVE_SLACK, Kind, Out, Party, Phase, Refusal, Seat, Status};
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

    /// The party at `seat` raises its hand (`publishState` of `raiseHands`) as the state `id`: the roster tells everyone,
    /// and its answer names the state. A hand that is up already stays as it was.
    pub(crate) fn raise_hand(
        &mut self,
        seat: Seat,
        id: &str,
        body: &Value,
        at: At<'_>,
        out: &mut Out,
    ) -> Result<Value, Refusal> {
        self.group_in_call(seat)?;
        let published = body.get("publishedState").ok_or(Refusal::Bad)?;
        if published.get("stateType").and_then(Value::as_str) != Some(wire::RAISE_HANDS) {
            return Err(Refusal::Bad);
        }
        if self.party(seat).hand.is_none() {
            let rank = self
                .parties
                .iter()
                .filter_map(|party| party.hand.as_ref())
                .map(|hand| hand.rank)
                .max();
            self.party_mut(seat).hand = Some(Hand {
                id: id.to_owned(),
                rank: rank.unwrap_or_default() + 1,
                content: published.get("content").cloned().unwrap_or_else(|| json!({})),
            });
            self.tell_group(at.origin, &[seat], None, out);
        }
        let hand = self.party(seat).hand.as_ref().ok_or(Refusal::Gone)?;
        Ok(json!({"publishStateResponse": {"typeRank": hand.rank, "stateId": hand.id}}))
    }

    /// The party at `seat` lowers its hand (`removeState` naming the hand's state): the roster tells everyone. The
    /// answer names each state with how its removal went.
    pub(crate) fn lower_hand(&mut self, seat: Seat, body: &Value, at: At<'_>, out: &mut Out) -> Result<Value, Refusal> {
        self.group_in_call(seat)?;
        let ids = body.get("stateIds").and_then(Value::as_array).ok_or(Refusal::Bad)?;
        let mut results = Vec::new();
        for id in ids.iter().filter_map(Value::as_str) {
            let own = self.party(seat).hand.as_ref().is_some_and(|hand| hand.id == id);
            if own {
                self.party_mut(seat).hand = None;
                self.tell_group(at.origin, &[seat], None, out);
            }
            let (code, phrase) = if own { (200, "Success") } else { (404, "NotFound") };
            results.push(json!({"stateId": id, "result": {"code": code, "phrase": phrase}}));
        }
        Ok(json!({"removeStateResponse": {"results": results}}))
    }

    /// The party at `seat` sends live reactions (`sendMessage`): each reaction goes to everyone in the call, its
    /// sender too, at their `receiveMessage` callback, named by who sent it.
    pub(crate) fn react(&self, seat: Seat, body: &Value, at: At<'_>, out: &mut Out) -> Result<(), Refusal> {
        self.group_in_call(seat)?;
        let messages = body
            .get("messageContent")
            .and_then(Value::as_array)
            .ok_or(Refusal::Bad)?;
        let sender = self.party(seat);
        let from = json!({"id": sender.mri(), "displayName": sender.name,
            "participantId": sender.participant.get("participantId")});
        for message in messages
            .iter()
            .filter(|message| message.get("type").and_then(Value::as_str) == Some("Reaction"))
        {
            let told = json!({
                "from": from, "type": "Reaction", "payload": message.get("payload"),
                "operationId": message.get("operationId"),
            });
            for party in self.parties.iter().filter(|party| party.status == Status::InCall) {
                out.call_back(at.origin, party.link("receiveMessage"), told.clone());
            }
        }
        Ok(())
    }

    /// The party at `seat` is in the call, which is a group call's.
    fn group_in_call(&self, seat: Seat) -> Result<(), Refusal> {
        if self.kind != Kind::Group {
            return Err(Refusal::Missing);
        }
        self.in_call(seat)
    }

    /// The host at `seat` mutes others' main audio (`mute`): everyone else in the call (`scope` `all`) or those it
    /// names. Each muted party's roster entry turns `serverMuted` at a new version (its client mutes itself), and the
    /// outcome goes to `callback`, the host's own.
    pub(crate) fn mute_others(
        &mut self,
        seat: Seat,
        body: &Value,
        callback: Option<&str>,
        at: At<'_>,
        out: &mut Out,
    ) -> Result<(), Refusal> {
        self.hosts(seat)?;
        let named: Vec<String> = body
            .get("muteParticipants")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|person| person.as_str().or_else(|| person.get("id").and_then(Value::as_str)))
            .map(str::to_owned)
            .collect();
        let everyone = body.get("scope").and_then(Value::as_str) == Some("all");
        let muted: Vec<Seat> = (0..self.parties.len())
            .map(Seat)
            .filter(|other| *other != seat && self.party(*other).status == Status::InCall)
            .filter(|other| everyone || named.contains(&self.party(*other).mri()))
            .collect();
        for other in &muted {
            let party = self.party_mut(*other);
            party.server_muted = true;
            party.mute_version += 2;
        }
        if !muted.is_empty() {
            self.tell_group(at.origin, &muted, None, out);
        }
        let operation = body.get("operationId").cloned().unwrap_or(Value::Null);
        out.call_back(at.origin, callback, wire::mute_outcome(self, &muted, &operation));
        Ok(())
    }

    /// The party at `seat` ends the call at its conversation (`DELETE`): a meeting's organizer ends it for everyone in
    /// it, whom it names as who ended it; anyone else leaves it.
    pub(crate) fn end_meeting(&mut self, seat: Seat, body: &Value, at: At<'_>, out: &mut Out) {
        let organizes = self.role_of(self.party(seat)) == Some("organizer");
        if !organizes || self.in_call(seat).is_err() {
            self.leave(seat, body, at, out);
            return;
        }
        let sender = self.party(seat).participant.clone();
        let end = wire::end(0, ENDED_FOR_ALL, Some(&sender));
        self.end_group(at, &end, out);
    }
    /// Whether `party`'s main audio is muted by the meeting's options: it is an attendee, and attendees may not
    /// unmute.
    pub(crate) fn audio_blocked(&self, party: &Party) -> bool {
        let blocked = self
            .meeting
            .as_ref()
            .is_some_and(|meeting| meeting.controls.audio_blocked());
        blocked && self.role_of(party) == Some("attendee")
    }

    /// The meeting's options as its organizer saved them now (`controls`): each party whose audio they mute or let go
    /// gets a new mute version, the roster tells everyone their roles and mutes, and each one in the call or its lobby
    /// is told the options at its `conversationUpdate` callback.
    pub(crate) fn set_controls(&mut self, controls: Controls, at: At<'_>, out: &mut Out) {
        let Some(meeting) = self.meeting.as_ref() else { return };
        if self.phase == Phase::Ended || meeting.controls == controls {
            return;
        }
        let blocked_before: Vec<bool> = self.parties.iter().map(|party| self.audio_blocked(party)).collect();
        if let Some(meeting) = self.meeting.as_mut() {
            meeting.controls = controls;
        }
        for (number, before) in blocked_before.into_iter().enumerate() {
            if self.audio_blocked(self.party(Seat(number))) != before {
                self.party_mut(Seat(number)).mute_version += 2;
            }
        }
        let present: Vec<Seat> = (0..self.parties.len())
            .map(Seat)
            .filter(|seat| matches!(self.party(*seat).status, Status::InCall | Status::Lobby))
            .collect();
        self.tell_group(at.origin, &present, None, out);
        let update = wire::conversation_update(self);
        for seat in present {
            out.call_back(at.origin, self.party(seat).link("conversationUpdate"), update.clone());
        }
    }
}

/// What everyone in a meeting its organizer ended is told.
const ENDED_FOR_ALL: &str = "ConversationEndForAllInitiated";
