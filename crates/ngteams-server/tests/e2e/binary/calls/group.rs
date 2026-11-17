//! Group calls as users make them on an own server: the real `ngteams-server` binary (`serve --role all` on SQLite,
//! or `--role api` and `--role media` as two processes on one Postgres and Redis), the run's coturn, and each
//! account's client as the app runs it ([`Phone`]). The accounts sit among synthetic ones with odd names, in a group
//! chat with members who have no client.
//!
//! - three in a call hear the other two and never themselves; mute takes one out of the mix and the speaker shows as
//!   speaking in the others' view; a late joiner hears and is heard; one who leaves is no longer heard and the call
//!   goes on; the last ones out end it, and the chat shows its end with everyone who was in it;
//! - a killed media node ends its calls for everyone, and a new call starts once a media node runs again;
//! - a group start with no media node fails;
//! - six people talk for a while: what each receiver played has no gap, and the media node's CPU is noted.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use teams_core::engine::{Action, Event};
use teams_core::{CallAction, CallId, CallStatus, CallView, End};
use teams_media::devices::wav;
use teams_media::tone::{Tone, level_at};
use teams_proto::Mri;
use teams_store::OutboxState;

use super::super::support::{Deployment, Node, Port};
use super::support::{Line, Phone, brief, coturn};
use crate::support::{Backend, PASSWORD, PG_ENV, redis_url};

/// A tone of the mix is heard at least this loud (the mix of two 0.5 tones is soft-clipped).
const HEARD: f32 = 0.15;
/// Below this, a tone isn't in a recording.
const ABSENT: f32 = 0.05;
/// How long the media runs before what was heard is judged.
const TALK: Duration = Duration::from_secs(4);
const RING: u64 = 12;
/// Set, the nodes keep the media node's addresses at their defaults (the host's outward address): the run's coturn
/// must then relay from that address, as a coturn beside a server on one host does (`commands.md` of QA 297).
const MEDIA_DEFAULTS: &str = "NGTS_TEST_MEDIA_DEFAULTS";
/// Set, the relay URLs the nodes give out, comma-separated, in place of the run's coturn over UDP: a client in a VM
/// needs one at its own address (QA 422: libwebrtc on Windows ignores loopback), forwarded to the coturn.
const TURN_URLS: &str = "NGTS_TEST_TURN_URLS";

const fn tone(hz: f32, amplitude: f32) -> Tone {
    Tone { hz, amplitude }
}

/// Four people's tones: none a low harmonic of another's (a clipped mix has odd harmonics).
const TONES: [Tone; 4] = [tone(440.0, 0.5), tone(660.0, 0.5), tone(880.0, 0.5), tone(1170.0, 0.5)];

/// Synthetic people with odd names: members of the chats with no client of their own.
const ODD_NAMES: [&str; 12] = [
    "Zoë O'Brien-Ångström",
    "李 小龍",
    "ابن سينا",
    "Dr. 🦀 Ferris",
    "Robert'); DROP TABLE users;--",
    "<b>Bold</b> & \"Quoted\"",
    "Q",
    "Ana   Maria   de la Cruz",
    "Ωmega ‹test› ∑",
    "Jean-Ēmīls Ŝkoda",
    "Name-That-Is-Very-Long-Indeed-For-A-Person-Who-Has-Many-Middle-Names-Alpha-Beta-Gamma",
    "Ỳ̷̛̝ zalgo",
];

/// How the deployment runs its group calls' media.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// One `serve` (`--role all`, the default) on SQLite, its media configuration left at the defaults.
    All,
    /// `serve --role api` and `serve --role media`, two processes on one Postgres and Redis.
    Split,
}

/// The deployment, its people, and where its nodes' logs go.
pub struct Group {
    pub origin: String,
    pub api: Node,
    pub media: Option<Node>,
    pub people: Vec<(String, String)>,
    pub synthetic: Vec<String>,
    logs: Option<PathBuf>,
    /// The API node's log of its last start, when the run keeps the nodes' logs.
    pub api_log: Option<PathBuf>,
    starts: u32,
    _internal: Option<Port>,
    _deployment: Deployment,
}

fn append(config: &Path, tables: &str) {
    let mut file = std::fs::OpenOptions::new().append(true).open(config).unwrap();
    writeln!(file, "{tables}").unwrap();
}

/// The added account's object id, from `user add`'s `added <upn> (<id>)`.
fn added_id(line: &str) -> String {
    let start = line.find('(').unwrap() + 1;
    let end = line[start..].find(')').unwrap() + start;
    line[start..end].to_owned()
}

