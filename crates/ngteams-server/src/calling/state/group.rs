//! A group call's own steps. The call is up from its start (the caller is in it at once, through the media node) and
//! goes on while anyone is in it; each party rings, answers, declines, joins and leaves alone. What the media node
//! answered is given to a step; the legs a step lets go of and the room it ends are in [`Out`], for the operation to
//! drop once the step is stored.

use serde_json::Value;

use super::{
    At, Call, KEEP_ALIVE, KEEP_ALIVE_SLACK, Kind, Leg, Out, Outcome, Party, Phase, Refusal, Seat, Status, Watcher,
};
use crate::calling::media::Sources;
use crate::calling::wire::{self, Links};

/// The first party's first source id; each party's sources are this far apart.
const FIRST_SOURCE: u32 = 1000;
const SOURCE_STEP: u32 = 10;

impl Call {
    /// The sources of the next party to get a leg.
    pub(crate) fn next_sources(&mut self) -> Sources {
        let audio = self.next_source.max(FIRST_SOURCE);
        self.next_source = audio + SOURCE_STEP;
        Sources {
            audio,
            video: audio + 1,
            sharing: audio + 2,
            data: audio + 3,
        }
    }

    /// The seats of the parties in the call.
    fn in_call_seats(&self) -> Vec<Seat> {
        (0..self.parties.len())
            .map(Seat)
            .filter(|seat| self.party(*seat).status == Status::InCall)
            .collect()
    }

    /// The seats a whole roster lists: whoever is or was in the call.
    pub(crate) fn rostered(&self) -> Vec<Seat> {
        (0..self.parties.len())
            .map(Seat)
            .filter(|seat| self.party(*seat).joined.is_some())
            .collect()
    }

    /// Looks at the call next at the earliest ring's end or its keep-alive's.
    pub(crate) fn reschedule(&mut self) {
        let rings = self
            .parties
            .iter()
            .filter(|party| party.status == Status::Ringing)
            .filter_map(|party| party.ring_due);
        self.due_at = rings.fold(self.alive_due, i64::min);
    }

    /// The call as its start makes it: the caller in it with its leg, each callee ringing until `ring_due`.
    pub(crate) fn started_group(&mut self, at: At<'_>, ring_due: i64) {
        self.kind = Kind::Group;
        self.phase = Phase::Connected;
        self.connected_at = Some(at.now);
        self.alive_due = at.seconds() + KEEP_ALIVE + KEEP_ALIVE_SLACK;
        let joined = wire::iso(at.now);
        for (number, party) in self.parties.iter_mut().enumerate() {
            if number == Seat::CALLER.0 {
                party.status = Status::InCall;
                party.joined = Some(joined.clone());
            } else {
                party.status = Status::Ringing;
                party.ring_due = Some(ring_due);
            }
        }
        self.reschedule();
    }

    /// What the start tells the caller: the media node's `answer` in its acceptance, and the first roster.
    pub(crate) fn tell_started(&mut self, origin: &str, answer: &Value, out: &mut Out) {
        let caller = self.party(Seat::CALLER);
        let accepted = wire::mixer_acceptance(origin, self, Seat::CALLER, answer);
        out.call_back(origin, caller.link("acceptance"), accepted);
        self.tell_group(origin, &[Seat::CALLER], Some(Seat::CALLER), out);
    }

    /// A ringing party's endpoint answers (its answer already with the media node): the party is in the call, its
    /// other endpoints stop ringing, and the roster tells everyone (the party the whole of it).
    pub(crate) fn group_accept(
        &mut self,
        seat: Seat,
        endpoint: &str,
        (links, body): (Links, &Value),
        at: At<'_>,
        out: &mut Out,
    ) -> Result<Value, Refusal> {
        self.ringing(seat)?;
        let acceptance = body.get("callAcceptance").ok_or(Refusal::Bad)?;
        let party = self.party_mut(seat);
        party.keep(endpoint, links);
        for other in party.endpoints.iter().filter(|known| known.endpoint != endpoint) {
            let end = wire::end(487, "Call cancelled as it was accepted by another fork.", None);
            out.call_back(at.origin, other.links.get("end").map(String::as_str), end);
        }
        party.endpoints.retain(|known| known.endpoint == endpoint);
        party.active = Some(endpoint.to_owned());
        if let Some(by) = acceptance.get("acceptedBy").filter(|by| by.is_object()) {
            party.participant = wire::named_by(by, party);
        }
        party.status = Status::InCall;
        party.ring_due = None;
        party.joined = Some(wire::iso(at.now));
        self.reschedule();
        self.tell_group(at.origin, &[seat], Some(seat), out);
        self.tell_presence(at.origin, wire::JOINED, seat, out);
        Ok(wire::accept_answer(at.origin, self, seat))
    }

