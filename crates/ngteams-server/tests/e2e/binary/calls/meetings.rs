//! Meetings as users meet them on an own server (QA of task 298): the real `ngteams-server` binary (`serve --role
//! all` on SQLite, or `--role api` and `--role media` as two processes on one Postgres and Redis), the run's coturn,
//! and each account's client as the app runs it ([`Phone`], with the calendar's and CSA's bases as the app has them).
//! All people are synthetic, with odd names. The meetings invite synthetic people with no client too.
//!
//! - A scheduled meeting is joined from its calendar event, from its chat and with Join with an ID. A wrong passcode
//!   finds nothing. Nobody rings. The roster shows the organizer. The audio is mixed. Cameras, a share and a second
//!   sharer who takes it over. A raised hand and a reaction. The organizer's Mute all, then an own unmute. End meeting
//!   ends it for everyone. The chat shows its start and its end. Nobody gets a call log entry or a missed call.
//! - The lobby: with "only invited people" bypass, an outsider who joins by ID waits. A presenter admits one, the
//!   organizer turns one away. With "only the organizer", an invited guest who comes first waits until the organizer
//!   comes and lets it in.
//! - The organizer's Controls bind everyone at once: attendees, then the attendees' microphones off, then all present.
//! - Raised hands and reactions in a group call.
//! - A channel's Meet now: posted, titled from the pre-join, joined from the post; the post shows its end.

use std::time::Duration;

use reqwest::Method;
use serde_json::json;
use teams_core::engine::Event;
use teams_core::{CallAction, CallId, CallParticipant, CallStatus, CallView, End, EventWrite};
use teams_media::session::SHARING_LINE;
use teams_media::tone::Tone;
use teams_proto::chat::ChatReads;
use teams_proto::{CalendarEvent, MeetingOption, Mri, NewTeam, TeamRole, TeamWrite};

use super::group::{Group, Layout, group_chat};
use super::support::{FRAMES, Line, Phone, WAIT, brief};
use crate::calendar::meetings::invited;
use crate::calendar::{DAY, HOUR, MONDAY, draft};

/// A tone of the mix is heard at least this loud (the mix of three 0.5 tones is soft-clipped).
const HEARD: f32 = 0.1;
/// Below this, a tone isn't in a recording.
const ABSENT: f32 = 0.05;
/// How long the media runs before what was heard is judged.
const TALK: Duration = Duration::from_secs(4);
/// How long the media runs after a change before it is judged.
const SETTLE: Duration = Duration::from_secs(5);
/// How long a check waits for something that must not come.
const QUIET: Duration = Duration::from_secs(3);

/// The tests here run one at a time, also beside each other (`--test-threads 4`): each one runs many clients' media.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const fn tone(hz: f32) -> Tone {
    Tone { hz, amplitude: 0.5 }
}

/// The people's tones: none a low harmonic of another's (a clipped mix has odd harmonics).
const TONES: [Tone; 5] = [tone(440.0), tone(660.0), tone(880.0), tone(1170.0), tone(1530.0)];

/// The people with a client: a organizes; e is outside every meeting until it joins by ID.
const PEOPLE: [(&str, &str); 5] = [
    ("ada@own.test", "Ada Ł. Lovelace"),
    ("bert@own.test", "Bert 🎺 Bach"),
    ("cora@own.test", "Cora 'Cee' Nuñez"),
    ("dov@own.test", "Dov ד׳ Levi"),
    ("eve@own.test", "Eve <i>Guest</i> Ωmega"),
];

const SUBJECT: &str = "Q4 ✨ «planning» & co";

/// The `index`th person's client, as the app runs it: its tone, the calendar's and CSA's bases.
async fn phone(group: &Group, index: usize) -> Phone {
    let (upn, name) = &group.people[index];
    let mut phone = Phone::signed_in(&group.api, &group.origin, (upn, name), TONES[index]).await;
    phone.served();
    phone.start();
    phone.online(0).await;
    phone
}

async fn phones(group: &Group, count: usize) -> Vec<Phone> {
    let mut phones = Vec::new();
    for index in 0..count {
        phones.push(phone(group, index).await);
    }
    phones
}

/// The Teams meeting `organizer` schedules as `subject`, with `guests` and every synthetic person invited (they have
/// no client), once each of them has it.
async fn schedule(group: &Group, organizer: &Phone, guests: &[&Phone], subject: &str) -> CalendarEvent {
    let mut meeting = draft(subject, MONDAY + 9 * HOUR, 30);
    meeting.online = true;
    meeting.attendees = guests
        .iter()
        .map(|guest| invited(&guest.upn, "QA Operator", Some(guest.mri.as_str()), true))
        .collect();
    for (index, mri) in group.synthetic.iter().enumerate() {
        let upn = format!("odd.{index}@own.test");
        meeting
            .attendees
            .push(invited(&upn, "QA Operator", Some(mri.as_str()), false));
    }
    let written = organizer
        .engine()
        .write_event(EventWrite::Create(meeting))
        .await
        .unwrap();
    let event = written.created.unwrap();
    organizer.sync().await;
    for guest in guests {
        guest.sync().await;
    }
    event
}

/// The organizer saves the meeting's "Who can bypass the lobby" (`AutoAdmittedUsers`) as `value` on the meeting
/// options page (the native client opens that page in a browser).
async fn lobby_bypass(group: &Group, organizer: &Phone, event: &CalendarEvent, value: &str) {
    let info = event.meeting.as_ref().unwrap();
    let chat = event.meeting_chat.as_ref().unwrap();
    let path = format!(
        "/beta/meetings/v1/options/{}/{}/{chat}/0/0",
        info.tenant_id,
        organizer.oid()
    );
    let body = json!({"options": [{"name": "AutoAdmittedUsers", "currentValue": value}]});
    let (status, answer) =
        super::super::calendar::raw(&group.api, &organizer.upn, Method::POST, &path, Some(body)).await;
    assert_eq!(status, 200, "{answer}");
}

/// The Join of the meeting's card in `phone`'s calendar (`subject`), read from the service as the calendar shows it.
async fn join_from_calendar(phone: &Phone, subject: &str) -> CallAction {
    let events = phone.engine().calendar(MONDAY, MONDAY + DAY).await.unwrap();
    let card = events
        .iter()
        .find(|event| event.subject == subject)
        .unwrap_or_else(|| panic!("{}: no {subject:?} in the calendar", phone.upn));
    CallAction::JoinMeeting {
        chat: card.meeting_chat.clone().expect("a Teams meeting has a chat"),
        meeting: card.meeting.clone().expect("a Teams meeting names its organizer"),
    }
}