/// The `[calling]` tables of the nodes' configuration: the ring timeout and the run's coturn.
fn calling(relay: std::net::SocketAddr, secret: &str) -> String {
    let urls = std::env::var(TURN_URLS).unwrap_or_else(|_| format!("turn:{relay}?transport=udp"));
    let urls: Vec<&str> = urls.split(',').collect();
    format!("[calling]\nring_timeout = {RING}\n[calling.turn]\nsecret = {secret:?}\nurls = {urls:?}\n")
}

impl Group {
    /// `None` (skipped, and said) without a coturn, or for [`Layout::Split`] without Postgres and Redis.
    pub async fn start(layout: Layout, flow: &str, people: &[(&str, &str)], api_only: bool) -> Option<Self> {
        let Some((relay, secret)) = coturn() else {
            eprintln!("skipped: no coturn named");
            return None;
        };
        let backend = match layout {
            Layout::All => Backend::Sqlite,
            Layout::Split => Backend::Postgres,
        };
        let redis = (layout == Layout::Split).then(redis_url).flatten();
        if layout == Layout::Split && (std::env::var_os(PG_ENV).is_none() || redis.is_none()) {
            eprintln!("skipped: the split layout needs Postgres and Redis");
            return None;
        }
        eprintln!("group calls, {flow}: {layout:?}");
        let mut deployment = Deployment::new(backend, false, redis).await;
        deployment.origin = deployment.origin.replacen("http:", "https:", 1);
        let mut config = calling(relay, &secret);
        config.push_str("[mixer]\n");
        if std::env::var_os(MEDIA_DEFAULTS).is_none() {
            // the loopback coturn relays from 127.0.0.1: the node's candidate and its replies are there too
            config.push_str("udp_listen = \"127.0.0.1:0\"\n");
        }
        if layout == Layout::Split {
            config.push_str("heartbeat = 1\ndead_after = 3\ndrain_timeout = 3\n");
        }
        let mut api = deployment.node("a");
        append(&api.config, &config);
        let (media, internal) = if layout == Layout::Split {
            let internal = Port::take();
            let mut media = deployment.node("media");
            let address = std::net::SocketAddr::from(([127, 0, 0, 1], internal.number));
            append(&media.config, &format!("{config}internal_listen = \"{address}\"\n"));
            media.role = Some(("media", Some(address)));
            api.role = Some(("api", None));
            (Some(media), Some(internal))
        } else {
            if api_only {
                api.role = Some(("api", None));
            }
            (None, None)
        };
        api.cli(&["migrate"], "");
        let mut added = Vec::new();
        for (upn, name) in people {
            added.push(((*upn).to_owned(), (*name).to_owned()));
            api.cli(&["user", "add", upn, "--name", name], &format!("{PASSWORD}\n"));
        }
        let mut synthetic = Vec::new();
        for (index, name) in ODD_NAMES.iter().enumerate() {
            let line = api.cli(
                &["user", "add", &format!("odd.{index}@own.test"), "--name", name],
                &format!("{PASSWORD}\n"),
            );
            synthetic.push(format!("8:orgid:{}", added_id(&line)));
        }
        let logs = std::env::var_os(super::support::LOG_DIR)
            .map(|dir| Path::new(&dir).join(format!("group-{flow}-{layout:?}")));
        let mut group = Self {
            origin: deployment.origin.clone(),
            api,
            media,
            people: added,
            synthetic,
            logs,
            api_log: None,
            starts: 0,
            _internal: internal,
            _deployment: deployment,
        };
        group.start_api().await;
        if api_only {
            return Some(group);
        }
        if group.media.is_some() {
            group.start_media().await;
        }
        Some(group)
    }

    fn log(&mut self, name: &str) -> Option<PathBuf> {
        self.starts += 1;
        let dir = self.logs.as_ref()?;
        std::fs::create_dir_all(dir).unwrap();
        Some(dir.join(format!("{name}-start-{}.log", self.starts)))
    }

    async fn start_api(&mut self) {
        self.api_log = self.log("api");
        match &self.api_log {
            Some(log) => self.api.start_logged(log).await,
            None => self.api.start().await,
        }
    }

    pub async fn start_media(&mut self) {
        let log = self.log("media");
        let media = self.media.as_mut().unwrap();
        match log {
            Some(log) => media.start_logged(&log).await,
            None => media.start().await,
        }
    }

    /// The process that runs the media: the media node's, else the one node's.
    pub fn media_pid(&self) -> u32 {
        self.media.as_ref().unwrap_or(&self.api).pid().unwrap()
    }

    /// A running client of the `index`th person, its microphone `tone`.
    pub async fn phone(&self, index: usize, tone: Tone) -> Phone {
        let (upn, name) = &self.people[index];
        let mut phone = Phone::signed_in(&self.api, &self.origin, (upn, name), tone).await;
        phone.start();
        phone.online(0).await;
        phone
    }

    pub async fn stop(mut self) {
        if let Some(media) = &mut self.media {
            media.stop().await;
        }
        self.api.stop().await;
    }
}