    /// A ringing party declines: its endpoints stop ringing and its leg goes; the call goes on for the others.
    pub(crate) fn group_reject(&mut self, seat: Seat, body: &Value, at: At<'_>, out: &mut Out) -> Result<(), Refusal> {
        self.ringing(seat)?;
        let code = body.pointer("/callEnd/code").and_then(Value::as_i64).unwrap_or(603);
        let party = self.party_mut(seat);
        for known in &party.endpoints {
            let end = wire::end(code, "CallEndReasonLocalUserInitiated", Some(&party.participant));
            out.call_back(at.origin, known.links.get("end").map(String::as_str), end);
        }
        party.status = Status::Declined;
        Self::let_go(party, out);
        self.reschedule();
        Ok(())
    }

    /// A party in the call leaves: it leaves the roster and its leg goes. The last one out ends the call, and the rings
    /// still going end with it. A leave naming another endpoint of the party's than the one in the call changes
    /// nothing (it comes from an endpoint whose answer lost to the other's), nor does a leave of anyone not in it.
    pub(crate) fn group_leave(&mut self, seat: Seat, body: &Value, at: At<'_>, out: &mut Out) {
        let party = self.party(seat);
        let named = body.pointer("/participants/from/endpointId").and_then(Value::as_str);
        let own = party
            .participant
            .get("endpointId")
            .and_then(Value::as_str)
            .filter(|own| *own != wire::ZERO_ENDPOINT);
        let lost_fork = matches!((named, own), (Some(named), Some(own)) if named != own);
        if self.phase == Phase::Ended || party.status != Status::InCall || lost_fork {
            return;
        }
        self.take_out(seat, out);
        if self.in_call_seats().is_empty() {
            let end = wire::end(487, "Call ended as everyone left.", None);
            self.end_group(at, &end, out);
        } else {
            self.tell_group(at.origin, &[seat], None, out);
            self.tell_presence(at.origin, wire::LEFT, seat, out);
        }
    }

    /// The media node lost the party's media: it is taken out of the call, as a leave would, and its endpoint is told.
    #[cfg_attr(not(test), expect(dead_code, reason = "the media node (task 186b) makes this step"))]
    pub(crate) fn media_lost(&mut self, seat: Seat, at: At<'_>, out: &mut Out) {
        if self.phase == Phase::Ended || self.party(seat).status != Status::InCall {
            return;
        }
        let end = wire::failure(408, "The call's media was lost.");
        out.call_back(at.origin, self.party(seat).link("end"), end);
        let leave = serde_json::json!({});
        self.group_leave(seat, &leave, at, out);
    }

    /// The party leaves the call: out of the roster, its leg let go.
    fn take_out(&mut self, seat: Seat, out: &mut Out) {
        let party = self.party_mut(seat);
        party.status = Status::Left;
        party.active = None;
        party.endpoints.clear();
        party.answered = None;
        Self::let_go(party, out);
    }

    fn let_go(party: &mut Party, out: &mut Out) {
        party.ring_due = None;
        if let Some(leg) = party.leg.take() {
            out.released.push(leg.id);
        }
    }

    /// Ends the call: everyone in it gets `end`, every ring still going ends, every leg and the room go.
    fn end_group(&mut self, at: At<'_>, end: &Value, out: &mut Out) {
        let rung = wire::end(487, "Call cancelled as the call ended.", None);
        for seat in 0..self.parties.len() {
            let party = self.party_mut(Seat(seat));
            match party.status {
                Status::InCall => out.call_back(at.origin, party.link("end"), end.clone()),
                Status::Ringing => {
                    for known in &party.endpoints {
                        out.call_back(at.origin, known.links.get("end").map(String::as_str), rung.clone());
                    }
                    party.status = Status::Missed;
                }
                _ => {}
            }
            if party.status == Status::InCall {
                party.status = Status::Left;
            }
            Self::let_go(party, out);
        }
        out.room_over = true;
        let talked = (at.now - self.connected_at.unwrap_or(at.now)) / 1000;
        self.finish(at, Outcome::Talked(talked.max(0)), out);
    }

    /// What the sweeper makes of a group call that is due: the rings that are due end (the call goes on), a call whose
    /// keep-alives stopped is given up. `false` when nothing changed.
    pub(crate) fn group_lapse(&mut self, at: At<'_>, out: &mut Out) -> bool {
        if self.phase == Phase::Ended {
            return false;
        }
        if self.alive_due <= at.seconds() {
            let end = wire::failure(408, "Call timed out without a keep-alive.");
            self.end_group(at, &end, out);
            return true;
        }
        let rung = wire::end(487, "Call cancelled on timeout.", None);
        let mut changed = false;
        for party in &mut self.parties {
            let due = party.status == Status::Ringing && party.ring_due.is_some_and(|due| due <= at.seconds());
            if due {
                for known in &party.endpoints {
                    out.call_back(at.origin, known.links.get("end").map(String::as_str), rung.clone());
                }
                party.status = Status::Missed;
                Self::let_go(party, out);
                changed = true;
            }
        }
        self.reschedule();
        changed
    }