/// The Join of the meeting chat's header: the meeting read from its chat, as the header reads it.
async fn join_from_chat(phone: &Phone, chat: &str, subject: &str) -> CallAction {
    let details = phone
        .engine()
        .meeting(chat.to_owned())
        .await
        .unwrap()
        .unwrap_or_else(|| panic!("{}: the chat names no meeting", phone.upn));
    assert_eq!(details.title.as_deref(), Some(subject), "{}", phone.upn);
    CallAction::JoinMeeting {
        chat: chat.to_owned(),
        meeting: details.meeting,
    }
}

/// `phone` presses `action` and its call comes to `wanted`.
async fn join(phone: &mut Phone, action: CallAction, wanted: &CallStatus) -> CallId {
    let mark = phone.mark();
    let call = phone.act(action).await;
    phone.status(mark, call, wanted).await;
    call
}

/// `phone` joins with its calendar card's Join (`subject`) and its call comes to `wanted`.
async fn join_card(phone: &mut Phone, subject: &str, wanted: &CallStatus) -> CallId {
    let action = join_from_calendar(phone, subject).await;
    join(phone, action, wanted).await
}

/// Nothing rang at any of `phones`, from its start on.
fn never_rang(phones: &[&Phone]) {
    for phone in phones {
        let rang: Vec<String> = phone
            .log
            .iter()
            .filter(|event| matches!(event, Event::IncomingCall(_) | Event::RingEnded { .. }))
            .map(brief)
            .collect();
        assert!(rang.is_empty(), "{} rang: {rang:?}", phone.upn);
    }
}

/// `phone`'s latest view of `call`.
fn latest(phone: &Phone, call: CallId) -> Option<CallView> {
    phone.log.iter().rev().find_map(|event| match event {
        Event::CallParticipants { call: of, view } if *of == call => Some(view.clone()),
        _ => None,
    })
}

fn party<'a>(view: &'a CallView, mri: &str) -> Option<&'a CallParticipant> {
    view.participants.iter().find(|party| party.mri == mri)
}

/// Pumps `phone`'s events until `done` holds of it; fails after [`WAIT`].
async fn until(phone: &mut Phone, what: &str, done: impl Fn(&Phone) -> bool) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while !done(phone) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{}: {what} didn't happen in time; its lines {:?}; its events {:?}",
            phone.upn,
            phone.video.lines(),
            phone.log.iter().rev().take(30).map(brief).collect::<Vec<_>>()
        );
        let mark = phone.mark();
        phone.quiet(mark, Duration::from_millis(100), |_| false).await;
    }
}

/// Until `phone`'s latest view of `call` is `wanted` (it may be already).
async fn shows(phone: &mut Phone, call: CallId, what: &str, wanted: impl Fn(&CallView) -> bool) {
    until(phone, what, |phone| latest(phone, call).as_ref().is_some_and(&wanted)).await;
}

/// Until `phone`'s view of `call` shows each of `others`.
async fn sees_people(phone: &mut Phone, call: CallId, others: &[&str]) {
    let what = format!("{} others in the call", others.len());
    shows(phone, call, &what, |view| {
        others.iter().all(|mri| party(view, mri).is_some())
    })
    .await;
}

/// The last event since `mark` that `pick` takes, waited for.
async fn told<T>(phone: &mut Phone, mark: usize, what: &str, pick: impl Fn(&Event) -> Option<T>) -> T {
    let at = phone.since(mark, what, |event| pick(event).is_some()).await;
    pick(&phone.log[at]).unwrap()
}

/// Until `phone`'s lobby of `call` is told as `waiting` (by MRI), since `mark`.
async fn lobby_is(phone: &mut Phone, mark: usize, call: CallId, waiting: &[&str]) {
    let what = format!("the lobby {waiting:?}");
    told(phone, mark, &what, |event| match event {
        Event::Lobby {
            call: of,
            waiting: told,
        } if *of == call => {
            let told: Vec<&str> = told.iter().map(|person| person.mri.as_str()).collect();
            (told == waiting).then_some(())
        }
        _ => None,
    })
    .await;
}

/// Until `phone` is told that its meeting `call` is as `wanted` says, since `mark`.
async fn meeting_is(
    phone: &mut Phone,
    mark: usize,
    call: CallId,
    what: &str,
    wanted: impl Fn(&teams_call::MeetingState) -> bool,
) {
    told(phone, mark, what, |event| match event {
        Event::CallMeeting { call: of, state } if *of == call && wanted(state) => Some(()),
        _ => None,
    })
    .await;
}

/// Until `phone` is told that `from` reacted with `reaction` in `call`, since `mark`.
async fn reaction_told(phone: &mut Phone, mark: usize, call: CallId, from: &str, reaction: &str) {
    let what = format!("{from}'s {reaction}");
    told(phone, mark, &what, |event| match event {
        Event::CallReaction {
            call: of,
            from: by,
            reaction: told,
            ..
        } if *of == call && by.as_str() == from && told == reaction => Some(()),
        _ => None,
    })
    .await;
}