/// The group chat `first` makes with `others` and `quiet` (members with no client), topic `topic`, once every other
/// client has it.
pub async fn group_chat(first: &Phone, others: &[&Phone], quiet: &[String], topic: &str) -> String {
    let mut members: Vec<Mri> = others.iter().map(|phone| Mri::new(phone.mri.clone())).collect();
    members.extend(quiet.iter().map(Mri::new));
    let create = Action::CreateChat {
        others: members,
        topic: Some(topic.to_owned()),
    };
    let row = first.engine().write(create).await.unwrap();
    assert_eq!(row.state, OutboxState::Sent, "{row:?}");
    let chat = row.message_id.unwrap();
    for phone in std::iter::once(first).chain(others.iter().copied()) {
        phone.sync().await;
    }
    chat
}

/// More chats among the synthetic people, as a team has them: odd topics, nobody calls in them.
async fn seed_chats(first: &Phone, synthetic: &[String]) {
    for (index, topic) in ["Ünïcödé & <tags>", "🚀 launch / ops", "'quoted' \"topic\"", "x"]
        .iter()
        .enumerate()
    {
        let others = synthetic.iter().skip(index).take(3 + index).map(Mri::new).collect();
        let row = first
            .engine()
            .write(Action::CreateChat {
                others,
                topic: Some((*topic).to_owned()),
            })
            .await
            .unwrap();
        assert_eq!(row.state, OutboxState::Sent, "{row:?}");
    }
}

/// `caller` starts the call in `chat`; each of `answerers` answers its ring. Each one's call id, the caller's first.
async fn start_and_answer(caller: &mut Phone, answerers: &mut [&mut Phone], chat: &str) -> Vec<CallId> {
    let marks: Vec<usize> = answerers.iter().map(|phone| phone.mark()).collect();
    let placed = caller.mark();
    let call = caller.act(CallAction::Call { chat: chat.to_owned() }).await;
    caller.status(placed, call, &CallStatus::Connected).await;
    let mut calls = vec![call];
    for (phone, mark) in answerers.iter_mut().zip(marks) {
        let rings = phone.incoming(mark).await;
        assert_eq!(rings.caller.as_str(), caller.mri, "{}: the caller", phone.upn);
        assert_eq!(
            rings.group_chat.as_deref(),
            Some(chat),
            "{}: rung for the group chat",
            phone.upn
        );
        let answered = phone.act(CallAction::Answer { call: rings.call }).await;
        let at = phone
            .since(mark, "connected", |event| {
                matches!(event, Event::Call { call: of, status: CallStatus::Connected | CallStatus::Ended(_) }
                    if *of == answered)
            })
            .await;
        let events: Vec<String> = phone.log[mark..].iter().map(brief).collect();
        assert!(
            matches!(
                phone.log[at],
                Event::Call {
                    status: CallStatus::Connected,
                    ..
                }
            ),
            "{}: the answered call ended: {events:?}",
            phone.upn
        );
        calls.push(answered);
    }
    calls
}

/// The view of `own`'s call shows each of `mris` but `own` (a view lists the others).
fn in_call<'a>(own: &'a str, mris: &'a [String]) -> impl Fn(&CallView) -> bool + 'a {
    move |view| {
        mris.iter()
            .filter(|mri| *mri != own)
            .all(|mri| view.participants.iter().any(|party| &party.mri == mri))
    }
}

fn speaks(mri: &str) -> impl Fn(&CallView) -> bool + '_ {
    move |view| view.participants.iter().any(|party| party.mri == mri && party.speaking)
}

/// Until `phone`'s latest view of `call` is `wanted` (it may be already).
async fn shows_now(phone: &mut Phone, call: CallId, what: &str, wanted: impl Fn(&CallView) -> bool) {
    let deadline = tokio::time::Instant::now() + super::support::WAIT;
    loop {
        let latest = phone.log.iter().rev().find_map(|event| match event {
            Event::CallParticipants { call: of, view } if *of == call => Some(view.clone()),
            _ => None,
        });
        if latest.as_ref().is_some_and(&wanted) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{}: not {what}; the latest view {latest:?}",
            phone.upn
        );
        let mark = phone.mark();
        phone.quiet(mark, Duration::from_millis(200), |_| false).await;
    }
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