    /// Makes `account` a seat with a new leg `leg` (the leg it had before, if any, is answered for the operation to
    /// drop): the first step of a join or an add, before the media node is asked for the leg. Someone in the call
    /// already is refused.
    pub(crate) fn reserve(&mut self, account: &Party, leg: &str) -> Result<(Seat, Sources, Option<String>), Refusal> {
        if self.phase == Phase::Ended {
            return Err(Refusal::Gone);
        }
        let seat = match self.seat_of(&account.id) {
            Some(seat) if self.party(seat).status == Status::InCall => return Err(Refusal::Forbidden),
            Some(seat) => {
                let party = self.party_mut(seat);
                party.participant = account.participant.clone();
                party.name.clone_from(&account.name);
                seat
            }
            None => {
                self.parties.push(Party {
                    status: Status::Left,
                    ..account.clone()
                });
                Seat(self.parties.len() - 1)
            }
        };
        let sources = self.next_sources();
        let before = self.party_mut(seat).leg.replace(Leg {
            id: leg.to_owned(),
            sources,
        });
        Ok((seat, sources, before.map(|leg| leg.id).filter(|before| before != leg)))
    }

    /// A reservation whose leg the media node didn't make is let go.
    pub(crate) fn unreserve(&mut self, seat: Seat, leg: &str) {
        let party = self.party_mut(seat);
        if party.leg.as_ref().is_some_and(|kept| kept.id == leg) {
            party.leg = None;
        }
    }

    /// The party at `seat`, its leg `leg` made, joins with its endpoint: the media node's `answer` goes to its
    /// acceptance, the roster tells everyone (the joiner the whole of it).
    pub(crate) fn join(
        &mut self,
        seat: Seat,
        (leg, endpoint, links): (&str, &str, Links),
        answer: &Value,
        at: At<'_>,
        out: &mut Out,
    ) -> Result<(), Refusal> {
        let party = self.party(seat);
        if self.phase == Phase::Ended || party.leg.as_ref().is_none_or(|kept| kept.id != leg) {
            return Err(Refusal::Gone);
        }
        if party.status == Status::InCall {
            return Err(Refusal::Forbidden);
        }
        let was_ringing = party.status == Status::Ringing;
        let party = self.party_mut(seat);
        if was_ringing {
            for other in party.endpoints.iter().filter(|known| known.endpoint != endpoint) {
                let end = wire::end(487, "Call cancelled as it was joined from another fork.", None);
                out.call_back(at.origin, other.links.get("end").map(String::as_str), end);
            }
        }
        party.endpoints.retain(|known| known.endpoint == endpoint);
        party.keep(endpoint, links);
        party.active = Some(endpoint.to_owned());
        party.status = Status::InCall;
        party.ring_due = None;
        party.joined = Some(wire::iso(at.now));
        let accepted = wire::mixer_acceptance(at.origin, self, seat, answer);
        out.call_back(at.origin, self.party(seat).link("acceptance"), accepted);
        self.reschedule();
        self.tell_group(at.origin, &[seat], Some(seat), out);
        self.tell_presence(at.origin, wire::JOINED, seat, out);
        Ok(())
    }

    /// The party at `seat`, its leg `leg` made with the media node's offer, rings until `ring_due` (someone added
    /// it to the call).
    pub(crate) fn ring(&mut self, seat: Seat, leg: &str, ring_due: i64) -> Result<(), Refusal> {
        let party = self.party(seat);
        if self.phase == Phase::Ended || party.leg.as_ref().is_none_or(|kept| kept.id != leg) {
            return Err(Refusal::Gone);
        }
        if party.status == Status::InCall {
            return Err(Refusal::Forbidden);
        }
        let party = self.party_mut(seat);
        party.status = Status::Ringing;
        party.ring_due = Some(ring_due);
        party.endpoints.clear();
        party.active = None;
        self.reschedule();
        Ok(())
    }

    /// A chat member follows the roster from its endpoint (a subscription without media): told the whole roster now
    /// and its changes from then on.
    pub(crate) fn watch(
        &mut self,
        account: &str,
        endpoint: &str,
        links: Links,
        at: At<'_>,
        out: &mut Out,
    ) -> Result<(), Refusal> {
        if self.phase == Phase::Ended {
            return Err(Refusal::Gone);
        }
        let update = wire::roster(self, &self.rostered());
        out.call_back(at.origin, links.get("rosterUpdate").map(String::as_str), update);
        self.watchers
            .retain(|watcher| !(watcher.id == account && watcher.endpoint == endpoint));
        self.watchers.push(Watcher {
            id: account.to_owned(),
            endpoint: endpoint.to_owned(),
            links,
        });
        Ok(())
    }

    /// A party in the call offered its media again and the media node answered `answer`: the answer goes back to the
    /// party at its own callback. Never forwarded to anyone else.
    pub(crate) fn group_renegotiated(
        &mut self,
        seat: Seat,
        (endpoint, links): (&str, Links),
        answer: &Value,
        at: At<'_>,
        out: &mut Out,
    ) -> Result<(), Refusal> {
        self.in_call(seat)?;
        if !links.contains_key("mediaAnswer") {
            return Err(Refusal::Bad);
        }
        if self.turned_back(seat, &links, at, out) {
            return Ok(());
        }
        self.negotiations += 1;
        let number = self.negotiations;
        let party = self.party_mut(seat);
        party.keep(endpoint, links);
        party.answered = Some(number);
        let told = wire::mixer_media_answer(at.origin, self, seat, answer);
        out.call_back(at.origin, self.party(seat).link("mediaAnswer"), told);
        Ok(())
    }

