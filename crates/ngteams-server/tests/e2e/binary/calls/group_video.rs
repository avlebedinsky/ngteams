//! Group calls' video and screen sharing as users meet them on an own server: the real `ngteams-server` binary
//! (`serve --role all` on SQLite, or `--role api` and `--role media` as two processes on one Postgres and Redis), the
//! run's coturn, and each account's client as the app runs it ([`Phone`]), its camera the file backend's test pattern.
//! Each client's camera has a shape of its own, so the picture on a receiving line says whose camera it is. The
//! accounts sit among synthetic people with odd names, in a group chat with members who have no client.
//!
//! - three in a call, every camera on: each sees the other two, each on a line of its own, and never itself, while
//!   the audio stays mixed; a camera off is seen as off and its line shows nothing more, on again it comes back; a late
//!   joiner sees the cameras that are on and is seen; one who leaves is gone from the others' views and its line
//!   stops; on two nodes, a killed media node ends the call for everyone;
//! - one shares the screen: the others see it, Stop sharing ends it for them; a second sharer takes over as Teams
//!   does;
//! - six with their cameras on: every receiver's frame rate on each line and the media node's CPU are noted;
//! - eleven with their cameras on, more than a client's nine receiving lines: each line shows the one its client asks
//!   for; when one with a line leaves, the line goes to the one that had none, and its video comes on it.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use teams_core::engine::{Action, Event};
use teams_core::{CallAction, CallId, CallParticipant, CallStatus, CallView, End};
use teams_media::devices::file::PatternCamera;
use teams_media::session::SHARING_LINE;
use teams_media::tone::Tone;
use teams_proto::Mri;
use teams_store::OutboxState;

use super::super::support::{Deployment, Node, Port};
use super::support::{FRAMES, Phone, Seen, WAIT, brief, coturn};
use crate::support::{Backend, PASSWORD, PG_ENV, redis_url};

/// A tone of the mix is heard at least this loud (the mix of two 0.5 tones is soft-clipped).
const HEARD: f32 = 0.15;
/// Below this, a tone isn't in a recording.
const ABSENT: f32 = 0.05;
/// How long the media runs before what was heard is judged.
const TALK: Duration = Duration::from_secs(4);
const RING: u64 = 12;

/// The tests here run one at a time, also beside each other (`--test-threads 4`): each one runs many clients' video.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const fn tone(hz: f32, amplitude: f32) -> Tone {
    Tone { hz, amplitude }
}

/// Four people's tones: none a low harmonic of another's (a clipped mix has odd harmonics).
const TONES: [Tone; 4] = [tone(440.0, 0.5), tone(660.0, 0.5), tone(880.0, 0.5), tone(1170.0, 0.5)];

const fn camera(width: u32, height: u32, fps: u32) -> PatternCamera {
    PatternCamera { width, height, fps }
}

/// The cameras' shapes, each far enough from the others' (more than 12 %) to tell after libwebrtc's scaling.
const SHAPES: [PatternCamera; 11] = [
    camera(640, 360, 15), // 1.78
    camera(480, 360, 15), // 1.33
    camera(360, 360, 15), // 1.00
    camera(270, 360, 15), // 0.75
    camera(640, 320, 15), // 2.00
    camera(200, 360, 15), // 0.56
    camera(560, 360, 15), // 1.56
    camera(416, 360, 15), // 1.16
    camera(320, 360, 15), // 0.89
    camera(240, 360, 15), // 0.67
    camera(640, 266, 15), // 2.41
];

/// Synthetic people with odd names: members of the chats with no client of their own.
const ODD_NAMES: [&str; 8] = [
    "Zoë O'Brien-Ångström",
    "李 小龍",
    "ابن سينا",
    "Dr. 🦀 Ferris",
    "Robert'); DROP TABLE users;--",
    "<b>Bold</b> & \"Quoted\"",
    "Ana   Maria   de la Cruz",
    "Name-That-Is-Very-Long-Indeed-For-A-Person-Who-Has-Many-Middle-Names-Alpha-Beta-Gamma",
];

/// A picture's shape: its width over its height.
fn shape(width: u32, height: u32) -> f64 {
    f64::from(width) / f64::from(height.max(1))
}

/// Whether `seen`'s last picture has `camera`'s shape (within 6 %).
fn shaped(seen: Seen, camera: PatternCamera) -> bool {
    let wanted = shape(camera.width, camera.height);
    (shape(seen.width, seen.height) / wanted - 1.0).abs() < 0.06
}