/// Each `(listener, tones it hears, tones it doesn't)`: measured over the last [`TALK`], again over the next ones
/// while a heard tone is too quiet (a loaded host starves a client's media for a second now and then).
async fn judge(sides: &[(&Phone, &[Tone], &[Tone])], what: &str) {
    let measure = || {
        sides
            .iter()
            .map(|(phone, heard, absent)| {
                (
                    heard.iter().map(|tone| phone.heard(*tone, TALK)).collect::<Vec<_>>(),
                    absent.iter().map(|tone| phone.heard(*tone, TALK)).collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>()
    };
    let ok = |levels: &[(Vec<f32>, Vec<f32>)]| {
        levels
            .iter()
            .all(|(heard, absent)| heard.iter().all(|l| *l > HEARD) && absent.iter().all(|l| *l < ABSENT))
    };
    let mut levels = measure();
    for _ in 0..3 {
        if ok(&levels) {
            break;
        }
        eprintln!("{what}: measured again over the next {TALK:?}: {levels:.3?}");
        tokio::time::sleep(TALK).await;
        levels = measure();
    }
    for ((phone, heard, absent), (levels_heard, levels_absent)) in sides.iter().zip(&levels) {
        let hz = |tones: &[Tone]| tones.iter().map(|tone| tone.hz).collect::<Vec<_>>();
        eprintln!(
            "{what}: {} heard {:?} Hz at {levels_heard:.3?}, not {:?} Hz at {levels_absent:.3?}",
            phone.upn,
            hz(heard),
            hz(absent)
        );
    }
    assert!(ok(&levels), "{what}: {levels:.3?}");
}

/// `leaver` hangs up: its call ends as hung up.
async fn hang_up(leaver: &mut Phone, call: CallId) {
    let mark = leaver.mark();
    leaver.act(CallAction::HangUp { call }).await;
    assert_eq!(leaver.ended(mark, call).await, End::HungUp, "{}", leaver.upn);
}

fn started(line: &Line) -> bool {
    *line == Line::Started
}

/// The end of a call exactly `people` were in.
fn ended_with(people: &[String]) -> impl Fn(&Line) -> bool + '_ {
    move |line| {
        matches!(line, Line::Ended { seconds: Some(_), people: in_it }
            if in_it.len() == people.len() && people.iter().all(|one| in_it.contains(one)))
    }
}

/// What `phone`'s own streams hold of calls: its call log's entries and its activity feed's missed calls.
async fn call_traces(phone: &Phone) -> Vec<String> {
    let client = phone.client();
    let reads = ChatReads::new(&client);
    let log = reads.messages("48:calllogs", 20).await.unwrap();
    let feed = reads.messages("48:notifications", 50).await.unwrap();
    let missed = feed.messages.into_iter().filter(|item| {
        item.properties
            .get("activity")
            .is_some_and(|activity| activity["activitySubtype"] == "missedCall")
    });
    log.messages
        .into_iter()
        .chain(missed)
        .map(|message| message.content)
        .collect()
}

/// For [`QUIET`], none of `phones` has a call log entry or a missed call.
async fn no_call_traces(phones: &[&Phone]) {
    let deadline = tokio::time::Instant::now() + QUIET;
    while tokio::time::Instant::now() < deadline {
        for phone in phones {
            let traces = call_traces(phone).await;
            assert!(traces.is_empty(), "{}: {traces:?}", phone.upn);
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// The video lines of `phone` (not the sharing line) that showed at least [`FRAMES`] frames.
fn lines_with_video(phone: &Phone) -> usize {
    phone
        .video
        .lines()
        .iter()
        .filter(|(line, seen)| **line != SHARING_LINE && seen.frames >= FRAMES)
        .count()
}

/// `viewer` sees `sharer` share, and [`FRAMES`] more frames on its sharing line.
async fn sees_share(viewer: &mut Phone, call: CallId, sharer: &str) {
    let what = format!("{sharer} sharing");
    shows(viewer, call, &what, |view| {
        party(view, sharer).is_some_and(|p| p.sharing)
    })
    .await;
    let since = viewer.video.line(SHARING_LINE).frames;
    until(viewer, &format!("{sharer}'s screen"), |phone| {
        phone.video.line(SHARING_LINE).frames >= since + FRAMES
    })
    .await;
}

// --- a scheduled meeting, from the first join to its end ---

/// The people of [`scheduled`]: a organizes, b comes from its calendar, c from the meeting chat, e (not invited) with
/// Join with an ID, d is invited and never comes.
struct Meeting {
    a: Phone,
    b: Phone,
    c: Phone,
    d: Phone,
    e: Phone,
    chat: String,
    /// a's, b's, c's and e's calls.
    calls: [CallId; 4],
}

impl Meeting {
    async fn joined(group: &Group) -> Self {
        let mut people = phones(group, 5).await;
        let (mut eve, dov) = (people.pop().unwrap(), people.pop().unwrap());
        let (mut cora, mut bert) = (people.pop().unwrap(), people.pop().unwrap());
        let mut ada = people.pop().unwrap();
        let event = schedule(group, &ada, &[&bert, &cora, &dov], SUBJECT).await;
        let chat = event.meeting_chat.clone().unwrap();
        assert_eq!(event.meeting.as_ref().unwrap().organizer_id, ada.oid());
        // the first one in starts the meeting's call: b, from its calendar's card, before the organizer comes
        let from_card = join_from_calendar(&bert, SUBJECT).await;
        let b_call = join(&mut bert, from_card, &CallStatus::Connected).await;
        // c from the meeting chat's header; a from its card
        let from_chat = join_from_chat(&cora, &chat, SUBJECT).await;
        let c_call = join(&mut cora, from_chat, &CallStatus::Connected).await;
        let from_card = join_from_calendar(&ada, SUBJECT).await;
        let a_call = join(&mut ada, from_card, &CallStatus::Connected).await;
        // e, not invited, types the ID with a wrong passcode first: no meeting is found, and no call is made
        let code = event
            .meeting_code
            .clone()
            .expect("a Teams meeting has an ID and a passcode");
        eprintln!(
            "the meeting's ID {} and passcode length {}",
            code.id,
            code.passcode.len()
        );
        let mut wrong = code.clone();
        wrong.passcode = format!("{}x", code.passcode);
        let mark = eve.mark();
        let tried = eve.act(CallAction::JoinMeetingCode(wrong)).await;
        let end = eve.ended(mark, tried).await;
        eprintln!("e's wrong passcode: {end:?}");
        assert!(matches!(end, End::Failed(_)), "{end:?}");
        // then the right one: the company's people bypass the lobby by default
        let e_call = join(&mut eve, CallAction::JoinMeetingCode(code), &CallStatus::Connected).await;
        Self {
            a: ada,
            b: bert,
            c: cora,
            d: dov,
            e: eve,
            chat,
            calls: [a_call, b_call, c_call, e_call],
        }
    }

    fn mris(&self) -> Vec<String> {
        [&self.a, &self.b, &self.c, &self.e]
            .iter()
            .map(|phone| phone.mri.clone())
            .collect()
    }

    /// Each sees the other three; the roster names a as the organizer; nobody rang; each hears the other three and
    /// not itself.
    async fn everyone_in(&mut self) {
        let mris = self.mris();
        let [ca, cb, cc, ce] = self.calls;
        for (phone, call) in [
            (&mut self.a, ca),
            (&mut self.b, cb),
            (&mut self.c, cc),
            (&mut self.e, ce),
        ] {
            let own = phone.mri.clone();
            let others: Vec<&str> = mris.iter().filter(|mri| **mri != own).map(String::as_str).collect();
            sees_people(phone, call, &others).await;
        }
        let organizer = self.a.mri.clone();
        for (phone, call) in [(&mut self.b, cb), (&mut self.c, cc), (&mut self.e, ce)] {
            shows(phone, call, "a as the organizer", |view| {
                party(view, &organizer).is_some_and(|p| p.organizer)
                    && view.participants.iter().filter(|p| p.organizer).count() == 1
            })
            .await;
        }
        let own = self.a.log.iter().rev().find_map(|event| match event {
            Event::CallOwn { call, organizer, .. } if *call == ca => Some(*organizer),
            _ => None,
        });
        eprintln!("a told it organizes: {own:?}");
        never_rang(&[&self.a, &self.b, &self.c, &self.d, &self.e]);
        tokio::time::sleep(SETTLE).await;
        let [ta, tb, tc, _, te] = TONES;
        judge(
            &[
                (&self.a, &[tb, tc, te], &[ta]),
                (&self.b, &[ta, tc, te], &[tb]),
                (&self.c, &[ta, tb, te], &[tc]),
                (&self.e, &[ta, tb, tc], &[te]),
            ],
            "four in the meeting",
        )
        .await;
    }

    /// a's and b's cameras on: c and e each show two video lines; then off.
    async fn cameras(&mut self) {
        let [ca, cb, ..] = self.calls;
        self.a.act(CallAction::Camera { call: ca, on: true }).await;
        self.b.act(CallAction::Camera { call: cb, on: true }).await;
        for phone in [&mut self.c, &mut self.e] {
            until(phone, "two cameras on two lines", |phone| lines_with_video(phone) >= 2).await;
            eprintln!("{}: the lines {:?}", phone.upn, phone.video.lines());
        }
        self.a.act(CallAction::Camera { call: ca, on: false }).await;
        self.b.act(CallAction::Camera { call: cb, on: false }).await;
    }

    /// b shares: c and e see it; c shares too and takes it over: b and e see c's share and b no longer sharing. c
    /// stops.
    async fn shares(&mut self) {
        let [_, cb, cc, ce] = self.calls;
        let (mb, mc) = (self.b.mri.clone(), self.c.mri.clone());
        self.b.act(CallAction::Share { call: cb, on: true }).await;
        sees_share(&mut self.c, cc, &mb).await;
        sees_share(&mut self.e, ce, &mb).await;
        self.c.act(CallAction::Share { call: cc, on: true }).await;
        sees_share(&mut self.e, ce, &mc).await;
        shows(&mut self.e, ce, "b no longer sharing", |view| {
            party(view, &mb).is_some_and(|p| !p.sharing)
        })
        .await;
        sees_share(&mut self.b, cb, &mc).await;
        eprintln!("e's sharing line: {:?}", self.e.video.line(SHARING_LINE));
        self.c.act(CallAction::Share { call: cc, on: false }).await;
        shows(&mut self.e, ce, "c's share over", |view| {
            party(view, &mc).is_some_and(|p| !p.sharing)
        })
        .await;
    }

    /// e raises its hand: the others see it, and e is told; e lowers it. c reacts: everyone is told.
    async fn hands_and_reactions(&mut self) {
        let [ca, cb, cc, ce] = self.calls;
        let me = self.e.mri.clone();
        let mark = self.e.mark();
        self.e.act(CallAction::RaiseHand { call: ce, raised: true }).await;
        for (phone, call) in [(&mut self.a, ca), (&mut self.b, cb), (&mut self.c, cc)] {
            shows(phone, call, "e's hand up", |view| {
                party(view, &me).is_some_and(|p| p.hand_raised)
            })
            .await;
        }
        told(&mut self.e, mark, "its own hand up", |event| match event {
            Event::CallOwn {
                call,
                hand_raised: true,
                ..
            } if *call == ce => Some(()),
            _ => None,
        })
        .await;
        self.e
            .act(CallAction::RaiseHand {
                call: ce,
                raised: false,
            })
            .await;
        for (phone, call) in [(&mut self.a, ca), (&mut self.b, cb), (&mut self.c, cc)] {
            shows(phone, call, "e's hand down", |view| {
                party(view, &me).is_some_and(|p| !p.hand_raised)
            })
            .await;
        }
        let mc = self.c.mri.clone();
        let marks = [self.a.mark(), self.b.mark(), self.c.mark(), self.e.mark()];
        self.c
            .act(CallAction::React {
                call: cc,
                reaction: "like".to_owned(),
            })
            .await;
        for ((phone, call), mark) in [
            (&mut self.a, ca),
            (&mut self.b, cb),
            (&mut self.c, cc),
            (&mut self.e, ce),
        ]
        .into_iter()
        .zip(marks)
        {
            reaction_told(phone, mark, call, &mc, "like").await;
        }
    }

    /// a's Mute all: b, c and e are told "You've been muted", show as muted at a, and a hears none of them. b
    /// unmutes itself: a sees and hears b again.
    async fn mute_all(&mut self) {
        let [ca, cb, cc, ce] = self.calls;
        let marks = [self.b.mark(), self.c.mark(), self.e.mark()];
        self.a.act(CallAction::MuteAll { call: ca }).await;
        for ((phone, call), mark) in [(&mut self.b, cb), (&mut self.c, cc), (&mut self.e, ce)]
            .into_iter()
            .zip(marks)
        {
            meeting_is(phone, mark, call, "muted by the organizer", |state| {
                state.muted_by_organizer.is_some()
            })
            .await;
        }
        let others = [self.b.mri.clone(), self.c.mri.clone(), self.e.mri.clone()];
        shows(&mut self.a, ca, "the others muted", |view| {
            others.iter().all(|mri| party(view, mri).is_some_and(|p| p.muted))
        })
        .await;
        let organizer = self.a.mri.clone();
        shows(&mut self.b, cb, "a not muted", |view| {
            party(view, &organizer).is_some_and(|p| !p.muted)
        })
        .await;
        tokio::time::sleep(SETTLE).await;
        let [ta, tb, tc, _, te] = TONES;
        judge(
            &[(&self.a, &[], &[tb, tc, te]), (&self.b, &[ta], &[tc, te])],
            "after Mute all",
        )
        .await;
        self.b.act(CallAction::Mute { call: cb, muted: false }).await;
        let mb = self.b.mri.clone();
        shows(&mut self.a, ca, "b unmuted", |view| {
            party(view, &mb).is_some_and(|p| !p.muted)
        })
        .await;
        tokio::time::sleep(SETTLE).await;
        judge(&[(&self.a, &[tb], &[tc, te])], "b unmuted itself").await;
    }

    /// a's End meeting: the call ends for everyone; the meeting chat shows its start and its end with the four, at
    /// each member (d too, who never came); nobody has a call log entry or a missed call.
    async fn end_meeting(mut self) {
        let [ca, cb, cc, ce] = self.calls;
        let marks = [self.b.mark(), self.c.mark(), self.e.mark()];
        let ended_at = self.d.mark();
        self.a.act(CallAction::EndMeeting { call: ca }).await;
        for ((phone, call), mark) in [(&mut self.b, cb), (&mut self.c, cc), (&mut self.e, ce)]
            .into_iter()
            .zip(marks)
        {
            let end = phone.ended(mark, call).await;
            eprintln!("{}: the meeting ended: {end:?}", phone.upn);
            assert!(matches!(end, End::Remote(_)), "{}: {end:?}", phone.upn);
        }
        let end = self.a.ended(0, ca).await;
        eprintln!("a: the meeting ended: {end:?}");
        let (everyone, chat) = (self.mris(), self.chat.clone());
        self.d
            .pushed_line(ended_at, &chat, "the meeting's end", ended_with(&everyone))
            .await;
        for phone in [&mut self.a, &mut self.b, &mut self.c, &mut self.d] {
            phone.sync().await;
            let lines = phone.stored_lines(&chat).await;
            eprintln!("{}: the meeting chat's call lines: {lines:?}", phone.upn);
            phone.shows_lines(&chat, &[&started, &ended_with(&everyone)]).await;
        }
        no_call_traces(&[&self.a, &self.b, &self.c, &self.d, &self.e]).await;
        never_rang(&[&self.a, &self.b, &self.c, &self.d, &self.e]);
        for phone in [&mut self.a, &mut self.b, &mut self.c, &mut self.d, &mut self.e] {
            phone.close().await;
        }
    }
}

async fn scheduled(layout: Layout) {
    let _one = ONE_AT_A_TIME.lock().await;
    let Some(group) = Group::start(layout, "meeting", &PEOPLE, false).await else {
        return;
    };
    let mut meeting = Meeting::joined(&group).await;
    meeting.everyone_in().await;
    meeting.cameras().await;
    meeting.shares().await;
    meeting.hands_and_reactions().await;
    meeting.mute_all().await;
    meeting.end_meeting().await;
    group.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_scheduled_meeting_joined_from_its_event_its_chat_and_its_id_runs_and_ends_for_all_on_one_node() {
    scheduled(Layout::All).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_scheduled_meeting_joined_from_its_event_its_chat_and_its_id_runs_and_ends_for_all_on_api_and_media_nodes() {
    scheduled(Layout::Split).await;
}

// --- the lobby ---

/// `host` lets `waiting` in (`admit`) or turns it away, from its `call`.
async fn let_in(host: &Phone, call: CallId, waiting: &Phone, admit: bool) {
    host.act(CallAction::Lobby {
        call,
        waiting: Mri::new(waiting.mri.clone()),
        admit,
    })
    .await;
}

/// Nothing puts `phone` in a call for [`QUIET`]: it stays where it is (the lobby).
async fn stays_out(phone: &mut Phone, what: &str) {
    let mark = phone.mark();
    let connected = |event: &Event| {
        matches!(
            event,
            Event::Call {
                status: CallStatus::Connected,
                ..
            }
        )
    };
    assert!(phone.quiet(mark, QUIET, connected).await, "{}: {what}", phone.upn);
}

/// The people of [`lobby`]: a organizes, b is invited, d and e are outside the meeting.
struct Lobby {
    a: Phone,
    b: Phone,
    d: Phone,
    e: Phone,
}

impl Lobby {
    async fn start(group: &Group) -> Self {
        let mut phones = phones(group, 5).await;
        let e = phones.pop().unwrap();
        let d = phones.pop().unwrap();
        drop(phones.pop());
        let b = phones.pop().unwrap();
        let a = phones.pop().unwrap();
        Self { a, b, d, e }
    }

    /// "Only invited people" bypass: b comes straight in; d (not invited, by ID) waits, a and b (a presenter) see it
    /// wait, b admits it, and d hears and is heard. Then e waits and a turns it away: its call ends, it never comes in.
    async fn invited_only(&mut self, group: &Group) {
        let subject = "Lobby 🚪 «check»";
        let event = schedule(group, &self.a, &[&self.b], subject).await;
        lobby_bypass(group, &self.a, &event, "InvitedUsers").await;
        let code = event.meeting_code.clone().unwrap();
        let a_call = join_card(&mut self.a, subject, &CallStatus::Connected).await;
        let b_call = join_card(&mut self.b, subject, &CallStatus::Connected).await;
        let d_call = self.admitted_by_a_presenter(code.clone(), a_call, b_call).await;
        self.turned_away(code, a_call).await;
        hang_up(&mut self.d, d_call).await;
        hang_up(&mut self.b, b_call).await;
        hang_up(&mut self.a, a_call).await;
    }

    async fn admitted_by_a_presenter(
        &mut self,
        code: teams_proto::MeetingCode,
        a_call: CallId,
        b_call: CallId,
    ) -> CallId {
        let (a_mark, b_mark) = (self.a.mark(), self.b.mark());
        let d_call = join(&mut self.d, CallAction::JoinMeetingCode(code), &CallStatus::Lobby).await;
        let waiting = self.d.mri.clone();
        lobby_is(&mut self.a, a_mark, a_call, &[&waiting]).await;
        lobby_is(&mut self.b, b_mark, b_call, &[&waiting]).await;
        stays_out(&mut self.d, "came in without an admit").await;
        let (a_mark, d_mark) = (self.a.mark(), self.d.mark());
        let_in(&self.b, b_call, &self.d, true).await;
        self.d.status(d_mark, d_call, &CallStatus::Connected).await;
        lobby_is(&mut self.a, a_mark, a_call, &[]).await;
        let (ma, mb) = (self.a.mri.clone(), self.b.mri.clone());
        sees_people(&mut self.a, a_call, &[&mb, &waiting]).await;
        sees_people(&mut self.d, d_call, &[&ma, &mb]).await;
        tokio::time::sleep(SETTLE).await;
        let [ta, tb, _, td, _] = TONES;
        judge(
            &[(&self.d, &[ta, tb], &[td]), (&self.a, &[tb, td], &[ta])],
            "d let in from the lobby",
        )
        .await;
        d_call
    }

    async fn turned_away(&mut self, code: teams_proto::MeetingCode, a_call: CallId) {
        let a_mark = self.a.mark();
        let e_call = join(&mut self.e, CallAction::JoinMeetingCode(code), &CallStatus::Lobby).await;
        let waiting = self.e.mri.clone();
        lobby_is(&mut self.a, a_mark, a_call, &[&waiting]).await;
        let (a_mark, e_mark) = (self.a.mark(), self.e.mark());
        let_in(&self.a, a_call, &self.e, false).await;
        let end = self.e.ended(e_mark, e_call).await;
        eprintln!("e turned away: {end:?}");
        assert!(matches!(end, End::Remote(_)), "{end:?}");
        lobby_is(&mut self.a, a_mark, a_call, &[]).await;
        let came_in = self.e.log.iter().any(|event| {
            matches!(
                event,
                Event::Call {
                    status: CallStatus::Connected,
                    ..
                }
            )
        });
        assert!(!came_in, "e came in");
        let mark = self.a.mark();
        let shown =
            |event: &Event| matches!(event, Event::CallParticipants { view, .. } if party(view, &waiting).is_some());
        assert!(self.a.quiet(mark, QUIET, shown).await, "e shows in the call");
    }

    /// "Only the organizer" bypass: b, invited, comes before the organizer and waits until a comes and lets it in.
    async fn organizer_only(&mut self, group: &Group) {
        let subject = "Early bird 🐦 sync";
        let event = schedule(group, &self.a, &[&self.b], subject).await;
        lobby_bypass(group, &self.a, &event, "OrganizerOnly").await;
        let b_call = join_card(&mut self.b, subject, &CallStatus::Lobby).await;
        stays_out(&mut self.b, "came in with nobody to admit it").await;
        let a_mark = self.a.mark();
        let a_call = join_card(&mut self.a, subject, &CallStatus::Connected).await;
        let waiting = self.b.mri.clone();
        lobby_is(&mut self.a, a_mark, a_call, &[&waiting]).await;
        let b_mark = self.b.mark();
        let_in(&self.a, a_call, &self.b, true).await;
        self.b.status(b_mark, b_call, &CallStatus::Connected).await;
        tokio::time::sleep(SETTLE).await;
        let [ta, tb, ..] = TONES;
        judge(
            &[(&self.a, &[tb], &[ta]), (&self.b, &[ta], &[tb])],
            "b let in by the organizer",
        )
        .await;
        hang_up(&mut self.b, b_call).await;
        hang_up(&mut self.a, a_call).await;
        // the meeting started while b waited alone: its chat tells the start and the end all the same
        let chat = event.meeting_chat.clone().unwrap();
        let both = [self.a.mri.clone(), self.b.mri.clone()];
        self.a.sync().await;
        eprintln!("the early meeting's call lines: {:?}", self.a.stored_lines(&chat).await);
        self.a.shows_lines(&chat, &[&started, &ended_with(&both)]).await;
    }

    async fn close(mut self) {
        let everyone = [&self.a, &self.b, &self.d, &self.e];
        never_rang(&everyone);
        no_call_traces(&everyone).await;
        for phone in [&mut self.a, &mut self.b, &mut self.d, &mut self.e] {
            phone.close().await;
        }
    }
}

async fn lobby(layout: Layout) {
    let _one = ONE_AT_A_TIME.lock().await;
    let Some(group) = Group::start(layout, "lobby", &PEOPLE, false).await else {
        return;
    };
    let mut lobby = Lobby::start(&group).await;
    lobby.invited_only(&group).await;
    lobby.organizer_only(&group).await;
    lobby.close().await;
    group.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_lobby_holds_outsiders_until_a_host_lets_them_in_and_a_guest_waits_for_the_organizer_on_one_node() {
    lobby(Layout::All).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_lobby_holds_outsiders_until_a_host_lets_them_in_and_a_guest_waits_for_the_organizer_on_api_and_media_nodes()
 {
    lobby(Layout::Split).await;
}

// --- the organizer's Controls ---

/// a, b and c in one call (a scheduled meeting of a's, or a group chat's call): each one's call.
struct Three {
    a: Phone,
    b: Phone,
    c: Phone,
    calls: [CallId; 3],
}

impl Three {
    async fn phones(group: &Group) -> (Phone, Phone, Phone) {
        let mut phones = phones(group, 3).await;
        let c = phones.pop().unwrap();
        let b = phones.pop().unwrap();
        (phones.pop().unwrap(), b, c)
    }

    /// a schedules a meeting with b and c invited; each joins from its calendar card.
    async fn in_a_meeting(group: &Group) -> Self {
        let (mut a, mut b, mut c) = Self::phones(group).await;
        let subject = "Town hall 🏛️ «all hands»";
        schedule(group, &a, &[&b, &c], subject).await;
        let a_call = join_card(&mut a, subject, &CallStatus::Connected).await;
        let b_call = join_card(&mut b, subject, &CallStatus::Connected).await;
        let c_call = join_card(&mut c, subject, &CallStatus::Connected).await;
        let others = [b.mri.clone(), c.mri.clone()];
        sees_people(&mut a, a_call, &[&others[0], &others[1]]).await;
        Self {
            a,
            b,
            c,
            calls: [a_call, b_call, c_call],
        }
    }

    /// a calls a group chat of the three (and synthetic people); b and c answer their rings.
    async fn in_a_group_call(group: &Group) -> Self {
        let (mut a, mut b, mut c) = Self::phones(group).await;
        let chat = group_chat(&a, &[&b, &c], &group.synthetic[..6], "Stand-up 🧍 «daily»").await;
        let (b_rang, c_rang) = (b.mark(), c.mark());
        let a_call = join(&mut a, CallAction::Call { chat }, &CallStatus::Connected).await;
        let mut answered = Vec::new();
        for (phone, mark) in [(&mut b, b_rang), (&mut c, c_rang)] {
            let rings = phone.incoming(mark).await;
            answered.push(join(phone, CallAction::Answer { call: rings.call }, &CallStatus::Connected).await);
        }
        Self {
            a,
            b,
            c,
            calls: [a_call, answered[0], answered[1]],
        }
    }

    /// a sets one of the meeting's options from its Controls; b and c are told `what` holds.
    async fn option(
        &mut self,
        option: MeetingOption,
        what: &str,
        wanted: impl Fn(&teams_call::MeetingState) -> bool + Copy,
    ) {
        let [a_call, b_call, c_call] = self.calls;
        let marks = [self.b.mark(), self.c.mark()];
        self.a.act(CallAction::MeetingOption { call: a_call, option }).await;
        for ((phone, call), mark) in [(&mut self.b, b_call), (&mut self.c, c_call)].into_iter().zip(marks) {
            meeting_is(phone, mark, call, what, wanted).await;
        }
    }

    /// Only the organizer presents: b and c are attendees at once, and b's Mute all mutes nobody.
    async fn attendees(&mut self) {
        self.option(MeetingOption::Present(false), "an attendee", |state| {
            state.attendee && state.options.present == Some(false)
        })
        .await;
        let a_mark = self.a.mark();
        let pressed = self.b.press(CallAction::MuteAll { call: self.calls[1] }).await;
        eprintln!("b's Mute all as an attendee: {pressed:?}");
        let muted =
            |event: &Event| matches!(event, Event::CallMeeting { state, .. } if state.muted_by_organizer.is_some());
        assert!(
            self.a.quiet(a_mark, QUIET, muted).await,
            "an attendee muted the organizer"
        );
    }

    /// The attendees' microphones off: b and c are muted and blocked; the app's Mic button refuses the unmute while it
    /// is blocked (`teams_ui::calls`, "unmute-blocked"), so b presses nothing, and nobody hears b or c.
    async fn microphones_off(&mut self) {
        let unmute_off = MeetingOption::Attendees {
            unmute: false,
            camera: true,
        };
        self.option(unmute_off, "muted, the microphone blocked", |state| {
            state.unmute_blocked() && state.muted_by_organizer.is_some()
        })
        .await;
        let (mb, mc) = (self.b.mri.clone(), self.c.mri.clone());
        shows(&mut self.a, self.calls[0], "b and c muted", |view| {
            [&mb, &mc].iter().all(|mri| party(view, mri).is_some_and(|p| p.muted))
        })
        .await;
        tokio::time::sleep(SETTLE).await;
        let [ta, tb, tc, ..] = TONES;
        judge(
            &[(&self.a, &[], &[tb, tc]), (&self.c, &[ta], &[tb])],
            "attendees' microphones off",
        )
        .await;
    }

    /// Everyone presents again: nobody is bound; b unmutes and is heard.
    async fn all_present(&mut self) {
        self.option(MeetingOption::Present(true), "a presenter again", |state| {
            !state.attendee && !state.unmute_blocked()
        })
        .await;
        self.b
            .act(CallAction::Mute {
                call: self.calls[1],
                muted: false,
            })
            .await;
        let mb = self.b.mri.clone();
        shows(&mut self.a, self.calls[0], "b unmuted", |view| {
            party(view, &mb).is_some_and(|p| !p.muted)
        })
        .await;
        tokio::time::sleep(SETTLE).await;
        let [ta, tb, ..] = TONES;
        judge(&[(&self.a, &[tb], &[ta])], "b unmuted once all present").await;
    }

    /// b raises its hand: a and c see it; c reacts: a and b are told; b lowers its hand: a and c see it.
    async fn hands_and_a_reaction(&mut self) {
        let [a_call, b_call, c_call] = self.calls;
        let mb = self.b.mri.clone();
        for raised in [true, false] {
            self.b.act(CallAction::RaiseHand { call: b_call, raised }).await;
            for (phone, call) in [(&mut self.a, a_call), (&mut self.c, c_call)] {
                shows(phone, call, &format!("b's hand raised {raised}"), |view| {
                    party(view, &mb).is_some_and(|p| p.hand_raised == raised)
                })
                .await;
            }
            if !raised {
                break;
            }
            let mc = self.c.mri.clone();
            let marks = [self.a.mark(), self.b.mark()];
            let heart = CallAction::React {
                call: c_call,
                reaction: "heart".to_owned(),
            };
            self.c.act(heart).await;
            for ((phone, call), mark) in [(&mut self.a, a_call), (&mut self.b, b_call)].into_iter().zip(marks) {
                reaction_told(phone, mark, call, &mc, "heart").await;
            }
        }
    }

    /// Each hangs up (b, c, then a): the channel's post shows the end; nobody has a call log entry or a missed call.
    async fn meeting_over(mut self, channel: &str, started: &str) {
        let [a_call, b_call, c_call] = self.calls;
        hang_up(&mut self.b, b_call).await;
        hang_up(&mut self.c, c_call).await;
        hang_up(&mut self.a, a_call).await;
        ended_post(&self.b, channel, started).await;
        no_call_traces(&[&self.a, &self.b, &self.c]).await;
        for phone in [&mut self.a, &mut self.b, &mut self.c] {
            phone.close().await;
        }
    }

    async fn leave(mut self) {
        let [a_call, b_call, c_call] = self.calls;
        hang_up(&mut self.c, c_call).await;
        hang_up(&mut self.b, b_call).await;
        hang_up(&mut self.a, a_call).await;
        for phone in [&mut self.a, &mut self.b, &mut self.c] {
            phone.close().await;
        }
    }
}

async fn controls(layout: Layout) {
    let _one = ONE_AT_A_TIME.lock().await;
    let Some(group) = Group::start(layout, "controls", &PEOPLE, false).await else {
        return;
    };
    let mut three = Three::in_a_meeting(&group).await;
    three.attendees().await;
    three.microphones_off().await;
    three.all_present().await;
    never_rang(&[&three.a, &three.b, &three.c]);
    three.leave().await;
    group.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_organizers_controls_bind_everyone_in_the_running_meeting_at_once_on_one_node() {
    controls(Layout::All).await;
}

// --- hands and reactions in a group call ---

async fn group_hands(layout: Layout) {
    let _one = ONE_AT_A_TIME.lock().await;
    let Some(group) = Group::start(layout, "group-hands", &PEOPLE, false).await else {
        return;
    };
    let mut three = Three::in_a_group_call(&group).await;
    three.hands_and_a_reaction().await;
    three.leave().await;
    group.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn raised_hands_and_reactions_reach_everyone_in_a_group_call_on_api_and_media_nodes() {
    group_hands(Layout::Split).await;
}

// --- a channel's Meet now ---

/// A team of `owner`'s with `members` and synthetic people on it, and a channel of it, once each of them has it: the
/// channel's thread.
async fn channel_of(group: &Group, owner: &Phone, members: &[&Phone]) -> String {
    let create = TeamWrite::Create(NewTeam {
        name: "Rocket 🚀 «R&D»".into(),
        description: "Rocket's people & friends".into(),
        first_channel: "General".into(),
        public: false,
    });
    let (team, team_group) = owner.engine().team_write(create, None).await.unwrap().created.unwrap();
    let team_group = team_group.unwrap();
    let mut people: Vec<(Mri, TeamRole)> = members
        .iter()
        .map(|phone| (Mri::new(phone.mri.clone()), TeamRole::Member))
        .collect();
    people.extend(group.synthetic[..4].iter().map(|mri| (Mri::new(mri), TeamRole::Member)));
    let add = TeamWrite::AddMembers {
        team: team.clone(),
        group: team_group.clone(),
        members: people,
    };
    let written = owner.engine().team_write(add, None).await.unwrap();
    assert!(written.not_added.is_empty(), "{written:?}");
    let made = owner
        .engine()
        .create_channel(teams_core::ChannelDraft {
            channel: crate::teams::new_channel(
                &team,
                &team_group,
                "Launch ✨ v2",
                teams_proto::NewChannelKind::Standard,
            ),
            show_by_default: true,
        })
        .await
        .unwrap();
    let channel = made.thread.unwrap();
    owner.sync().await;
    for member in members {
        member.sync().await;
    }
    channel
}

/// The channel's posts `reader` reads, newest first: its kind, its text, its meeting's title.
async fn posts(reader: &Phone, channel: &str) -> Vec<(String, String, Option<String>)> {
    let client = reader.client();
    let page = ChatReads::new(&client).messages(channel, 30).await.unwrap();
    page.messages
        .iter()
        .map(|message| {
            let meeting = match message.properties.get("meeting") {
                Some(serde_json::Value::String(text)) => serde_json::from_str(text).ok(),
                other => other.cloned(),
            };
            let title = meeting
                .as_ref()
                .and_then(|meeting: &serde_json::Value| meeting.get("meetingtitle")?.as_str())
                .map(str::to_owned);
            (message.message_type.as_str().to_owned(), message.content.clone(), title)
        })
        .collect()
}

/// The `<callId>` an `Event/Call` post names.
fn call_id(text: &str) -> Option<String> {
    let rest = text.split("<callId>").nth(1)?;
    Some(rest.split('<').next()?.to_owned())
}

/// a's Meet now in `channel`, titled from its pre-join: the channel's post of its start (its kind, text and title),
/// as b reads it, and a's call.
async fn meet_now_posted(
    a: &mut Phone,
    b: &Phone,
    channel: &str,
    title: &str,
) -> ((String, String, Option<String>), CallId) {
    let meet_now = CallAction::MeetNow {
        channel: channel.to_owned(),
    };
    let a_call = join(a, meet_now, &CallStatus::Connected).await;
    a.engine()
        .requests()
        .title_meet_now(channel.to_owned(), title)
        .await
        .unwrap();
    let listed = posts(b, channel).await;
    eprintln!("the channel's posts: {listed:?}");
    let started = listed
        .iter()
        .find(|(kind, text, _)| kind == "Event/Call" && text.contains("callStarted"))
        .cloned()
        .unwrap_or_else(|| panic!("no start post: {listed:?}"));
    assert_eq!(started.2.as_deref(), Some(title), "{listed:?}");
    (started, a_call)
}

/// `phone` joins the channel's running meeting from its post; the roster names `organizer` as the organizer.
async fn join_from_post(phone: &mut Phone, channel: &str, organizer: &str) -> CallId {
    let action = CallAction::JoinChannel {
        channel: channel.to_owned(),
    };
    let call = join(phone, action, &CallStatus::Connected).await;
    shows(phone, call, "the organizer", |view| {
        party(view, organizer).is_some_and(|p| p.organizer)
    })
    .await;
    call
}

/// The channel's posts once the newest tells the meeting's end: one end, of the call its start named.
async fn ended_post(reader: &Phone, channel: &str, started: &str) {
    let deadline = tokio::time::Instant::now() + WAIT;
    let listed = loop {
        let listed = posts(reader, channel).await;
        if listed.first().is_some_and(|(_, text, _)| text.contains("callEnded")) {
            break listed;
        }
        assert!(tokio::time::Instant::now() < deadline, "no end post: {listed:?}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    eprintln!("the channel's posts at the end: {listed:?}");
    assert_eq!(call_id(&listed[0].1), call_id(started), "{listed:?}");
    let ends = listed
        .iter()
        .filter(|(kind, text, _)| kind == "Event/Call" && text.contains("callEnded"))
        .count();
    assert_eq!(ends, 1, "{listed:?}");
}

async fn channel_meet_now(layout: Layout) {
    let _one = ONE_AT_A_TIME.lock().await;
    let Some(group) = Group::start(layout, "meet-now", &PEOPLE, false).await else {
        return;
    };
    let (mut a, mut b, mut c) = Three::phones(&group).await;
    let channel = channel_of(&group, &a, &[&b, &c]).await;
    let (started, a_call) = meet_now_posted(&mut a, &b, &channel, "Sprint review ✨ «Q4» & demo").await;
    let organizer = a.mri.clone();
    let b_call = join_from_post(&mut b, &channel, &organizer).await;
    let c_call = join_from_post(&mut c, &channel, &organizer).await;
    let others = [b.mri.clone(), c.mri.clone()];
    sees_people(&mut a, a_call, &[&others[0], &others[1]]).await;
    tokio::time::sleep(SETTLE).await;
    let [ta, tb, tc, ..] = TONES;
    judge(
        &[(&a, &[tb, tc], &[ta]), (&b, &[ta, tc], &[tb]), (&c, &[ta, tb], &[tc])],
        "three in the channel's meeting",
    )
    .await;
    never_rang(&[&a, &b, &c]);
    let three = Three {
        a,
        b,
        c,
        calls: [a_call, b_call, c_call],
    };
    three.meeting_over(&channel, &started.1).await;
    group.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_channels_meet_now_is_posted_titled_joined_from_its_post_and_shown_ended_on_one_node() {
    channel_meet_now(Layout::All).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_channels_meet_now_is_posted_titled_joined_from_its_post_and_shown_ended_on_api_and_media_nodes() {
    channel_meet_now(Layout::Split).await;
}