    /// Whether the party's renegotiation is turned away (`491` at its `rejection` callback): its last answer isn't
    /// acknowledged yet.
    pub(crate) fn turned_back(&self, seat: Seat, links: &Links, at: At<'_>, out: &mut Out) -> bool {
        if self.party(seat).answered.is_none() {
            return false;
        }
        let failure = wire::negotiation_failure(491, "Negotiation is in progress.");
        out.call_back(at.origin, links.get("rejection").map(String::as_str), failure);
        true
    }

    /// Tells the roster's entries of `changed`: to each party in the call at its endpoint in it, to each ringing one at
    /// every endpoint that subscribed while it rang, and to every watcher. `whole` is told the whole roster instead.
    pub(crate) fn tell_group(&mut self, origin: &str, changed: &[Seat], whole: Option<Seat>, out: &mut Out) {
        self.roster_sequence += 1;
        let delta = wire::roster(self, changed);
        let full = wire::roster(self, &self.rostered());
        for (number, party) in self.parties.iter().enumerate() {
            let update = if whole == Some(Seat(number)) { &full } else { &delta };
            match party.status {
                Status::InCall => out.call_back(origin, party.link("rosterUpdate"), update.clone()),
                Status::Ringing => {
                    for known in &party.endpoints {
                        out.call_back(
                            origin,
                            known.links.get("rosterUpdate").map(String::as_str),
                            update.clone(),
                        );
                    }
                }
                _ => {}
            }
        }
        for watcher in &self.watchers {
            out.call_back(
                origin,
                watcher.links.get("rosterUpdate").map(String::as_str),
                delta.clone(),
            );
        }
    }