/// How the deployment runs its group calls' media.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layout {
    /// One `serve` (`--role all`, the default) on SQLite.
    All,
    /// `serve --role api` and `serve --role media`, two processes on one Postgres and Redis.
    Split,
}

/// The deployment, its people, and where its nodes' logs go.
struct Group {
    origin: String,
    api: Node,
    media: Option<Node>,
    people: Vec<(String, String)>,
    synthetic: Vec<String>,
    logs: Option<PathBuf>,
    starts: u32,
    _internal: Option<Port>,
    _deployment: Deployment,
    _one: tokio::sync::MutexGuard<'static, ()>,
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

/// `count` synthetic people of the test: `p<n>@own.test`, odd names.
fn people(count: usize) -> Vec<(String, String)> {
    const NAMES: [&str; 11] = [
        "Ada Ł. Lovelace",
        "Bert 🎺 Bach",
        "Cora 'Cee' Nuñez",
        "Dov ד׳ Levi",
        "Eli Ωmega",
        "Fay 量子",
        "Gus Ñandú",
        "Hana 🌺",
        "Ivo Škoda",
        "Jo <i>Jo</i>",
        "Kai O'Neil",
    ];
    NAMES
        .iter()
        .take(count)
        .enumerate()
        .map(|(index, name)| (format!("p{index}@own.test"), (*name).to_owned()))
        .collect()
}

impl Group {
    /// `None` (skipped, and said) without a coturn, or for [`Layout::Split`] without Postgres and Redis.
    async fn start(layout: Layout, flow: &str, count: usize) -> Option<Self> {
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
        let one = ONE_AT_A_TIME.lock().await;
        eprintln!("group video, {flow}: {layout:?}");
        let mut deployment = Deployment::new(backend, false, redis).await;
        deployment.origin = deployment.origin.replacen("http:", "https:", 1);
        let mut config = format!(
            "[calling]\nring_timeout = {RING}\n[calling.turn]\nsecret = {secret:?}\nurls = [\"turn:{relay}?transport=udp\"]\n"
        );
        // the loopback coturn relays from 127.0.0.1: the node's candidate and its replies are there too
        config.push_str("[mixer]\nudp_listen = \"127.0.0.1:0\"\n");
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
            (None, None)
        };
        api.cli(&["migrate"], "");
        let people = people(count);
        for (upn, name) in &people {
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
            .map(|dir| Path::new(&dir).join(format!("group-video-{flow}-{layout:?}")));
        let mut group = Self {
            origin: deployment.origin.clone(),
            api,
            media,
            people,
            synthetic,
            logs,
            starts: 0,
            _internal: internal,
            _deployment: deployment,
            _one: one,
        };
        let log = group.log("api");
        match log {
            Some(log) => group.api.start_logged(&log).await,
            None => group.api.start().await,
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

    async fn start_media(&mut self) {
        let log = self.log("media");
        let media = self.media.as_mut().unwrap();
        match log {
            Some(log) => media.start_logged(&log).await,
            None => media.start().await,
        }
    }

    /// The process that runs the media: the media node's, else the one node's.
    fn media_pid(&self) -> u32 {
        self.media.as_ref().unwrap_or(&self.api).pid().unwrap()
    }

    /// A running client of the `index`th person: its microphone `tone`, its camera of the shape `camera`.
    async fn phone(&self, index: usize, tone: Tone, camera: PatternCamera) -> Phone {
        let (upn, name) = &self.people[index];
        let mut phone = Phone::signed_in(&self.api, &self.origin, (upn, name), tone).await;
        phone.camera = camera;
        phone.start();
        phone.online(0).await;
        phone
    }

    /// The clients of the first `count` people, each its own camera shape; quiet tones but the first four's.
    async fn phones(&self, count: usize) -> Vec<Phone> {
        let mut phones = Vec::new();
        for (index, shape) in SHAPES.iter().take(count).enumerate() {
            #[expect(clippy::cast_precision_loss, reason = "a few people")]
            let tone = TONES
                .get(index)
                .copied()
                .unwrap_or_else(|| self::tone(1400.0 + 90.0 * index as f32, 0.1));
            phones.push(self.phone(index, tone, *shape).await);
        }
        phones
    }

    async fn stop(mut self) {
        if let Some(media) = &mut self.media {
            media.stop().await;
        }
        self.api.stop().await;
    }
}

/// The group chat `first` makes with `others` and `quiet` (members with no client), topic `topic`, once every other
/// client has it.
async fn group_chat(first: &Phone, others: &[&Phone], quiet: &[String], topic: &str) -> String {
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

/// `caller` starts the call in `chat`; each of `answerers` answers its ring. Each one's call id, the caller's first.
async fn start_and_answer(caller: &mut Phone, answerers: &mut [&mut Phone], chat: &str) -> Vec<CallId> {
    let marks: Vec<usize> = answerers.iter().map(|phone| phone.mark()).collect();
    let placed = caller.mark();
    let call = caller.act(CallAction::Call { chat: chat.to_owned() }).await;
    caller.status(placed, call, &CallStatus::Connected).await;
    let mut calls = vec![call];
    for (phone, mark) in answerers.iter_mut().zip(marks) {
        let rings = phone.incoming(mark).await;
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
            "{}: {what} didn't happen in time; its lines {:?}; the latest view {:?}",
            phone.upn,
            phone.video.lines(),
            phone
                .log
                .iter()
                .rev()
                .find(|event| matches!(event, Event::CallParticipants { .. }))
        );
        let mark = phone.mark();
        phone.quiet(mark, Duration::from_millis(100), |_| false).await;
    }
}

/// Until `phone`'s latest view of `call` is `wanted` (it may be already).
async fn shows(phone: &mut Phone, call: CallId, what: &str, wanted: impl Fn(&CallView) -> bool) {
    until(phone, what, |phone| latest(phone, call).as_ref().is_some_and(&wanted)).await;
}

/// Waits until `phone` shows each of `others` with its camera on, on a receiving line, and that line has shown
/// [`FRAMES`] more frames than `before` gives (`before(line)`), the last of the shape that other's camera has.
/// Gives the line of each other, in order.
async fn sees_since(
    phone: &mut Phone,
    call: CallId,
    others: &[(&str, PatternCamera)],
    before: impl Fn(usize) -> u64,
) -> Vec<usize> {
    let what = format!("{} cameras on, each on a line", others.len());
    shows(phone, call, &what, |view| {
        others.iter().all(|(mri, _)| {
            party(view, mri).is_some_and(|party| party.video_on && party.line.is_some_and(|line| line >= 1))
        })
    })
    .await;
    let view = latest(phone, call).unwrap();
    let mut lines = Vec::new();
    for (mri, camera) in others {
        let line = party(&view, mri).and_then(|party| party.line).unwrap();
        lines.push(line);
        let since = before(line);
        let what = format!("{mri}'s video ({}×{}) on line {line}", camera.width, camera.height);
        until(phone, &what, |phone| {
            let seen = phone.video.line(line);
            seen.frames >= since + FRAMES && shaped(seen, *camera)
        })
        .await;
    }
    let mut sorted = lines.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), lines.len(), "{}: two on one line: {lines:?}", phone.upn);
    lines
}

async fn sees(phone: &mut Phone, call: CallId, others: &[(&str, PatternCamera)]) -> Vec<usize> {
    sees_since(phone, call, others, |_| 0).await
}

/// The frames each of `lines` of `phone` shows a second, over `over` (its events pumped meanwhile).
async fn rates(phone: &mut Phone, lines: &[usize], over: Duration) -> Vec<f64> {
    let before: Vec<u64> = lines.iter().map(|line| phone.video.line(*line).frames).collect();
    let started = Instant::now();
    let mark = phone.mark();
    phone.quiet(mark, over, |_| false).await;
    let took = started.elapsed().as_secs_f64();
    #[expect(clippy::cast_precision_loss, reason = "frame counts are small")]
    let rates = lines
        .iter()
        .zip(before)
        .map(|(line, before)| (phone.video.line(*line).frames - before) as f64 / took)
        .collect();
    rates
}

/// `line` of `phone` shows no more than two frames more over a second (its events pumped meanwhile).
async fn stays_still(phone: &mut Phone, line: usize, what: &str) {
    let shown = phone.video.line(line).frames;
    let mark = phone.mark();
    phone.quiet(mark, Duration::from_secs(1), |_| false).await;
    let after = phone.video.line(line).frames;
    assert!(
        after <= shown + 2,
        "{}: {what}: line {line} went on, {shown} → {after}",
        phone.upn
    );
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
    for ((phone, _, _), (heard, absent)) in sides.iter().zip(&levels) {
        eprintln!("{what}: {} heard {heard:.3?}, absent {absent:.3?}", phone.upn);
    }
    assert!(ok(&levels), "{what}: {levels:.3?}");
}

/// `leaver` hangs up: its call ends as hung up.
async fn hang_up(leaver: &mut Phone, call: CallId) {
    let mark = leaver.mark();
    leaver.act(CallAction::HangUp { call }).await;
    assert_eq!(leaver.ended(mark, call).await, End::HungUp, "{}", leaver.upn);
}

/// No line of `phone` showed a picture of its own camera's shape: the server never sent it its own video.
fn never_itself(phone: &Phone) {
    let lines = phone.video.lines();
    assert!(
        !lines
            .iter()
            .any(|(line, seen)| *line != SHARING_LINE && seen.frames > 0 && shaped(*seen, phone.camera)),
        "{}: its own camera's shape on a line: {lines:?}",
        phone.upn
    );
}

/// Each phone's MRI and camera.
fn faces(phones: &[Phone]) -> Vec<(String, PatternCamera)> {
    phones.iter().map(|phone| (phone.mri.clone(), phone.camera)).collect()
}

/// The first three's cameras on: each sees the other two, each on a line of its own, never itself, and the audio is
/// still mixed. The lines each shows the others on.
async fn three_cameras_seen(phones: &mut [Phone], calls: &[CallId]) -> Vec<Vec<usize>> {
    let faces = faces(phones);
    for (phone, call) in phones.iter().zip(calls) {
        phone.act(CallAction::Camera { call: *call, on: true }).await;
    }
    let mut lines = Vec::new();
    for (index, (phone, call)) in phones.iter_mut().zip(calls).enumerate() {
        let others: Vec<(&str, PatternCamera)> = faces[..3]
            .iter()
            .enumerate()
            .filter(|(other, _)| *other != index)
            .map(|(_, (mri, camera))| (mri.as_str(), *camera))
            .collect();
        let seen = sees(phone, *call, &others).await;
        never_itself(phone);
        let fps = rates(phone, &seen, Duration::from_secs(3)).await;
        eprintln!(
            "{}: lines {seen:?} at {fps:.1?} fps; all lines {:?}",
            phone.upn,
            phone.video.lines()
        );
        assert!(
            fps.iter().all(|rate| *rate >= 5.0),
            "{}: a slow line: {fps:.1?}",
            phone.upn
        );
        lines.push(seen);
    }
    let [ta, tb, tc, td] = TONES;
    let [a, b, c, ..] = phones else { unreachable!() };
    judge(
        &[
            (&*a, &[tb, tc], &[ta, td]),
            (&*b, &[ta, tc], &[tb, td]),
            (&*c, &[ta, tb], &[tc, td]),
        ],
        "three with cameras on",
    )
    .await;
    lines
}

/// a's camera off: b and c see it off, a's line shows nothing more; on again: a's video comes back.
async fn camera_off_and_on(phones: &mut [Phone], calls: &[CallId], lines: &[Vec<usize>]) {
    let [a, b, c, ..] = phones else { unreachable!() };
    let (ma, ca) = (a.mri.clone(), a.camera);
    a.act(CallAction::Camera {
        call: calls[0],
        on: false,
    })
    .await;
    for (phone, call, line) in [(&mut *b, calls[1], lines[1][0]), (&mut *c, calls[2], lines[2][0])] {
        shows(phone, call, "a's camera off", |view| {
            party(view, &ma).is_some_and(|p| !p.video_on)
        })
        .await;
        stays_still(phone, line, "a's camera is off").await;
        let view = latest(phone, call).unwrap();
        eprintln!("{}: a's camera off: {:?}", phone.upn, party(&view, &ma));
    }
    a.act(CallAction::Camera {
        call: calls[0],
        on: true,
    })
    .await;
    for (phone, call) in [(&mut *b, calls[1]), (&mut *c, calls[2])] {
        let shown = phone.video.lines();
        let on = sees_since(phone, call, &[(&*ma, ca)], |line| {
            shown.get(&line).map_or(0, |seen| seen.frames)
        })
        .await;
        eprintln!("{}: a's camera on again, on line {on:?}", phone.upn);
    }
}

/// d joins late from the chat: it sees the three cameras that are on, then its own is seen. d's call, and the line
/// each of the three shows d on.
async fn late_joiner(phones: &mut [Phone], calls: &[CallId], chat: &str) -> (CallId, Vec<usize>) {
    let faces = faces(phones);
    let [a, b, c, d] = phones else { unreachable!() };
    d.sync().await;
    let joined_at = d.mark();
    let joined = d.act(CallAction::Join { chat: chat.to_owned() }).await;
    d.status(joined_at, joined, &CallStatus::Connected).await;
    let three: Vec<(&str, PatternCamera)> = faces[..3].iter().map(|(mri, camera)| (mri.as_str(), *camera)).collect();
    let d_lines = sees(d, joined, &three).await;
    d.act(CallAction::Camera { call: joined, on: true }).await;
    let mut d_at = Vec::new();
    for (phone, call) in [(&mut *a, calls[0]), (&mut *b, calls[1]), (&mut *c, calls[2])] {
        d_at.push(sees(phone, call, &[(&*faces[3].0, faces[3].1)]).await[0]);
        never_itself(phone);
    }
    never_itself(d);
    let fps = rates(d, &d_lines, Duration::from_secs(3)).await;
    eprintln!("{}: the late joiner's lines {d_lines:?} at {fps:.1?} fps", d.upn);
    let [ta, tb, tc, td] = TONES;
    judge(
        &[(&*d, &[ta, tb, tc], &[td]), (&*a, &[tb, tc, td], &[ta])],
        "d joined late with its camera",
    )
    .await;
    (joined, d_at)
}

/// d leaves: its tile goes away at the others, its line shows nothing more, the call and the others' video go on.
async fn leaver(phones: &mut [Phone], calls: &[CallId], joined: CallId, d_at: &[usize]) {
    let faces = faces(phones);
    let [a, b, c, d] = phones else { unreachable!() };
    hang_up(d, joined).await;
    let md = &faces[3].0;
    for ((phone, call), line) in [(&mut *a, calls[0]), (&mut *b, calls[1]), (&mut *c, calls[2])]
        .into_iter()
        .zip(d_at)
    {
        shows(phone, call, "d gone", |view| party(view, md).is_none()).await;
        stays_still(phone, *line, "d left").await;
    }
    let face = |index: usize| (faces[index].0.as_str(), faces[index].1);
    for (phone, call, others) in [
        (&mut *a, calls[0], [face(1), face(2)]),
        (&mut *b, calls[1], [face(0), face(2)]),
    ] {
        let shown = phone.video.lines();
        sees_since(phone, call, &others, |line| {
            shown.get(&line).map_or(0, |seen| seen.frames)
        })
        .await;
    }
}

/// On two nodes the media node's process dies with the cameras on: the call ends for everyone. On one, they hang up.
async fn the_end(group: &mut Group, phones: &mut [Phone], calls: &[CallId]) {
    let [a, b, c, ..] = phones else { unreachable!() };
    let Some(media) = group.media.as_mut() else {
        for (phone, call) in [(&mut *b, calls[1]), (&mut *c, calls[2]), (&mut *a, calls[0])] {
            hang_up(phone, call).await;
        }
        return;
    };
    let marks = [a.mark(), b.mark(), c.mark()];
    let killed = Instant::now();
    media.kill();
    for ((phone, call), mark) in [(&mut *a, calls[0]), (&mut *b, calls[1]), (&mut *c, calls[2])]
        .into_iter()
        .zip(marks)
    {
        let end = phone.ended(mark, call).await;
        eprintln!(
            "{}: the call ended {:?} after the kill: {end:?}",
            phone.upn,
            killed.elapsed()
        );
        assert!(
            matches!(&end, End::Remote(remote) if remote.code == Some(503)),
            "{end:?}"
        );
    }
    group.start_media().await;
}

/// Three with their cameras on see each other; camera off and on; a late joiner sees and is seen; a leaver's tile
/// goes; on two nodes the media node is killed at the end.
async fn three_cameras_then_a_fourth(layout: Layout) {
    let Some(mut group) = Group::start(layout, "three-cameras", 4).await else {
        return;
    };
    let mut phones = group.phones(4).await;
    let chat = {
        let [a, b, c, d] = phones.as_mut_slice() else {
            unreachable!()
        };
        group_chat(a, &[&*b, &*c, &*d], &group.synthetic[..5], "Q4 ✨ «planning» & co").await
    };
    let calls = {
        let [a, b, c, d] = phones.as_mut_slice() else {
            unreachable!()
        };
        let d_rang = d.mark();
        let calls = start_and_answer(a, &mut [&mut *b, &mut *c], &chat).await;
        let ring = d.incoming(d_rang).await;
        d.act(CallAction::Decline { call: ring.call }).await;
        calls
    };
    let lines = three_cameras_seen(&mut phones[..3], &calls).await;
    camera_off_and_on(&mut phones, &calls, &lines).await;
    let (joined, d_at) = late_joiner(&mut phones, &calls, &chat).await;
    leaver(&mut phones, &calls, joined, &d_at).await;
    the_end(&mut group, &mut phones, &calls).await;
    for phone in &mut phones {
        phone.close().await;
    }
    group.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_see_each_others_cameras_off_and_on_a_late_joiner_and_a_leaver_on_one_node() {
    three_cameras_then_a_fourth(Layout::All).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_see_each_others_cameras_a_late_joiner_a_leaver_and_a_killed_media_node_on_api_and_media_nodes() {
    three_cameras_then_a_fourth(Layout::Split).await;
}

/// `sharer` shares in `call` (`on`) or stops.
async fn share(sharer: &Phone, call: CallId, on: bool) {
    sharer.act(CallAction::Share { call, on }).await;
}

/// `viewer` sees `sharer` share and its screen on the sharing line: [`FRAMES`] more frames of a 16:9 picture.
async fn sees_share(viewer: &mut Phone, call: CallId, sharer: &str) {
    let what = format!("{sharer} sharing");
    shows(viewer, call, &what, |view| {
        party(view, sharer).is_some_and(|p| p.sharing)
    })
    .await;
    let since = viewer.video.line(SHARING_LINE).frames;
    until(viewer, &format!("{sharer}'s screen"), |phone| {
        let seen = phone.video.line(SHARING_LINE);
        seen.frames >= since + FRAMES && (shape(seen.width, seen.height) - 16.0 / 9.0).abs() < 0.1
    })
    .await;
    eprintln!(
        "{}: {sharer}'s screen: {:?}",
        viewer.upn,
        viewer.video.line(SHARING_LINE)
    );
}

/// `viewer` sees that `sharer` shares no more, and its sharing line goes still: what was on its way may still come in
/// the second after, then nothing.
async fn sees_share_end(viewer: &mut Phone, call: CallId, sharer: &str) {
    shows(viewer, call, &format!("{sharer}'s share over"), |view| {
        party(view, sharer).is_some_and(|p| !p.sharing)
    })
    .await;
    let ended = viewer.video.line(SHARING_LINE).frames;
    let mark = viewer.mark();
    viewer.quiet(mark, Duration::from_secs(1), |_| false).await;
    let shown = viewer.video.line(SHARING_LINE).frames;
    let mark = viewer.mark();
    viewer.quiet(mark, Duration::from_secs(1), |_| false).await;
    let late = viewer.video.line(SHARING_LINE).frames - shown;
    eprintln!(
        "{}: {sharer}'s share over: frames in the second after it {}, in the next second {late}",
        viewer.upn,
        shown - ended
    );
    assert_eq!(
        late, 0,
        "{}: {sharer}'s screen still comes a second after Stop sharing",
        viewer.upn
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shared_screen_reaches_the_others_until_stop_sharing_with_the_cameras_on() {
    let Some(group) = Group::start(Layout::All, "share", 3).await else {
        return;
    };
    let mut phones = group.phones(3).await;
    let [a, b, c] = phones.as_mut_slice() else {
        unreachable!()
    };
    let chat = group_chat(a, &[&*b, &*c], &group.synthetic[..3], "Demo 🖥 day").await;
    let calls = start_and_answer(a, &mut [&mut *b, &mut *c], &chat).await;
    let (ma, mb) = (a.mri.clone(), b.mri.clone());
    let (ca, cb) = (a.camera, b.camera);
    for (phone, call) in [(&*a, calls[0]), (&*b, calls[1])] {
        phone.act(CallAction::Camera { call, on: true }).await;
    }
    sees(c, calls[2], &[(&*ma, ca), (&*mb, cb)]).await;
    for round in 0..2 {
        // b shares: a and c see its screen, and the cameras go on beside it
        let shared = b.mark();
        share(b, calls[1], true).await;
        b.since(shared, "b's own share on", |event| {
            matches!(event, Event::CallSharing { on: true, .. })
        })
        .await;
        sees_share(a, calls[0], &mb).await;
        sees_share(c, calls[2], &mb).await;
        let c_lines = latest(c, calls[2])
            .map(|view| [&ma, &mb].map(|mri| party(&view, mri).and_then(|p| p.line).unwrap()))
            .unwrap();
        let fps = rates(c, &[c_lines[0], c_lines[1], SHARING_LINE], Duration::from_secs(3)).await;
        eprintln!("round {round}: c shows a's and b's cameras and b's screen at {fps:.1?} fps");
        assert!(
            fps.iter().all(|rate| *rate >= 3.0),
            "a still line beside the share: {fps:.1?}"
        );
        never_itself(b);
        // Stop sharing: it ends for them
        share(b, calls[1], false).await;
        sees_share_end(a, calls[0], &mb).await;
        sees_share_end(c, calls[2], &mb).await;
    }
    for (phone, call) in [(&mut *b, calls[1]), (&mut *c, calls[2]), (&mut *a, calls[0])] {
        hang_up(phone, call).await;
    }
    for phone in &mut phones {
        phone.close().await;
    }
    group.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_sharer_takes_over_the_share_as_in_teams() {
    let Some(group) = Group::start(Layout::All, "second-sharer", 3).await else {
        return;
    };
    let mut phones = group.phones(3).await;
    let [a, b, c] = phones.as_mut_slice() else {
        unreachable!()
    };
    let chat = group_chat(a, &[&*b, &*c], &[], "Two presenters").await;
    let calls = start_and_answer(a, &mut [&mut *b, &mut *c], &chat).await;
    let (mb, mc) = (b.mri.clone(), c.mri.clone());
    share(b, calls[1], true).await;
    sees_share(a, calls[0], &mb).await;
    sees_share(c, calls[2], &mb).await;
    // c shares while b shares: in Teams c's share takes over, b's stops, and everyone sees c's screen
    let b_mark = b.mark();
    share(c, calls[2], true).await;
    sees_share(a, calls[0], &mc).await;
    let b_view = |phone: &Phone| latest(phone, calls[0]).and_then(|view| party(&view, &mb).map(|p| p.sharing));
    let mark = a.mark();
    a.quiet(mark, Duration::from_secs(3), |_| false).await;
    b.quiet(b_mark, Duration::from_millis(100), |_| false).await;
    let b_told = b.log[b_mark..]
        .iter()
        .any(|event| matches!(event, Event::CallSharing { on: false, .. }));
    eprintln!(
        "c shares after b: a's view says b sharing {:?}; b told its share ended: {b_told}; b's events {:?}",
        b_view(&*a),
        b.log[b_mark..].iter().map(brief).collect::<Vec<_>>()
    );
    assert_eq!(b_view(&*a), Some(false), "a's view: b still shares beside c");
    assert!(b_told, "b's client still shares after c took over");
    sees_share(b, calls[1], &mc).await;
    for (phone, call) in [(&mut *b, calls[1]), (&mut *c, calls[2]), (&mut *a, calls[0])] {
        hang_up(phone, call).await;
    }
    for phone in &mut phones {
        phone.close().await;
    }
    group.stop().await;
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

/// `count` people in a call, each camera on; each sees every other on a line (all of them up to nine). Their phones
/// and calls.
async fn all_cameras(group: &Group, count: usize, topic: &str) -> (Vec<Phone>, Vec<CallId>, String) {
    let mut phones = group.phones(count).await;
    let (first, rest) = phones.split_first_mut().unwrap();
    let others: Vec<&Phone> = rest.iter().collect();
    let chat = group_chat(first, &others, &group.synthetic, topic).await;
    let mut answerers: Vec<&mut Phone> = rest.iter_mut().collect();
    let calls = start_and_answer(first, &mut answerers, &chat).await;
    for (phone, call) in phones.iter().zip(&calls) {
        phone.act(CallAction::Camera { call: *call, on: true }).await;
    }
    (phones, calls, chat)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn six_cameras_frame_rates_and_the_media_nodes_cpu() {
    let Some(group) = Group::start(Layout::Split, "six-cameras", 6).await else {
        return;
    };
    let (mut phones, calls, _) = all_cameras(&group, 6, "All hands 🙌 (six)").await;
    let everyone: Vec<(String, PatternCamera)> = phones.iter().map(|phone| (phone.mri.clone(), phone.camera)).collect();
    let mut lines = Vec::new();
    for (phone, call) in phones.iter_mut().zip(&calls) {
        let others: Vec<(&str, PatternCamera)> = everyone
            .iter()
            .filter(|(mri, _)| *mri != phone.mri)
            .map(|(mri, camera)| (mri.as_str(), *camera))
            .collect();
        lines.push(sees(phone, *call, &others).await);
        never_itself(phone);
    }
    let pid = group.media_pid();
    let (cpu, started) = (cpu_time(pid), Instant::now());
    let mut report = String::new();
    let mut slowest: f64 = f64::MAX;
    for (phone, lines) in phones.iter_mut().zip(&lines) {
        let fps = rates(phone, lines, Duration::from_secs(3)).await;
        slowest = fps.iter().copied().fold(slowest, f64::min);
        let _ = writeln!(report, "{}: lines {lines:?} at {fps:.1?} fps", phone.upn);
    }
    let took = started.elapsed();
    let share = cpu_time(pid).saturating_sub(cpu).as_secs_f64() / took.as_secs_f64() * 100.0;
    eprintln!(
        "{report}six cameras: the media node's CPU {share:.1}% of a core over {took:?}; the slowest line {slowest:.1} fps"
    );
    assert!(slowest >= 5.0, "a line below 5 fps:\n{report}");
    for (phone, call) in phones.iter_mut().zip(&calls).rev() {
        hang_up(phone, *call).await;
    }
    for phone in &mut phones {
        phone.close().await;
    }
    group.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn eleven_cameras_on_nine_lines_and_a_freed_line_goes_to_the_one_without() {
    let Some(group) = Group::start(Layout::All, "eleven-cameras", 11).await else {
        return;
    };
    let (mut phones, calls, _) = all_cameras(&group, 11, "Big room 🏟").await;
    let cameras: Vec<(String, PatternCamera)> = phones.iter().map(|phone| (phone.mri.clone(), phone.camera)).collect();
    let watcher = 0;
    // a sees nine of the ten others on its nine lines, each the one its line asks for; one has no line
    let (on_lines, waiting) = {
        let phone = &mut phones[watcher];
        let call = calls[watcher];
        shows(phone, call, "ten cameras on, nine on lines", |view| {
            view.participants.len() == 10
                && view.participants.iter().all(|p| p.video_on)
                && view.participants.iter().filter(|p| p.line.is_some()).count() == 9
        })
        .await;
        let view = latest(phone, call).unwrap();
        let waiting = view.participants.iter().find(|p| p.line.is_none()).unwrap().mri.clone();
        let on_lines: Vec<(String, usize)> = view
            .participants
            .iter()
            .filter_map(|p| Some((p.mri.clone(), p.line?)))
            .collect();
        for (mri, line) in &on_lines {
            let camera = cameras.iter().find(|(of, _)| of == mri).unwrap().1;
            until(phone, &format!("{mri} on line {line}"), |phone| {
                let seen = phone.video.line(*line);
                seen.frames >= FRAMES && shaped(seen, camera)
            })
            .await;
        }
        never_itself(phone);
        let lines: Vec<usize> = on_lines.iter().map(|(_, line)| *line).collect();
        let fps = rates(phone, &lines, Duration::from_secs(3)).await;
        eprintln!(
            "{}: nine lines {on_lines:?} at {fps:.1?} fps; {waiting} has no line",
            phone.upn
        );
        (on_lines, waiting)
    };
    // one who has a line at a leaves: the line goes to the one that had none, and its video comes on it, not frozen
    let (leaver_mri, freed) = on_lines[0].clone();
    let leaver = phones.iter().position(|phone| phone.mri == leaver_mri).unwrap();
    hang_up(&mut phones[leaver], calls[leaver]).await;
    let camera = cameras.iter().find(|(of, _)| *of == waiting).unwrap().1;
    let phone = &mut phones[watcher];
    let before = phone.video.line(freed).frames;
    shows(phone, calls[watcher], "the waiting one on the freed line", |view| {
        party(view, &leaver_mri).is_none() && party(view, &waiting).is_some_and(|p| p.line == Some(freed))
    })
    .await;
    until(
        phone,
        &format!("{waiting}'s video on the freed line {freed}"),
        |phone| {
            let seen = phone.video.line(freed);
            seen.frames >= before + FRAMES && shaped(seen, camera)
        },
    )
    .await;
    let fps = rates(phone, &[freed], Duration::from_secs(3)).await;
    eprintln!("{}: {waiting} on the freed line {freed} at {fps:.1?} fps", phone.upn);
    assert!(fps[0] >= 5.0, "the freed line is slow: {fps:.1?}");
    for (index, (phone, call)) in phones.iter_mut().zip(&calls).enumerate().rev() {
        if index != leaver {
            hang_up(phone, *call).await;
        }
    }
    for phone in &mut phones {
        phone.close().await;
    }
    group.stop().await;
}
