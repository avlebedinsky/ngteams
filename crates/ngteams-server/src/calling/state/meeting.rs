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
    /// Whether the call's end goes to its chat. A channel's meeting that was never posted (nobody entered it, so it
    /// lapses) has no post in its channel, and its end has nothing to close there: it posts no end.
    pub(crate) fn tells_chat_end(&self) -> bool {
        !self
            .meeting
            .as_ref()
            .is_some_and(|meeting| meeting.channel && meeting.post.is_none())
    }

    /// The channel meeting's post its end replies under, as Teams writes a channel meeting's end into its post's
    /// chain: the channel then shows the post alone, ended. `None` for any other call.
    pub(crate) fn end_under(&self) -> Option<i64> {
        let meeting = self.meeting.as_ref().filter(|meeting| meeting.channel)?;
        meeting.post.as_deref()?.parse().ok()
    }

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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::calling::media::MediaHome;
    use crate::calling::state::{Leg, Meeting, Showing};

    const ORIGIN: &str = "https://own.example.test";
    const NAMES: [&str; 6] = [
        "acceptance",
        "end",
        "rosterUpdate",
        "receiveMessage",
        "conversationUpdate",
        "clientCallback",
    ];

    fn link(endpoint: &str, name: &str) -> String {
        format!("{ORIGIN}/trouter/v4/f/{endpoint}/callAgent/agent/1/call/{name}/")
    }

    fn links(endpoint: &str) -> Links {
        NAMES
            .iter()
            .map(|name| ((*name).to_owned(), link(endpoint, name)))
            .collect()
    }

    fn party(id: &str) -> Party {
        Party {
            id: id.to_owned(),
            name: id.to_uppercase(),
            participant: json!({"id": format!("8:orgid:{id}"), "endpointId": format!("ep-{id}"),
                "participantId": format!("p-{id}")}),
            ..Party::default()
        }
    }

    fn at(now: i64) -> At<'static> {
        At { origin: ORIGIN, now }
    }

    fn sdp(blob: &str) -> Value {
        json!({"blob": blob, "contentType": "application/sdp-ngc-1.0"})
    }

    /// A meeting a organizes, its call started by a at second 1000; b and c joined it.
    fn meeting() -> Call {
        let mut organizer = party("a");
        organizer.keep("A", links("A"));
        organizer.active = Some("A".to_owned());
        let mut call = Call {
            id: "call".to_owned(),
            conversation: "conversation".to_owned(),
            phase: Phase::Connected,
            parties: vec![organizer],
            offer: Value::Null,
            started_at: 1_000_000,
            connected_at: None,
            ended_at: None,
            due_at: 0,
            roster_sequence: 0,
            negotiation: None,
            negotiations: 0,
            answered: None,
            held_by: None,
            chat: Some("19:meeting_x@thread.v2".to_owned()),
            accepted_as: None,
            kind: Kind::Group,
            media: Some(MediaHome {
                node: "n".into(),
                url: "http://n.invalid".into(),
                epoch: 1,
            }),
            next_source: 0,
            watchers: Vec::new(),
            alive_due: 0,
            meeting: Some(Meeting {
                organizer: "a".to_owned(),
                ..Meeting::default()
            }),
        };
        let sources = call.next_sources();
        call.party_mut(Seat::CALLER).leg = Some(Leg {
            id: "leg-a".to_owned(),
            sources,
            before: None,
        });
        call.started_group(at(1_000_000), 0);
        for id in ["b", "c"] {
            joined(&mut call, id);
        }
        call
    }

    fn joined(call: &mut Call, id: &str) -> Seat {
        let leg = format!("leg-{id}");
        let endpoint = id.to_uppercase();
        let reserved = call.reserve(&party(id), &leg).unwrap();
        call.join(
            reserved.seat,
            (&leg, &endpoint, links(&endpoint)),
            &sdp("answer"),
            at(1_001_000),
            &mut Out::default(),
        )
        .unwrap();
        reserved.seat
    }

    fn waits(call: &mut Call, id: &str) -> (Seat, Out) {
        let mut out = Out::default();
        let endpoint = id.to_uppercase();
        let seat = call
            .wait(
                &party(id),
                (&endpoint, links(&endpoint)),
                &sdp("offer"),
                at(1_002_000),
                &mut out,
            )
            .unwrap();
        (seat, out)
    }

    /// Each delivery's endpoint and the callback it went to.
    fn paths(out: &Out) -> Vec<(&str, &str)> {
        out.deliveries
            .iter()
            .map(|delivery| {
                let kind = delivery.path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
                (delivery.endpoint.as_str(), kind)
            })
            .collect()
    }

    const B: Seat = Seat(1);
    const C: Seat = Seat(2);

    #[test]
    fn the_organizer_and_presenters_host_attendees_and_whoever_waits_dont() {
        let mut call = meeting();
        assert_eq!(call.hosts(Seat::CALLER), Ok(()));
        assert_eq!(call.hosts(B), Ok(()));
        let (d, _) = waits(&mut call, "d");
        assert_eq!(call.hosts(d), Err(Refusal::Forbidden));
        call.set_controls(
            Controls {
                presenters: "OrganizerOnly".to_owned(),
                ..Controls::default()
            },
            at(1_003_000),
            &mut Out::default(),
        );
        assert_eq!(call.hosts(B), Err(Refusal::Forbidden));
        assert_eq!(call.hosts(Seat::CALLER), Ok(()));
        assert_eq!(call.role_of(call.party(B)), Some("attendee"));
        let mut group = call.clone();
        group.meeting = None;
        assert_eq!(group.hosts(Seat::CALLER), Err(Refusal::Forbidden));
        assert_eq!(group.role_of(group.party(B)), None);
    }

    #[test]
    fn someone_waits_with_its_offer_listed_in_the_lobby() {
        let mut call = meeting();
        let (d, out) = waits(&mut call, "d");
        assert_eq!(call.party(d).status, Status::Lobby);
        assert_eq!(call.party(d).waiting, Some(sdp("offer")));
        // the one that waits gets the whole roster, everyone in the call its entry
        assert_eq!(
            paths(&out),
            [
                ("A", "rosterUpdate"),
                ("B", "rosterUpdate"),
                ("C", "rosterUpdate"),
                ("D", "rosterUpdate")
            ]
        );
        let entry = &out.deliveries[0].body["participants"]["8:orgid:d"];
        assert_eq!(entry["state"], "active");
        assert_eq!(entry["endpoints"]["ep-d"]["modalityJoined"], "Lobby");
        assert!(entry["endpoints"]["ep-d"]["lobby"].is_object());
        assert!(entry["endpoints"]["ep-d"].get("call").is_none());
        assert_eq!(out.deliveries[3].body["participants"].as_object().unwrap().len(), 4);
    }

    #[test]
    fn one_turned_away_from_the_lobby_is_told_and_gone_and_may_try_again() {
        let mut call = meeting();
        let (d, _) = waits(&mut call, "d");
        // in the call already: no wait
        let refused = call.wait(
            &party("b"),
            ("B", links("B")),
            &sdp("offer"),
            at(1_002_000),
            &mut Out::default(),
        );
        assert_eq!(refused, Err(Refusal::Forbidden));
        // b, a presenter, turns d away
        let mut out = Out::default();
        call.remove(B, d, at(1_003_000), &mut out).unwrap();
        assert_eq!(out.deliveries[0].endpoint, "D");
        assert_eq!(out.deliveries[0].body["callEnd"]["code"], 403);
        assert_eq!(out.deliveries[0].body["callEnd"]["sender"]["id"], "8:orgid:b");
        assert_eq!(
            (call.party(d).status, call.party(d).waiting.as_ref()),
            (Status::Left, None)
        );
        assert_eq!(out.deliveries[1].body["participants"]["8:orgid:d"]["state"], "inactive");
        // turning away someone who doesn't wait, or oneself, changes nothing
        let mut out = Out::default();
        call.remove(B, d, at(1_004_000), &mut out).unwrap();
        call.remove(B, B, at(1_004_000), &mut out).unwrap();
        assert!(out.deliveries.is_empty());
        // it may try again, in the same seat
        assert_eq!(waits(&mut call, "d").0, d);
    }

    #[test]
    fn an_add_rings_nobody_who_waits_in_the_lobby() {
        let mut call = meeting();
        let (d, _) = waits(&mut call, "d");
        assert_eq!(call.reserve_added(&party("d"), "leg-d"), Err(Refusal::Forbidden));
        assert_eq!(call.party(d).status, Status::Lobby);
    }

    #[test]
    fn a_host_takes_someone_out_of_the_call_and_the_others_are_told() {
        let mut call = meeting();
        let mut out = Out::default();
        call.remove(Seat::CALLER, C, at(1_003_000), &mut out).unwrap();
        assert_eq!(call.party(C).status, Status::Left);
        assert_eq!(out.released, ["leg-c"]);
        assert_eq!(out.deliveries[0].endpoint, "C");
        assert_eq!(out.deliveries[0].body["callEnd"]["code"], 0);
        assert!(
            out.deliveries.iter().any(|delivery| delivery.path.is_empty()),
            "116 to the others"
        );
        let (d, _) = waits(&mut call, "d");
        assert_eq!(
            call.remove(d, B, at(1_004_000), &mut Out::default()),
            Err(Refusal::Forbidden)
        );
    }

    #[test]
    fn whoever_waits_alone_keeps_the_call_until_it_leaves() {
        let mut call = meeting();
        call.parties.truncate(1);
        call.party_mut(Seat::CALLER).leg = None;
        call.party_mut(Seat::CALLER).joined = None;
        let mut out = Out::default();
        call.started_waiting(&sdp("offer"), at(1_000_000), &mut out);
        assert_eq!(call.party(Seat::CALLER).status, Status::Lobby);
        assert_eq!(paths(&out), [("A", "rosterUpdate")]);
        assert_eq!(call.alive_due, 1_000 + KEEP_ALIVE + KEEP_ALIVE_SLACK);
        let mut out = Out::default();
        call.leave(Seat::CALLER, &json!({}), at(1_005_000), &mut out);
        assert_eq!(call.phase, Phase::Ended);
        assert!(out.room_over);
    }

    #[test]
    fn a_host_mutes_everyone_else_or_whom_it_names_and_an_own_unmute_takes_it_back() {
        let mut call = meeting();
        let mut out = Out::default();
        let named = json!({"scope": "specified", "muteParticipants": [{"id": "8:orgid:b"}], "operationId": "op"});
        let callback = link("A", "clientCallback");
        call.mute_others(Seat::CALLER, &named, Some(&callback), at(1_003_000), &mut out)
            .unwrap();
        assert!(call.party(B).server_muted && !call.party(C).server_muted);
        assert_eq!(call.party(B).mute_version, 2);
        let told = &out.deliveries.last().unwrap().body;
        assert_eq!(told["operationId"], "op");
        assert_eq!(told["muteUnmuteResponseAsync"]["8:orgid:b"][0]["serverMuteVersion"], 2);
        assert!(told["muteUnmuteResponseAsync"].get("8:orgid:c").is_none());
        let roster = &out.deliveries[0].body["participants"]["8:orgid:b"]["endpoints"]["ep-b"]["call"];
        assert_eq!(roster["mediaStreams"][0]["serverMuted"], true);
        assert_eq!(roster["serverMuteVersion"], 2);
        // everyone else, the host aside
        let all = json!({"scope": "all", "muteParticipants": []});
        call.mute_others(B, &all, None, at(1_004_000), &mut Out::default())
            .unwrap();
        assert!(call.party(C).server_muted && call.party(Seat::CALLER).server_muted);
        assert_eq!(call.party(B).mute_version, 2);
        // b's own unmute takes its mute back and tells everyone
        let mut out = Out::default();
        let unmute = json!({"endpointState": {"endpointStateSequenceNumber": 2, "state": {"isMuted": false}}});
        call.endpoint_state(B, &unmute, at(1_005_000), &mut out).unwrap();
        assert!(!call.party(B).server_muted);
        assert_eq!(out.deliveries.len(), 3);
        // an attendee may not
        call.set_controls(
            Controls {
                presenters: "OrganizerOnly".to_owned(),
                ..Controls::default()
            },
            at(1_006_000),
            &mut Out::default(),
        );
        assert_eq!(
            call.mute_others(B, &all, None, at(1_007_000), &mut Out::default()),
            Err(Refusal::Forbidden)
        );
    }

    #[test]
    fn hands_go_up_in_the_order_raised_and_come_down_by_their_state() {
        let mut call = meeting();
        let raise = json!({"publishedState": {"stateType": "raiseHands", "content": {"skinTone": 2}}});
        let mut out = Out::default();
        let answer = call.raise_hand(B, "h-b", &raise, at(1_003_000), &mut out).unwrap();
        assert_eq!(answer["publishStateResponse"], json!({"typeRank": 1, "stateId": "h-b"}));
        let state = &out.deliveries[0].body["participants"]["8:orgid:b"]["publishedStates"][0];
        assert_eq!(state["stateType"], "raiseHands");
        assert_eq!(state["content"]["skinTone"], 2);
        let answer = call
            .raise_hand(C, "h-c", &raise, at(1_003_000), &mut Out::default())
            .unwrap();
        assert_eq!(answer["publishStateResponse"]["typeRank"], 2);
        // raised again: as it was, nobody told
        let mut out = Out::default();
        let answer = call.raise_hand(B, "other", &raise, at(1_004_000), &mut out).unwrap();
        assert_eq!(answer["publishStateResponse"]["stateId"], "h-b");
        assert!(out.deliveries.is_empty());
        let other = json!({"publishedState": {"stateType": "spotlight"}});
        assert_eq!(
            call.raise_hand(B, "x", &other, at(1_004_000), &mut out),
            Err(Refusal::Bad)
        );
        assert_eq!(
            call.raise_hand(B, "x", &json!({}), at(1_004_000), &mut out),
            Err(Refusal::Bad)
        );
        // lowered by its state; another's state isn't the party's to lower
        let lower = json!({"stateIds": ["h-c", "h-b"]});
        let answer = call.lower_hand(B, &lower, at(1_005_000), &mut out).unwrap();
        let results = &answer["removeStateResponse"]["results"];
        assert_eq!(
            (
                results[0]["result"]["code"].clone(),
                results[1]["result"]["code"].clone()
            ),
            (json!(404), json!(200))
        );
        assert!(call.party(B).hand.is_none() && call.party(C).hand.is_some());
        assert_eq!(
            call.lower_hand(B, &json!({}), at(1_005_000), &mut out),
            Err(Refusal::Bad)
        );
        // a hand goes with its party
        call.leave(C, &json!({}), at(1_006_000), &mut Out::default());
        assert!(call.party(C).hand.is_none());
        let mut one = call.clone();
        one.kind = Kind::OneToOne;
        assert_eq!(
            one.raise_hand(B, "x", &raise, at(1_007_000), &mut out),
            Err(Refusal::Missing)
        );
    }

    #[test]
    fn a_reaction_reaches_everyone_in_the_call_named_by_its_sender() {
        let mut call = meeting();
        waits(&mut call, "d");
        let body = json!({"messageContent": [
            {"type": "Reaction", "operationId": "o1", "payload": {"values": [{"name": "heart", "count": 1}]}},
            {"type": "Other"},
        ]});
        let mut out = Out::default();
        call.react(C, &body, at(1_003_000), &mut out).unwrap();
        assert_eq!(
            paths(&out),
            [
                ("A", "receiveMessage"),
                ("B", "receiveMessage"),
                ("C", "receiveMessage")
            ]
        );
        let told = &out.deliveries[0].body;
        assert_eq!(told["from"]["id"], "8:orgid:c");
        assert_eq!(told["payload"]["values"][0]["name"], "heart");
        assert_eq!(told["operationId"], "o1");
        assert_eq!(call.react(C, &json!({}), at(1_003_000), &mut out), Err(Refusal::Bad));
    }

    #[test]
    fn the_organizers_end_ends_it_for_everyone_and_anyone_elses_is_a_leave() {
        let mut call = meeting();
        let (d, _) = waits(&mut call, "d");
        let mut out = Out::default();
        call.end_meeting(B, &json!({}), at(1_003_000), &mut out);
        assert_eq!((call.phase, call.party(B).status), (Phase::Connected, Status::Left));
        let mut out = Out::default();
        call.end_meeting(Seat::CALLER, &json!({}), at(1_004_000), &mut out);
        assert_eq!(call.phase, Phase::Ended);
        assert!(out.room_over);
        let ends: Vec<&str> = out
            .deliveries
            .iter()
            .filter(|delivery| delivery.body["callEnd"]["phrase"] == ENDED_FOR_ALL)
            .map(|delivery| delivery.endpoint.as_str())
            .collect();
        assert_eq!(ends, ["A", "C", "D"]);
        assert_eq!(call.party(d).status, Status::Left);
    }

    #[test]
    fn the_saved_options_make_attendees_mute_them_and_tell_everyone() {
        let mut call = meeting();
        waits(&mut call, "d");
        let restricted = Controls {
            lobby: "OrganizerOnly".to_owned(),
            presenters: "OrganizerOnly".to_owned(),
            restrictions: "DisableAudio,DisableVideo".to_owned(),
        };
        let mut out = Out::default();
        call.set_controls(restricted.clone(), at(1_003_000), &mut out);
        assert!(call.audio_blocked(call.party(B)) && !call.audio_blocked(call.party(Seat::CALLER)));
        assert_eq!(
            (call.party(B).mute_version, call.party(Seat::CALLER).mute_version),
            (2, 0)
        );
        let updates: Vec<&Value> = out
            .deliveries
            .iter()
            .filter(|delivery| delivery.path.ends_with("conversationUpdate/"))
            .map(|delivery| &delivery.body)
            .collect();
        assert_eq!(updates.len(), 4);
        assert_eq!(
            updates[0]["meetingDetails"]["meetingCapability"],
            json!({"autoAdmittedUsers": "OrganizerOnly", "presenterOption": "OrganizerOnly",
                "attendeeRestrictions": "DisableAv"})
        );
        let entry = &out.deliveries[0].body["participants"]["8:orgid:b"];
        assert_eq!(entry["meetingRole"], "attendee");
        assert_eq!(
            entry["endpoints"]["ep-b"]["call"]["mediaStreams"][0]["serverMuted"],
            true
        );
        // the same options again tell nothing
        let mut out = Out::default();
        call.set_controls(restricted.clone(), at(1_004_000), &mut out);
        assert!(out.deliveries.is_empty());
        // a conversation names the options in effect; a group call's names none
        let answered = wire::conversation(ORIGIN, "emea", &call);
        assert_eq!(
            answered["meetingDetails"]["meetingCapability"]["presenterOption"],
            "OrganizerOnly"
        );
        assert_eq!(answered["state"]["conversationType"], "scheduledMeeting");
        call.set_controls(Controls::default(), at(1_005_000), &mut Out::default());
        let answered = wire::conversation(ORIGIN, "emea", &call);
        assert_eq!(
            answered["meetingDetails"]["meetingCapability"],
            json!({"autoAdmittedUsers": "EveryoneInCompany", "presenterOption": "Everyone",
                "attendeeRestrictions": "UnRestricted"})
        );
        assert_eq!(call.party(B).mute_version, 4);
        call.meeting = None;
        assert!(wire::conversation(ORIGIN, "emea", &call)["meetingDetails"].is_null());
        // a group call goes by no saved options
        let mut out = Out::default();
        call.set_controls(restricted, at(1_006_000), &mut out);
        assert!(out.deliveries.is_empty());
        assert_eq!(
            (call.party(B).mute_version, call.party(Seat::CALLER).mute_version),
            (4, 0)
        );
    }

    /// Whether the roster lists a sharing stream of the party at `seat`.
    fn shares(call: &Call, seat: Seat) -> bool {
        let party = call.party(seat);
        let entry = &wire::roster(call, &[seat])["participants"][party.mri()];
        entry["endpoints"]
            .as_object()
            .into_iter()
            .flat_map(|endpoints| endpoints.values())
            .filter_map(|endpoint| endpoint["call"]["mediaStreams"].as_array())
            .flatten()
            .any(|stream| stream["label"] == "applicationsharing-video")
    }

    #[test]
    fn a_share_takes_over_in_a_meeting_as_in_a_group_call_and_one_taken_out_shares_no_more() {
        let mut call = meeting();
        let (d, _) = waits(&mut call, "d");
        let sharing = Showing {
            camera: false,
            sharing: true,
        };
        call.group_described(B, sharing, at(1_003_000), &mut Out::default())
            .unwrap();
        let mut out = Out::default();
        call.group_described(C, sharing, at(1_004_000), &mut out).unwrap();
        assert!(shares(&call, C) && !shares(&call, B));
        assert_eq!(
            call.group_described(d, sharing, at(1_004_500), &mut Out::default()),
            Err(Refusal::Forbidden),
            "one in the lobby has no media"
        );
        let told = &out.deliveries[0].body["participants"];
        assert!(
            told.get("8:orgid:b").is_some() && told.get("8:orgid:c").is_some(),
            "{told}"
        );
        call.remove(Seat::CALLER, C, at(1_005_000), &mut Out::default())
            .unwrap();
        assert!(!shares(&call, C) && !shares(&call, B));
    }

    #[test]
    fn a_channels_meeting_never_posted_tells_no_end_and_a_posted_one_does() {
        let mut call = meeting();
        assert!(call.tells_chat_end());
        let channel = call.meeting.as_mut().unwrap();
        channel.channel = true;
        assert!(!call.tells_chat_end());
        call.meeting.as_mut().unwrap().post = Some("1700000000000".to_owned());
        assert!(call.tells_chat_end());
        call.meeting = None;
        assert!(call.tells_chat_end());
    }
}
