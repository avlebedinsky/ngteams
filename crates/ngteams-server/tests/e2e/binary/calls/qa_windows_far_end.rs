//! QA 422: the far end of a Windows client's calls, for a person who listens to what the client plays. The real
//! server binary on SQLite and the run's coturn, reached over TCP (`NGTS_TEST_TURN_TRANSPORT=tcp`) so that a client
//! in a VM can reach both through an SSH tunnel. Three synthetic people answer every call that rings them:
//!
//! - Bert sends a 1 kHz tone at half scale;
//! - Cora sends silence (in a group call, what the client hears is Bert's tone alone);
//! - Dora sends a speech-like voice: a 125 Hz voice with its harmonics shaped as vowels, in syllables and pauses
//!   ([`speech`]), the same each call.
//!
//! The client's account is `w@own.test` (Wendy). The rig writes to `NGTS_QA_FAR_DIR`: `origin` (the server's public
//! origin), `chats` (the chat ids: `bert`, `dora`, `group` of Bert, Cora and Wendy) and `events.log` (each call event
//! of each person, with the Unix time in ms). It takes commands from the lines added to `commands` there:
//! `hold <who>`, `resume <who>`, `hangup <who>`, `call <who>` (that person calls Wendy 1:1) and `quit`.
//!
//! Run it by hand, for at most `NGTS_QA_FAR_SECONDS` (default 3 h): `--ignored --nocapture
//! binary::calls::qa_windows_far_end`.

use std::collections::BTreeMap;
use std::f64::consts::TAU;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use teams_core::engine::Event;
use teams_core::{CallAction, CallId, CallStatus};
use teams_media::devices::SAMPLE_RATE;
use teams_media::devices::wav::WavWriter;
use teams_media::tone::Tone;

use super::group::{Group, Layout, group_chat};
use super::support::{Phone, brief};
use crate::support::PASSWORD;

const DIR_ENV: &str = "NGTS_QA_FAR_DIR";
const SECONDS_ENV: &str = "NGTS_QA_FAR_SECONDS";
const BERT: Tone = Tone {
    hz: 1000.0,
    amplitude: 0.5,
};
const SILENT: Tone = Tone {
    hz: 1000.0,
    amplitude: 0.0,
};
/// The speech-like voice's pitch: 40 ms holds five whole periods, so each harmonic is a whole DFT bin there.
pub const VOICE_HZ: f64 = 125.0;
/// The voice's harmonics: up to 3.75 kHz.
pub const HARMONICS: u32 = 30;
/// How long the speech-like voice lasts.
const SPEECH_SECONDS: u32 = 120;

/// The vowels the voice goes through: their first three formants, in Hz.
const VOWELS: [[f64; 3]; 5] = [
    [730.0, 1090.0, 2440.0],
    [270.0, 2290.0, 3010.0],
    [530.0, 1840.0, 2480.0],
    [570.0, 840.0, 2410.0],
    [300.0, 870.0, 2240.0],
];

/// The speech-like voice at sample `n`: a syllable each 250 ms (190 ms of voice, its vowel the next of [`VOWELS`]),
/// and a pause of 500 ms after each eight syllables.
#[must_use]
pub fn speech(n: u64) -> f64 {
    let rate = f64::from(SAMPLE_RATE);
    #[expect(clippy::cast_precision_loss, reason = "a sample index of a two-minute file")]
    let t = n as f64 / rate;
    let phrase = t % 2.5;
    if phrase >= 2.0 {
        return 0.0;
    }
    let in_syllable = phrase % 0.25;
    if in_syllable >= 0.19 {
        return 0.0;
    }
    let envelope = (std::f64::consts::PI * in_syllable / 0.19).sin().powi(2);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a syllable count of a two-minute file"
    )]
    let vowel = VOWELS[((t / 0.25) as usize) % VOWELS.len()];
    let mut sum = 0.0;
    for k in 1..=HARMONICS {
        let hz = VOICE_HZ * f64::from(k);
        let gain: f64 = vowel
            .iter()
            .map(|formant| (-((hz - formant) / 120.0).powi(2)).exp())
            .sum::<f64>()
            + 0.05 / f64::from(k);
        sum += gain * (TAU * hz * t).sin();
    }
    0.12 * envelope * sum
}