    /// Tells each calling endpoint of every party in the call or ringing, but the one that changed, that `seat` came
    /// in or left (`evt` 115 or 116 at the endpoint's root).
    fn tell_presence(&self, origin: &str, evt: u64, seat: Seat, out: &mut Out) {
        let told = wire::presence(origin, self, evt);
        let changed = self.party(seat).active.as_deref();
        for party in &self.parties {
            let endpoints = match party.status {
                Status::InCall | Status::Ringing => &party.endpoints,
                _ => continue,
            };
            for known in endpoints
                .iter()
                .filter(|known| Some(known.endpoint.as_str()) != changed)
            {
                out.deliveries.push(super::Delivery {
                    endpoint: known.endpoint.clone(),
                    path: String::new(),
                    body: told.clone(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use serde_json::json;

    use super::*;
    use crate::calling::media::MediaHome;
    use crate::calling::state::Delivery;

    const ORIGIN: &str = "https://own.example.test";

    fn link(endpoint: &str, name: &str) -> String {
        format!("{ORIGIN}/trouter/v4/f/{endpoint}/callAgent/agent/1/call/{name}/")
    }

    fn links(endpoint: &str, names: &[&str]) -> Links {
        names
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

    fn answer() -> Value {
        json!({"blob": "answer", "contentType": "application/sdp-ngc-1.0"})
    }

    /// a's group call to b, c and d at second 1000: their rings end at 1030; b and c attached, d has no endpoint.
    fn started() -> (Call, Out) {
        let mut caller = party("a");
        caller.keep("A", links("A", &["acceptance", "end", "rosterUpdate"]));
        caller.active = Some("A".to_owned());
        let mut call = Call {
            id: "call".to_owned(),
            conversation: "conversation".to_owned(),
            phase: Phase::Connected,
            parties: vec![caller, party("b"), party("c"), party("d")],
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
            chat: Some("19:group@thread.v2".to_owned()),
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
        };
        for seat in 0..4 {
            let sources = call.next_sources();
            call.party_mut(Seat(seat)).leg = Some(Leg {
                id: format!("leg{seat}"),
                sources,
            });
        }
        call.started_group(at(1_000_000), 1_030);
        let mut out = Out::default();
        call.tell_started(ORIGIN, &answer(), &mut out);
        for (seat, endpoint) in [(1, "B"), (1, "B2"), (2, "C")] {
            call.attach(
                Seat(seat),
                endpoint,
                links(endpoint, &["end", "rosterUpdate"]),
                at(1_001_000),
            )
            .unwrap();
        }
        (call, out)
    }

    fn accepted(call: &mut Call, seat: usize, endpoint: &str, now: i64) -> Out {
        let mut out = Out::default();
        let body = json!({"callAcceptance": {"acceptedBy": {"endpointId": format!("ep-{endpoint}")}}});
        let links = links(endpoint, &["mediaAnswer", "rejection"]);
        call.group_accept(Seat(seat), endpoint, (links, &body), at(now), &mut out)
            .unwrap();
        out
    }

    fn paths(out: &Out) -> Vec<(&str, &str)> {
        out.deliveries
            .iter()
            .map(|delivery| {
                let kind = delivery.path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
                (delivery.endpoint.as_str(), kind)
            })
            .collect()
    }

    fn evt(delivery: &Delivery) -> Option<u64> {
        delivery.body.get("evt").and_then(Value::as_u64)
    }

    #[test]
    fn a_start_takes_the_caller_in_at_once_and_rings_every_callee_until_its_due() {
        let (call, out) = started();
        assert_eq!(call.party(Seat::CALLER).status, Status::InCall);
        assert!((1..4).all(|seat| call.party(Seat(seat)).status == Status::Ringing));
        assert_eq!(call.due_at, 1_030, "the earliest ring's end");
        assert_eq!(paths(&out), [("A", "acceptance"), ("A", "rosterUpdate")]);
        let accepted = &out.deliveries[0].body["callAcceptance"];
        assert_eq!(accepted["mediaContent"]["fromMixer"], true);
        assert_eq!(accepted["mediaContent"]["mediaLegId"], "leg0");
        assert!(accepted["links"].get("hold").is_none(), "a group call has no hold");
        assert!(
            accepted["links"]["callLeg"]
                .as_str()
                .unwrap()
                .contains("/active/call/0/")
        );
        let roster = &out.deliveries[1].body["participants"]["8:orgid:a"];
        let streams = &roster["endpoints"]["ep-a"]["call"]["mediaStreams"];
        assert_eq!(streams[0]["sourceId"], 1000);
        assert_eq!(streams[1]["sourceId"], 1001);
        assert_eq!(streams[1]["direction"], "recvonly");
        let sources: Vec<u32> = (0..4)
            .map(|seat| call.party(Seat(seat)).leg.as_ref().unwrap().sources.audio)
            .collect();
        assert_eq!(sources, [1000, 1010, 1020, 1030]);
        // the caller rings nowhere: its links aren't a ringing leg's
        let mut caller = call.clone();
        assert_eq!(
            caller
                .attach(Seat::CALLER, "A", links("A", &["end"]), at(1_002_000))
                .unwrap_err(),
            Refusal::Forbidden
        );
    }

    #[test]
    fn an_answer_takes_the_callee_in_and_tells_everyone_while_the_others_still_ring() {
        let (mut call, _) = started();
        let out = accepted(&mut call, 1, "B", 1_005_000);
        assert_eq!(call.party(Seat(1)).status, Status::InCall);
        assert_eq!(call.party(Seat(1)).participant["endpointId"], "ep-B");
        let told = paths(&out);
        // its other endpoint stops ringing, a and the ringing c get the change, b the whole roster; 115 to the others
        assert_eq!(
            &told[..4],
            [
                ("B2", "end"),
                ("A", "rosterUpdate"),
                ("B", "rosterUpdate"),
                ("C", "rosterUpdate")
            ]
        );
        assert_eq!(out.deliveries[2].body["participants"].as_object().unwrap().len(), 2);
        assert_eq!(out.deliveries[1].body["participants"].as_object().unwrap().len(), 1);
        let presence: Vec<&str> = out
            .deliveries
            .iter()
            .filter(|delivery| evt(delivery) == Some(wire::JOINED))
            .map(|delivery| delivery.endpoint.as_str())
            .collect();
        assert_eq!(presence, ["A", "C"]);
        assert_eq!(call.due_at, 1_030, "c and d still ring");
        // answered: it rings no more; a second answer is too late
        let body = json!({"callAcceptance": {}});
        assert_eq!(
            call.group_accept(Seat(1), "B2", (Links::new(), &body), at(1_006_000), &mut Out::default()),
            Err(Refusal::Gone)
        );
        assert_eq!(
            call.group_accept(
                Seat(2),
                "C",
                (Links::new(), &json!({})),
                at(1_006_000),
                &mut Out::default()
            ),
            Err(Refusal::Bad)
        );
    }

    #[test]
    fn a_decline_and_a_ring_that_runs_out_let_their_legs_go_and_the_call_runs_on() {
        let (mut call, _) = started();
        let mut out = Out::default();
        call.reject(Seat(2), &json!({}), at(1_005_000), &mut out).unwrap();
        assert_eq!(paths(&out), [("C", "end")]);
        assert_eq!(out.deliveries[0].body["callEnd"]["code"], 603);
        assert_eq!(
            (call.party(Seat(2)).status, out.released.clone()),
            (Status::Declined, vec!["leg2".to_owned()])
        );
        assert_eq!(
            call.reject(Seat(2), &json!({}), at(1_005_000), &mut Out::default()),
            Err(Refusal::Gone)
        );
    }

    #[test]
    fn a_ring_that_runs_out_lets_its_leg_go_and_the_call_runs_on() {
        let (mut call, _) = started();
        call.reject(Seat(2), &json!({}), at(1_005_000), &mut Out::default())
            .unwrap();
        // nothing is due before 1030
        let mut out = Out::default();
        assert!(!call.lapse(at(1_029_000), &mut out));
        assert!(call.lapse(at(1_030_000), &mut out));
        assert_eq!(paths(&out), [("B", "end"), ("B2", "end")]);
        assert_eq!(out.released, ["leg1", "leg3"]);
        assert!(!out.room_over && call.phase == Phase::Connected);
        assert_eq!(call.due_at, call.alive_due, "only the keep-alive is due now");
    }

    #[test]
    fn a_call_whose_keep_alives_stopped_is_given_up() {
        let (mut call, _) = started();
        call.lapse(at(1_030_000), &mut Out::default());
        // a call whose keep-alives stopped is given up
        let mut out = Out::default();
        assert!(!call.lapse(at(1_100_000), &mut out), "nothing is due");
        assert!(call.lapse(at(call.alive_due * 1000), &mut out));
        assert_eq!(paths(&out), [("A", "end")]);
        assert_eq!(out.deliveries[0].body["callEnd"]["code"], 408);
        assert!(out.room_over && call.phase == Phase::Ended);
        assert!(!call.group_lapse(at(9_999_999_000), &mut Out::default()));
    }

    #[test]
    fn leaves_take_parties_out_until_the_last_one_ends_the_call() {
        let (mut call, _) = started();
        accepted(&mut call, 1, "B", 1_005_000);
        // a ringing party's leave, and one from an endpoint whose answer lost, change nothing
        let before = call.clone();
        call.leave(Seat(2), &json!({}), at(1_006_000), &mut Out::default());
        let lost = json!({"participants": {"from": {"endpointId": "ep-B2"}}});
        call.leave(Seat(1), &lost, at(1_006_000), &mut Out::default());
        assert_eq!(call, before);
        // the caller leaves: b goes on, told the caller left
        let mut out = Out::default();
        call.leave(Seat::CALLER, &json!({}), at(1_010_000), &mut out);
        assert_eq!(call.party(Seat::CALLER).status, Status::Left);
        assert_eq!(out.released, ["leg0"]);
        let entry = &out.deliveries[0].body["participants"]["8:orgid:a"];
        assert_eq!(
            (
                entry["state"].as_str(),
                entry["endpoints"].as_object().map(serde_json::Map::len)
            ),
            (Some("inactive"), Some(0))
        );
        assert!(
            out.deliveries
                .iter()
                .any(|delivery| evt(delivery) == Some(wire::LEFT) && delivery.endpoint == "B")
        );
        assert!(out.ended.is_none());
        // the last one leaves: the rings still going end, every leg and the room go
        let mut out = Out::default();
        call.leave(Seat(1), &json!({}), at(1_070_000), &mut out);
        assert_eq!(call.phase, Phase::Ended);
        assert_eq!(out.ended, Some(Outcome::Talked(70)));
        assert!(out.room_over);
        assert_eq!(out.released, ["leg1", "leg2", "leg3"]);
        assert_eq!(paths(&out), [("C", "end")]);
        assert_eq!(call.rostered(), [Seat(0), Seat(1)]);
        call.leave(Seat(1), &json!({}), at(1_071_000), &mut Out::default());
    }

    #[test]
    fn a_lost_leg_takes_its_party_out_and_tells_its_endpoint() {
        let (mut call, _) = started();
        accepted(&mut call, 1, "B", 1_005_000);
        let mut out = Out::default();
        call.media_lost(Seat(1), at(1_010_000), &mut out);
        assert_eq!(out.deliveries[0].body["callEnd"]["code"], 408);
        assert_eq!(out.deliveries[0].endpoint, "B");
        assert_eq!(call.party(Seat(1)).status, Status::Left);
        let mut out = Out::default();
        call.media_lost(Seat(2), at(1_011_000), &mut out);
        assert!(out.deliveries.is_empty(), "c isn't in the call");
    }

    #[test]
    fn a_join_and_an_add_reserve_a_seat_first_and_take_it_with_the_leg_they_made() {
        let (mut call, _) = started();
        // e joins: a new seat, then in the call with the node's answer at its own acceptance
        let (seat, sources, before) = call.reserve(&party("e"), "leg-e").unwrap();
        assert_eq!((seat, sources.audio, before), (Seat(4), 1040, None));
        assert_eq!(
            call.join(
                seat,
                ("other", "E", Links::new()),
                &answer(),
                at(1_002_000),
                &mut Out::default()
            ),
            Err(Refusal::Gone),
            "a leg it didn't reserve"
        );
        let mut out = Out::default();
        let joined = links("E", &["acceptance", "rosterUpdate"]);
        call.join(seat, ("leg-e", "E", joined), &answer(), at(1_002_000), &mut out)
            .unwrap();
        assert_eq!(paths(&out)[0], ("E", "acceptance"));
        assert_eq!(
            out.deliveries[0].body["callAcceptance"]["mediaContent"]["mediaLegId"],
            "leg-e"
        );
        assert_eq!(call.reserve(&party("e"), "again").unwrap_err(), Refusal::Forbidden);
    }

    #[test]
    fn a_ringing_callee_joins_instead_of_answering_with_a_new_leg() {
        let (mut call, _) = started();
        // rung, c is named by its invitation, which names no endpoint of its
        call.party_mut(Seat(2)).participant["endpointId"] = json!(wire::ZERO_ENDPOINT);
        // a ringing c joins instead of answering: its new leg replaces the one it rang with
        let (seat, _, before) = call.reserve(&party("c"), "leg-c2").unwrap();
        assert_eq!((seat, before.as_deref()), (Seat(2), Some("leg2")));
        let mut out = Out::default();
        call.join(
            seat,
            ("leg-c2", "C9", links("C9", &["acceptance"])),
            &answer(),
            at(1_003_000),
            &mut out,
        )
        .unwrap();
        assert_eq!(paths(&out)[0], ("C", "end"));
        assert_eq!(call.party(seat).participant["endpointId"], "ep-c", "named as it joined");
        let named = json!({"participants": {"from": {"endpointId": "ep-c"}}});
        assert_eq!(
            call.join(seat, ("leg-c2", "C9", Links::new()), &answer(), at(1_003_000), &mut out),
            Err(Refusal::Forbidden)
        );
        call.leave(seat, &named, at(1_004_000), &mut Out::default());
        assert_eq!(call.party(seat).status, Status::Left, "its own leave takes it out");
    }

    #[test]
    fn an_added_member_rings_with_the_leg_reserved_for_it() {
        let (mut call, _) = started();
        let (seat, _, _) = call.reserve(&party("e"), "leg-e").unwrap();
        call.join(
            seat,
            ("leg-e", "E", Links::new()),
            &answer(),
            at(1_002_000),
            &mut Out::default(),
        )
        .unwrap();
        // f is added: it rings with the leg reserved for it
        let (seat, _, _) = call.reserve(&party("f"), "leg-f").unwrap();
        assert_eq!(call.ring(seat, "other", 1_050), Err(Refusal::Gone));
        call.ring(seat, "leg-f", 1_050).unwrap();
        assert_eq!(call.party(seat).status, Status::Ringing);
        assert_eq!(call.ring(Seat(4), "leg-e", 1_050), Err(Refusal::Forbidden));
        // a reservation the node didn't make a leg for is let go
        let (seat, _, _) = call.reserve(&party("g"), "leg-g").unwrap();
        call.unreserve(seat, "other");
        assert!(call.party(seat).leg.is_some());
        call.unreserve(seat, "leg-g");
        assert!(call.party(seat).leg.is_none());
        let mut over = call.clone();
        over.phase = Phase::Ended;
        assert_eq!(over.reserve(&party("h"), "leg-h").unwrap_err(), Refusal::Gone);
        assert_eq!(over.ring(Seat(5), "leg-f", 1_050), Err(Refusal::Gone));
        assert_eq!(
            over.watch("h", "H", Links::new(), at(1_004_000), &mut Out::default()),
            Err(Refusal::Gone)
        );
    }

    #[test]
    fn a_watcher_is_told_the_whole_roster_and_its_changes() {
        let (mut call, _) = started();
        let mut out = Out::default();
        call.watch("w", "W", links("W", &["rosterUpdate"]), at(1_002_000), &mut out)
            .unwrap();
        assert_eq!(paths(&out), [("W", "rosterUpdate")]);
        assert!(out.deliveries[0].body["participants"].get("8:orgid:a").is_some());
        call.watch(
            "w",
            "W",
            links("W", &["rosterUpdate"]),
            at(1_002_000),
            &mut Out::default(),
        )
        .unwrap();
        assert_eq!(call.watchers.len(), 1, "a watcher's endpoint is kept once");
        let out = accepted(&mut call, 2, "C", 1_003_000);
        assert!(paths(&out).contains(&("W", "rosterUpdate")));
    }

    #[test]
    fn a_renegotiation_is_answered_to_its_party_alone_and_one_at_a_time() {
        let (mut call, _) = started();
        accepted(&mut call, 1, "B", 1_005_000);
        let offered = links("B", &["mediaAnswer", "rejection"]);
        assert_eq!(
            call.group_renegotiated(
                Seat(2),
                ("C", offered.clone()),
                &answer(),
                at(1_006_000),
                &mut Out::default()
            ),
            Err(Refusal::Forbidden),
            "c isn't in the call"
        );
        assert_eq!(
            call.group_renegotiated(
                Seat(1),
                ("B", Links::new()),
                &answer(),
                at(1_006_000),
                &mut Out::default()
            ),
            Err(Refusal::Bad)
        );
        let mut out = Out::default();
        call.group_renegotiated(Seat(1), ("B", offered.clone()), &answer(), at(1_006_000), &mut out)
            .unwrap();
        assert_eq!(paths(&out), [("B", "mediaAnswer")]);
        let told = &out.deliveries[0].body["mediaAnswer"];
        assert_eq!(told["mediaContent"]["fromMixer"], true);
        assert!(
            told["links"]["mediaAcknowledgement"]
                .as_str()
                .unwrap()
                .contains("/mediaAnswers/call/1/t/1/")
        );
        // the next waits for the acknowledgement
        let mut out = Out::default();
        call.group_renegotiated(Seat(1), ("B", offered.clone()), &answer(), at(1_007_000), &mut out)
            .unwrap();
        assert_eq!(paths(&out), [("B", "rejection")]);
        assert_eq!(out.deliveries[0].body["mediaNegotiationFailure"]["code"], 491);
        call.acknowledge_answer(Seat(1), 2, at(1_008_000), &mut Out::default())
            .unwrap();
        assert_eq!(
            call.party(Seat(1)).answered,
            Some(1),
            "another negotiation's acknowledgement"
        );
        call.acknowledge_answer(Seat(1), 1, at(1_008_000), &mut Out::default())
            .unwrap();
        assert!(call.party(Seat(1)).answered.is_none());
        // what 1:1 calls do with their media and holds isn't a group call's
        let body = json!({"mediaNegotiation": {"mediaContent": {"blob": "x", "contentType": "y"}}});
        assert_eq!(
            call.renegotiate(Seat(1), "B", (offered, &body), at(1_009_000), &mut Out::default()),
            Err(Refusal::Forbidden)
        );
        assert_eq!(
            call.hold(Seat(1), "B", Links::new(), at(1_009_000), &mut Out::default())
                .unwrap_err(),
            Refusal::Forbidden
        );
        assert_eq!(
            call.resume(Seat(1), "B", Links::new(), at(1_009_000), &mut Out::default()),
            Err(Refusal::Forbidden)
        );
    }

    #[test]
    fn a_keep_alive_and_a_mute_of_a_party_in_the_call() {
        let (mut call, _) = started();
        assert_eq!(
            call.keep_alive(Seat(1), at(1_002_000)),
            Err(Refusal::Forbidden),
            "b still rings"
        );
        call.keep_alive(Seat::CALLER, at(2_000_000)).unwrap();
        assert_eq!(call.alive_due, 2_000 + KEEP_ALIVE + KEEP_ALIVE_SLACK);
        assert_eq!(call.due_at, 1_030);
        let mut out = Out::default();
        let muted = json!({"endpointState": {"state": {"isMuted": true}, "endpointStateSequenceNumber": 2}});
        call.endpoint_state(Seat::CALLER, &muted, at(2_001_000), &mut out)
            .unwrap();
        assert_eq!(
            paths(&out),
            [
                ("A", "rosterUpdate"),
                ("B", "rosterUpdate"),
                ("B2", "rosterUpdate"),
                ("C", "rosterUpdate")
            ]
        );
    }

    #[test]
    fn a_group_calls_invitation_and_presence_read_as_the_client_and_the_captures_have_them() {
        let (call, _) = started();
        let offer = json!({"blob": "offer to b", "contentType": "application/sdp-ngc-1.0"});
        let invitation = wire::group_invitation(ORIGIN, &call, Seat(1), ("tenant", "own"), &offer);
        assert_eq!(invitation["evt"], 109);
        let gp = base64::engine::general_purpose::STANDARD
            .decode(invitation["gp"].as_str().unwrap())
            .unwrap();
        let gp: Value = serde_json::from_slice(&gp).unwrap();
        let notification = &gp["callNotification"];
        assert_eq!(notification["mediaContent"]["blob"], "offer to b");
        assert_eq!(notification["mediaContent"]["mediaLegId"], "leg1");
        assert_eq!(notification["conversationType"], "groupCall");
        assert!(
            notification["links"]["attach"]
                .as_str()
                .unwrap()
                .contains("/forked/call/1/i1/1/attach")
        );
        assert_eq!(gp["groupChat"]["threadId"], "19:group@thread.v2");
        assert_eq!(gp["conversationInvitation"]["groupCallInitiator"], "8:orgid:a");
        assert_eq!(gp["conversationInvitation"]["isMultiParty"], true);
    }

    #[test]
    fn a_group_calls_presence_and_conversation_read_as_the_captures_have_them() {
        let (call, _) = started();
        let presence = wire::presence(ORIGIN, &call, wire::JOINED);
        assert_eq!(presence["evt"], 115);
        let packed = base64::engine::general_purpose::STANDARD
            .decode(presence["cp"].as_str().unwrap())
            .unwrap();
        let mut text = String::new();
        std::io::Read::read_to_string(&mut flate2::read::GzDecoder::new(packed.as_slice()), &mut text).unwrap();
        let content: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(content["participants"][0]["id"], "8:orgid:a");
        let conversation = wire::conversation(ORIGIN, "own", &call);
        assert_eq!(conversation["state"]["isMultiParty"], true);
        assert!(
            conversation["links"]["addParticipantAndModality"]
                .as_str()
                .unwrap()
                .ends_with("/add")
        );
        assert!(conversation["roster"]["participants"].get("8:orgid:a").is_some());
        let refused = wire::running_elsewhere(ORIGIN, &call);
        assert_eq!(refused["conversationEnd"]["subCode"], 5704);
        assert_eq!(wire::add_failure("8:orgid:x")["participants"]["8:orgid:x"]["code"], 580);
    }
}