/// `phone`'s mute of `call` set to `muted`; each of `others` sees it in its view of its own call.
async fn mute(phone: &Phone, call: CallId, muted: bool, others: &mut [(&mut Phone, CallId)]) {
    let marks: Vec<usize> = others.iter().map(|(other, _)| other.mark()).collect();
    phone.act(CallAction::Mute { call, muted }).await;
    let mri = phone.mri.clone();
    for ((other, their), mark) in others.iter_mut().zip(marks) {
        other
            .view(mark, *their, &format!("{} muted {muted}", phone.upn), |view| {
                view.participants
                    .iter()
                    .any(|party| party.mri == mri && party.muted == muted)
            })
            .await;
    }
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

/// Four people in a group chat: a, b and c in its call (their call ids), d rung too.
struct Four {
    a: Phone,
    b: Phone,
    c: Phone,
    d: Phone,
    chat: String,
    calls: [CallId; 3],
    mris: Vec<String>,
}

/// How long the media runs after a change before what was heard is judged.
const SETTLE: Duration = Duration::from_secs(5);

impl Four {
    /// a starts the call in the chat (among synthetic members and more chats); b and c answer; d's ring goes on.
    /// Where d's log stood before the ring.
    async fn start(group: &Group) -> (Self, usize) {
        let a = group.phone(0, TONES[0]).await;
        let mut b = group.phone(1, TONES[1]).await;
        let mut c = group.phone(2, TONES[2]).await;
        let d = group.phone(3, TONES[3]).await;
        seed_chats(&a, &group.synthetic).await;
        let chat = group_chat(&a, &[&b, &c, &d], &group.synthetic[..6], "Q4 ✨ «planning» & co").await;
        let rang = d.mark();
        let mut a = a;
        let calls = start_and_answer(&mut a, &mut [&mut b, &mut c], &chat).await;
        let mris = vec![a.mri.clone(), b.mri.clone(), c.mri.clone()];
        let four = Self {
            a,
            b,
            c,
            d,
            chat,
            calls: [calls[0], calls[1], calls[2]],
            mris,
        };
        (four, rang)
    }

    /// Each of the three sees the other two, and hears them and not itself.
    async fn three_hear_each_other(&mut self) {
        let [ta, tb, tc, td] = TONES;
        for (phone, call) in [
            (&mut self.a, self.calls[0]),
            (&mut self.b, self.calls[1]),
            (&mut self.c, self.calls[2]),
        ] {
            let own = phone.mri.clone();
            phone
                .view(0, call, "the three in the call", in_call(&own, &self.mris))
                .await;
        }
        tokio::time::sleep(SETTLE).await;
        let sides: [(&Phone, &[Tone], &[Tone]); 3] = [
            (&self.a, &[tb, tc], &[ta, td]),
            (&self.b, &[ta, tc], &[tb, td]),
            (&self.c, &[ta, tb], &[tc, td]),
        ];
        judge(&sides, "three in the call").await;
    }

    /// `who` (0 a, 1 b, 2 c) sets its mute; the other two see it.
    async fn mute(&mut self, who: usize, muted: bool) {
        let [ca, cb, cc] = self.calls;
        match who {
            0 => mute(&self.a, ca, muted, &mut [(&mut self.b, cb), (&mut self.c, cc)]).await,
            1 => mute(&self.b, cb, muted, &mut [(&mut self.a, ca), (&mut self.c, cc)]).await,
            _ => mute(&self.c, cc, muted, &mut [(&mut self.a, ca), (&mut self.b, cb)]).await,
        }
    }

    /// Everyone mutes, then a unmutes and speaks: a alone is heard, and shows as speaking at b and c.
    async fn only_a_speaks(&mut self) {
        let [ta, tb, tc, _] = TONES;
        for who in 0..3 {
            self.mute(who, true).await;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        self.mute(0, false).await;
        shows_now(&mut self.b, self.calls[1], "a speaking at b", speaks(&self.mris[0])).await;
        shows_now(&mut self.c, self.calls[2], "a speaking at c", speaks(&self.mris[0])).await;
        tokio::time::sleep(SETTLE).await;
        let sides: [(&Phone, &[Tone], &[Tone]); 3] = [
            (&self.a, &[], &[tb, tc]),
            (&self.b, &[ta], &[tc]),
            (&self.c, &[ta], &[tb]),
        ];
        judge(&sides, "only a unmuted").await;
    }

    /// b unmutes and a mutes: b is heard and is the speaker at a and c. Then all unmute, and all are heard again.
    async fn then_b_then_all(&mut self) {
        let [ta, tb, tc, _] = TONES;
        self.mute(1, false).await;
        self.mute(0, true).await;
        shows_now(&mut self.a, self.calls[0], "b speaking at a", speaks(&self.mris[1])).await;
        shows_now(&mut self.c, self.calls[2], "b speaking at c", speaks(&self.mris[1])).await;
        tokio::time::sleep(SETTLE).await;
        let sides: [(&Phone, &[Tone], &[Tone]); 2] = [(&self.a, &[tb], &[tc]), (&self.c, &[tb], &[ta])];
        judge(&sides, "only b unmuted").await;
        self.mute(0, false).await;
        self.mute(2, false).await;
        tokio::time::sleep(SETTLE).await;
        let sides: [(&Phone, &[Tone], &[Tone]); 3] = [
            (&self.a, &[tb, tc], &[ta]),
            (&self.b, &[ta, tc], &[tb]),
            (&self.c, &[ta, tb], &[tc]),
        ];
        judge(&sides, "all unmuted again").await;
    }

    /// d's ring runs out; then d joins the call from the chat, hears the three and is heard. d's call id.
    async fn late_joiner(&mut self, before_ring: usize) -> CallId {
        let [ta, tb, tc, td] = TONES;
        let d = &mut self.d;
        let invitation = d.incoming(before_ring).await;
        assert_eq!(invitation.group_chat.as_deref(), Some(self.chat.as_str()));
        let end = d.ring_ended(before_ring, invitation.call).await;
        eprintln!("d's ring ended: {end:?}");
        d.sync().await;
        let joined_at = d.mark();
        let joined = d
            .act(CallAction::Join {
                chat: self.chat.clone(),
            })
            .await;
        d.status(joined_at, joined, &CallStatus::Connected).await;
        self.mris.push(d.mri.clone());
        for (phone, call) in [(&mut self.a, self.calls[0]), (&mut self.d, joined)] {
            let own = phone.mri.clone();
            phone
                .view(0, call, "the four in the call", in_call(&own, &self.mris))
                .await;
        }
        tokio::time::sleep(SETTLE).await;
        let sides: [(&Phone, &[Tone], &[Tone]); 3] = [
            (&self.d, &[ta, tb, tc], &[td]),
            (&self.a, &[tb, tc, td], &[ta]),
            (&self.b, &[ta, tc, td], &[tb]),
        ];
        judge(&sides, "d joined late").await;
        joined
    }

    /// c leaves: nobody hears c, the call goes on for the others.
    async fn one_leaves(&mut self) {
        let [ta, tb, tc, td] = TONES;
        hang_up(&mut self.c, self.calls[2]).await;
        let gone = self.c.mri.clone();
        shows_now(&mut self.a, self.calls[0], "c gone", |view| {
            view.participants.iter().all(|party| party.mri != gone)
        })
        .await;
        tokio::time::sleep(SETTLE).await;
        let sides: [(&Phone, &[Tone], &[Tone]); 3] = [
            (&self.a, &[tb, td], &[tc, ta]),
            (&self.b, &[ta, td], &[tc, tb]),
            (&self.d, &[ta, tb], &[tc, td]),
        ];
        judge(&sides, "c left").await;
    }

    /// The last ones hang up: b, d, then a; the call is over for all, and the chat says who was in it, at everyone.
    async fn the_end(mut self, joined: CallId) {
        let seen = self.c.mark();
        hang_up(&mut self.b, self.calls[1]).await;
        hang_up(&mut self.d, joined).await;
        let a_mark = self.a.mark();
        hang_up(&mut self.a, self.calls[0]).await;
        assert!(
            !self.a.log[a_mark..]
                .iter()
                .any(|event| matches!(event, Event::IncomingCall(_))),
            "a rang"
        );
        let (everyone, chat) = (self.mris.clone(), self.chat.clone());
        self.c
            .pushed_line(seen, &chat, "the call's end", ended_with(&everyone))
            .await;
        for phone in [&mut self.a, &mut self.b, &mut self.c, &mut self.d] {
            let lines = phone.stored_lines(&chat).await;
            eprintln!("{}: the chat's call lines: {lines:?}", phone.upn);
            phone.shows_lines(&chat, &[&started, &ended_with(&everyone)]).await;
            phone.close().await;
        }
    }
}

/// Three in a group call, mute and the speaker's mark, a late joiner, one leaving, and the end.
async fn three_then_four(layout: Layout) {
    let people = [
        ("ada@own.test", "Ada Ł. Lovelace"),
        ("bert@own.test", "Bert 🎺 Bach"),
        ("cora@own.test", "Cora 'Cee' Nuñez"),
        ("dov@own.test", "Dov ד׳ Levi"),
    ];
    let Some(group) = Group::start(layout, "three-then-four", &people, false).await else {
        return;
    };
    let (mut four, rang) = Four::start(&group).await;
    four.three_hear_each_other().await;
    four.only_a_speaks().await;
    four.then_b_then_all().await;
    let joined = four.late_joiner(rang).await;
    four.one_leaves().await;
    four.the_end(joined).await;
    group.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_hear_each_other_mute_speak_a_late_joiner_one_leaves_and_the_end_on_one_node() {
    three_then_four(Layout::All).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_hear_each_other_mute_speak_a_late_joiner_one_leaves_and_the_end_on_api_and_media_nodes() {
    three_then_four(Layout::Split).await;
}

/// The call ended as the service's, with `code`.
fn ended_by_the_service(end: &End, code: i64) {
    let End::Remote(remote) = end else {
        panic!("not the service's end: {end:?}");
    };
    assert_eq!(remote.code, Some(code), "{end:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_killed_media_node_ends_the_call_for_everyone_and_a_new_one_starts_once_a_node_runs_again() {
    let people = [
        ("ada@own.test", "Ada"),
        ("bert@own.test", "Bert"),
        ("cora@own.test", "Cora"),
    ];
    let Some(mut group) = Group::start(Layout::Split, "media-killed", &people, false).await else {
        return;
    };
    let mut a = group.phone(0, TONES[0]).await;
    let mut b = group.phone(1, TONES[1]).await;
    let mut c = group.phone(2, TONES[2]).await;
    let chat = group_chat(&a, &[&b, &c], &group.synthetic[..2], "Incident 🔥 room").await;
    let [ta, tb, tc, _] = TONES;
    for round in 0..2 {
        let marks = [a.mark(), b.mark(), c.mark()];
        let calls = start_and_answer(&mut a, &mut [&mut b, &mut c], &chat).await;
        tokio::time::sleep(TALK + Duration::from_secs(1)).await;
        judge(
            &[(&a, &[tb, tc], &[ta]), (&b, &[ta, tc], &[tb]), (&c, &[ta, tb], &[tc])],
            &format!("round {round}"),
        )
        .await;
        if round == 1 {
            for (phone, call) in [(&mut b, calls[1]), (&mut c, calls[2]), (&mut a, calls[0])] {
                hang_up(phone, call).await;
            }
            break;
        }
        // the media node's process dies (a crash, a lost machine): the call ends for everyone
        let killed = Instant::now();
        group.media.as_mut().unwrap().kill();
        for ((phone, call), mark) in [(&mut a, calls[0]), (&mut b, calls[1]), (&mut c, calls[2])]
            .into_iter()
            .zip(marks)
        {
            let end = phone.ended(mark, call).await;
            eprintln!(
                "{}: the call ended {:?} after the kill: {end:?}",
                phone.upn,
                killed.elapsed()
            );
            ended_by_the_service(&end, 503);
        }
        // the chat tells its end
        let people = [a.mri.clone(), b.mri.clone(), c.mri.clone()];
        a.shows_lines(&chat, &[&started, &ended_with(&people)]).await;
        // no media node: a new start fails at once
        let from = a.mark();
        match a.press(CallAction::Call { chat: chat.clone() }).await {
            Err(error) => eprintln!("no media node: the client refused the call: {error}"),
            Ok(call) => {
                let end = a.ended(from, call).await;
                eprintln!(
                    "no media node: the call ended {end:?}; events {:?}",
                    a.log[from..].iter().map(brief).collect::<Vec<_>>()
                );
                assert_eq!(end, End::Failed("start"));
            }
        }
        // a media node runs again
        group.start_media().await;
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let everyone = [a.mri.clone(), b.mri.clone(), c.mri.clone()];
    let lines = a.stored_lines(&chat).await;
    eprintln!("the chat's call lines: {lines:?}");
    a.shows_lines(
        &chat,
        &[&started, &ended_with(&everyone), &started, &ended_with(&everyone)],
    )
    .await;
    for phone in [&mut a, &mut b, &mut c] {
        phone.close().await;
    }
    group.stop().await;
}

/// a starts a group call and no media node takes it: the start fails, nobody rings.
async fn no_media_node(layout: Layout) {
    let people = [
        ("ada@own.test", "Ada"),
        ("bert@own.test", "Bert"),
        ("cora@own.test", "Cora"),
    ];
    let Some(group) = Group::start(layout, "no-media-node", &people, true).await else {
        return;
    };
    let mut a = group.phone(0, TONES[0]).await;
    let mut b = group.phone(1, TONES[1]).await;
    let c = group.phone(2, TONES[2]).await;
    let chat = group_chat(&a, &[&b, &c], &[], "No mixer here").await;
    let (from, to) = (a.mark(), b.mark());
    match a.press(CallAction::Call { chat }).await {
        Err(error) => eprintln!("{layout:?}: the client refused the call: {error}"),
        Ok(call) => {
            let end = a.ended(from, call).await;
            let events: Vec<String> = a.log[from..].iter().map(brief).collect();
            eprintln!("{layout:?}: the call ended {end:?}; a's events {events:?}");
            assert_eq!(end, End::Failed("start"));
        }
    }
    // the server answered the start 503, as its log says (when the run keeps the nodes' logs)
    if let Some(log) = &group.api_log {
        let text = std::fs::read_to_string(log).unwrap();
        assert!(
            text.contains("no media node takes it") && text.contains("503 Service Unavailable"),
            "the start wasn't refused with 503: {text}"
        );
    }
    assert!(
        b.quiet(to, Duration::from_secs(3), |event| matches!(
            event,
            Event::IncomingCall(_)
        ))
        .await,
        "b rang"
    );
    group.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_group_start_fails_without_a_media_node_on_one_api_node() {
    no_media_node(Layout::All).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_group_start_fails_without_a_media_node_on_api_and_media_nodes() {
    no_media_node(Layout::Split).await;
}

/// A process's CPU time so far (user and system), from `/proc`.
fn cpu_time(pid: u32) -> Duration {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let after = &stat[stat.rfind(')').unwrap() + 2..];
    let fields: Vec<&str> = after.split(' ').collect();
    // utime and stime are the 14th and 15th fields; `after` starts at the 3rd
    let ticks: u64 = fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap();
    Duration::from_millis(ticks * 10)
}

/// Where each quiet 100 ms window of `samples` (RMS below `floor`) lies, as time since `base`, among the windows
/// within `span` of it; the recording started at `started` (`base` + that). The gaps in what a speaker played.
fn gaps(samples: &[f32], started: Duration, span: (Duration, Duration), floor: f32) -> (usize, Vec<Duration>) {
    let at = |time: Duration| usize::try_from(time.saturating_sub(started).as_millis()).unwrap() * 48;
    let (from, to) = (at(span.0), at(span.1).min(samples.len()));
    let windows: Vec<f32> = samples
        .get(from..to)
        .unwrap_or_default()
        .as_chunks::<4800>()
        .0
        .iter()
        .map(|chunk| (chunk.iter().map(|sample| sample * sample).sum::<f32>() / 4800.0).sqrt())
        .collect();
    let quiet = windows
        .iter()
        .enumerate()
        .filter(|(_, rms)| **rms < floor)
        .map(|(index, _)| span.0.max(started) + Duration::from_millis(100 * u64::try_from(index).unwrap()))
        .collect();
    (windows.len(), quiet)
}

/// One receiver's recording after the call: the tones' levels in the last 20 s of the talk, and its gaps.
struct Heard {
    levels: Vec<f32>,
    windows: usize,
    gaps: Vec<Duration>,
}

fn analyse(recording: &Path, tones: &[Tone], started: Duration, span: (Duration, Duration)) -> Heard {
    let samples = wav::read(recording).unwrap();
    let at = |time: Duration| usize::try_from(time.saturating_sub(started).as_millis()).unwrap() * 48;
    let end = at(span.1).min(samples.len());
    let last = &samples[end.saturating_sub(20 * 48_000)..end];
    let levels = tones.iter().map(|tone| level_at(last, tone.hz)).collect();
    let (windows, gaps) = gaps(&samples, started, span, 0.05);
    Heard { levels, windows, gaps }
}

/// What each receiver's stats said in each 10 s of the talk: the audio packets it got (50 a second come), and the
/// share it reports lost.
#[derive(Default)]
struct Received {
    packets: Vec<u64>,
    worst_loss: f64,
    worst_jitter: f64,
}

/// The six's talk, read every 10 s: the media node's CPU, and each receiver's audio stats.
async fn talk(
    phones: &[Phone],
    calls: &[CallId],
    pid: u32,
    seconds: u64,
) -> (Vec<Received>, String, f64, f64, Duration) {
    let connected = Instant::now();
    let (cpu_start, mut cpu_last, mut at_last) = (cpu_time(pid), cpu_time(pid), Instant::now());
    let mut received: Vec<Received> = phones.iter().map(|_| Received::default()).collect();
    let mut last_packets = vec![0_u64; phones.len()];
    let (mut report, mut peak) = (String::new(), 0.0_f64);
    while connected.elapsed() < Duration::from_secs(seconds) {
        tokio::time::sleep(Duration::from_secs(10)).await;
        let (cpu, now) = (cpu_time(pid), Instant::now());
        let share = cpu.saturating_sub(cpu_last).as_secs_f64() / (now - at_last).as_secs_f64() * 100.0;
        peak = peak.max(share);
        (cpu_last, at_last) = (cpu, now);
        let _ = write!(
            report,
            "t={:>3}s media node CPU {share:5.1}%;",
            connected.elapsed().as_secs()
        );
        for (index, (phone, call)) in phones.iter().zip(calls).enumerate() {
            let health = phone.engine().call_health(*call).await.unwrap();
            let audio = health.and_then(|health| health.audio.received).unwrap_or_default();
            let (loss, jitter) = (audio.loss_percent.unwrap_or(0.0), audio.jitter_ms.unwrap_or(0.0));
            let got = &mut received[index];
            got.packets.push(audio.packets - last_packets[index]);
            got.worst_loss = got.worst_loss.max(loss);
            got.worst_jitter = got.worst_jitter.max(jitter);
            last_packets[index] = audio.packets;
            let name = phone.upn.split('@').next().unwrap();
            let _ = write!(
                report,
                " {name}: {} pkts, loss {loss:.1}%, jitter {jitter:.0} ms;",
                audio.packets
            );
        }
        report.push('\n');
    }
    let talked = connected.elapsed();
    let average = cpu_time(pid).saturating_sub(cpu_start).as_secs_f64() / talked.as_secs_f64() * 100.0;
    (received, report, average, peak, talked)
}

/// Six people talk at once for `NGTS_QA_LOAD_SECONDS` (30 s unless set; QA ran 120), the media node a process of
/// its own: every receiver's stats show its audio whole (no loss, every packet), it hears the five others and never
/// itself, and what it played is quiet in 2 % of the talk at most (a loaded host starves the clients, which all run
/// in this process, for a moment now and then: so do three 1:1 calls without a media node). The media node's CPU
/// is printed.
/// Everyone hangs up, the callees first, and closes; when each hung up after `connected`, in `phones`' order (each
/// recording ends at its hang-up, which says when it started).
async fn hang_up_callees_first(phones: &mut [Phone], calls: &[CallId], connected: Instant) -> Vec<Duration> {
    let mut ends = Vec::new();
    let (first, rest) = phones.split_first_mut().unwrap();
    for (phone, call) in rest.iter_mut().zip(&calls[1..]) {
        hang_up(phone, *call).await;
        ends.push(connected.elapsed());
    }
    hang_up(first, calls[0]).await;
    ends.insert(0, connected.elapsed());
    for phone in phones {
        phone.close().await;
    }
    ends
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn six_people_talk_for_a_while_without_a_gap() {
    let seconds: u64 = std::env::var("NGTS_QA_LOAD_SECONDS")
        .ok()
        .and_then(|seconds| seconds.parse().ok())
        .unwrap_or(30);
    let people = [
        ("p1@own.test", "Pia Ünal"),
        ("p2@own.test", "Quinn 量子"),
        ("p3@own.test", "Rafa Ñ"),
        ("p4@own.test", "Sam O'Neil"),
        ("p5@own.test", "Tò Mì"),
        ("p6@own.test", "Uma 🌺"),
    ];
    let Some(group) = Group::start(Layout::Split, "six-talk", &people, false).await else {
        return;
    };
    let tones: Vec<Tone> = [440.0, 620.0, 810.0, 1030.0, 1270.0, 1490.0]
        .iter()
        .map(|hz| tone(*hz, 0.2))
        .collect();
    let mut phones = Vec::new();
    for (index, tone) in tones.iter().enumerate() {
        phones.push(group.phone(index, *tone).await);
    }
    let (first, rest) = phones.split_first_mut().unwrap();
    let others: Vec<&Phone> = rest.iter().collect();
    let chat = group_chat(first, &others, &group.synthetic, "All hands 🙌 (six)").await;
    let mut answerers: Vec<&mut Phone> = rest.iter_mut().collect();
    let calls = start_and_answer(first, &mut answerers, &chat).await;
    let connected = Instant::now();
    let (received, report, average, peak, talked) = talk(&phones, &calls, group.media_pid(), seconds).await;
    let ends = hang_up_callees_first(&mut phones, &calls, connected).await;
    group.stop().await;
    eprintln!("{report}");
    eprintln!(
        "six in a call for {talked:?}: the media node's CPU {average:.1}% on average, {peak:.1}% at the most (one core = \
         100%, a debug build)"
    );
    let span = (Duration::from_secs(5), talked);
    let mut failures = Vec::new();
    for (((index, phone), end), got) in phones.iter().enumerate().zip(ends).zip(&received) {
        let (recording, tones) = (phone.recording.clone(), tones.clone());
        let played = wav::read(&recording).unwrap().len();
        let started = end.saturating_sub(Duration::from_millis(u64::try_from(played / 48).unwrap()));
        let heard = tokio::task::spawn_blocking(move || analyse(&recording, &tones, started, span))
            .await
            .unwrap();
        eprintln!(
            "{}: heard {:.3?} (own {:.3}); packets each 10 s {:?}, worst loss {:.1}%, worst jitter {:.0} ms; {} of {} \
             100 ms windows quiet, at {:?}",
            phone.upn,
            heard.levels,
            heard.levels[index],
            got.packets,
            got.worst_loss,
            got.worst_jitter,
            heard.gaps.len(),
            heard.windows,
            heard.gaps
        );
        let others_heard = heard
            .levels
            .iter()
            .enumerate()
            .all(|(other, level)| other == index || *level > 0.06);
        let whole = got.worst_loss <= 1.0 && got.packets.iter().all(|packets| *packets >= 475);
        if !others_heard || heard.levels[index] > 0.02 || !whole || heard.gaps.len() * 50 > heard.windows {
            failures.push(phone.upn.clone());
        }
    }
    assert!(
        failures.is_empty(),
        "lost audio, a missing tone or long gaps at {failures:?}"
    );
}