fn write_speech(path: &Path) {
    let mut writer = WavWriter::create(path).unwrap();
    #[expect(clippy::cast_possible_truncation, reason = "a sample of the voice is within ±1")]
    let samples: Vec<f32> = (0..u64::from(SPEECH_SECONDS * SAMPLE_RATE))
        .map(|n| speech(n) as f32)
        .collect();
    assert!(samples.iter().all(|s| s.abs() < 0.95), "the voice clips");
    writer.write(&samples).unwrap();
    writer.finish().unwrap();
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

struct Rig {
    dir: PathBuf,
    log: std::fs::File,
}

impl Rig {
    fn note(&mut self, who: &str, what: &str) {
        let line = format!("{} {who} {what}", now_ms());
        eprintln!("{line}");
        writeln!(self.log, "{line}").unwrap();
    }

    /// The command lines added to `commands` since `read` of its bytes.
    fn commands(&self, read: &mut usize) -> Vec<String> {
        let text = std::fs::read_to_string(self.dir.join("commands")).unwrap_or_default();
        let Some(added) = text.get(*read..) else {
            return Vec::new();
        };
        let complete = added.rfind('\n').map_or(0, |end| end + 1);
        *read += complete;
        added[..complete].lines().map(str::to_owned).collect()
    }
}

/// One synthetic person: the client, and its call up.
struct Person {
    name: &'static str,
    phone: Phone,
    call: Option<CallId>,
    chat: String,
}

/// Takes what came to `person` and answers what rings.
async fn serve(rig: &mut Rig, person: &mut Person) {
    let mark = person.phone.mark();
    while person.phone.pump(Duration::ZERO).await {}
    let mut answers = Vec::new();
    for event in &person.phone.log[mark..] {
        match event {
            Event::IncomingCall(incoming) => {
                rig.note(person.name, &format!("{} rings; answers", brief(event)));
                answers.push(incoming.call);
            }
            Event::Call { call, status } => {
                rig.note(person.name, &brief(event));
                match status {
                    CallStatus::Connected => person.call = Some(*call),
                    CallStatus::Ended(_) if person.call == Some(*call) => person.call = None,
                    _ => (),
                }
            }
            Event::CallHeld { .. } | Event::CallHeldByFarEnd { .. } | Event::Failed { .. } => {
                rig.note(person.name, &brief(event));
            }
            _ => (),
        }
    }
    for call in answers {
        let answer = person.phone.press(CallAction::Answer { call }).await;
        rig.note(person.name, &format!("answer: {answer:?}"));
    }
}

async fn command(rig: &mut Rig, people: &mut [Person], line: &str) -> bool {
    let mut words = line.split_whitespace();
    let (Some(verb), who) = (words.next(), words.next().unwrap_or_default()) else {
        return true;
    };
    if verb == "quit" {
        rig.note("rig", "quit");
        return false;
    }
    let Some(person) = people.iter_mut().find(|person| person.name == who) else {
        rig.note("rig", &format!("unknown command {line:?}"));
        return true;
    };
    let action = match (verb, person.call) {
        ("hold", Some(call)) => CallAction::Hold { call, held: true },
        ("resume", Some(call)) => CallAction::Hold { call, held: false },
        ("hangup", Some(call)) => CallAction::HangUp { call },
        ("call", _) => CallAction::Call {
            chat: person.chat.clone(),
        },
        _ => {
            rig.note(person.name, &format!("{line:?}: no call up"));
            return true;
        }
    };
    let pressed = person.phone.press(action).await;
    rig.note(person.name, &format!("{line}: {pressed:?}"));
    true
}

/// Bert, Cora and Dora, online, with their chats with Wendy written to `dir`.
async fn far_end(group: &Group, dir: &Path) -> Vec<Person> {
    let speech_wav = dir.join("speech.wav");
    write_speech(&speech_wav);
    let wendy = group.api.sign_in("w@own.test", PASSWORD).await;
    let skype = wendy.skypetoken().await.unwrap();
    let wendy_mri = format!("8:{}", skype.skype_id.as_deref().unwrap());
    let bert = group.phone(0, BERT).await;
    let cora = group.phone(1, SILENT).await;
    let (upn, name) = &group.people[2];
    let mut dora = Phone::signed_in(&group.api, &group.origin, (upn, name), SILENT).await;
    dora.speak(speech_wav);
    dora.start();
    dora.online(0).await;
    let bert_chat = bert.chat_with(&wendy_mri).await;
    let dora_chat = dora.chat_with(&wendy_mri).await;
    let group_chat_id = group_chat(&bert, &[&cora], std::slice::from_ref(&wendy_mri), "QA audio group").await;
    let chats: BTreeMap<&str, &str> = [
        ("bert", bert_chat.as_str()),
        ("dora", dora_chat.as_str()),
        ("group", group_chat_id.as_str()),
    ]
    .into();
    let mut text = String::new();
    for (key, chat) in &chats {
        writeln!(text, "{key} {chat}").unwrap();
    }
    std::fs::write(dir.join("chats"), text).unwrap();
    std::fs::write(dir.join("origin"), &group.origin).unwrap();
    vec![
        Person {
            name: "bert",
            phone: bert,
            call: None,
            chat: bert_chat,
        },
        Person {
            name: "cora",
            phone: cora,
            call: None,
            chat: group_chat_id,
        },
        Person {
            name: "dora",
            phone: dora,
            call: None,
            chat: dora_chat,
        },
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "QA 422: a far end for a Windows client's calls, run by hand"]
async fn qa_windows_far_end() {
    let Some(dir) = std::env::var_os(DIR_ENV).map(PathBuf::from) else {
        eprintln!("skipped: {DIR_ENV} names no directory");
        return;
    };
    let seconds = std::env::var(SECONDS_ENV)
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(3 * 3600);
    let people = [
        ("b@own.test", "Bert"),
        ("c@own.test", "Cora"),
        ("d@own.test", "Dora"),
        ("w@own.test", "Wendy"),
    ];
    let Some(group) = Group::start(Layout::All, "windows far end", &people, false).await else {
        return;
    };
    let mut people = far_end(&group, &dir).await;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("events.log"))
        .unwrap();
    let mut rig = Rig { dir, log };
    rig.note("rig", &format!("ready at {}", group.origin));
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut read = rig.commands(&mut 0).iter().map(|line| line.len() + 1).sum();
    'run: while Instant::now() < deadline {
        for person in &mut people {
            serve(&mut rig, person).await;
        }
        for line in rig.commands(&mut read) {
            if !command(&mut rig, &mut people, &line).await {
                break 'run;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    for person in &mut people {
        person.phone.close().await;
    }
    group.stop().await;
}

#[test]
fn the_voice_has_its_harmonics_and_its_pauses() {
    let rate = u64::from(SAMPLE_RATE);
    let voiced: Vec<f64> = (rate / 10..rate / 10 + 1920).map(speech).collect();
    let peak = voiced.iter().fold(0.0_f64, |top, s| top.max(s.abs()));
    assert!((0.1..0.95).contains(&peak), "{peak}");
    assert!((2 * rate + rate / 10..2 * rate + rate / 5).all(|n| speech(n) == 0.0));
}
