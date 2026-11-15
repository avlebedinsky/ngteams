//! QA scenes of a colleague's profile card on an own server's account (task 299, round 1), on the real
//! `ngteams-server` binary (`NGTEAMS_QA_SERVER_BIN`): each one fails when the server is the one in this process.
//!
//! The parent seeds synthetic people on the server as an operator does (`user add`, `profile set`): Bert reports to
//! Cora, who reports to Eve (no manager); Finn and Zoë (an odd name) report to Bert; Olga (Cora's report) has her
//! automatic replies on; Gil has no organization data at all; Dan and a person with a very long name share Bert's
//! group chat with the account. Before the app starts, Bert, Olga and Gil sign in on the server as any client of its
//! HTTP API does (password grant, authz) and write: Bert's 1:1 chat with the account and a group chat of the account,
//! Bert, Dan, Olga, Gil and the long name, where each of the three posts. Then the app runs a and b (Microsoft 365, on the
//! mock) beside the own server's account, which is shown.
//!
//! The user's way to a colleague's card, by pointer: Bert's face in the 1:1 chat's header (the full card: Overview,
//! Profile, Organization), a rest on his face beside his message in the group chat (the compact card), a click on
//! Olga's face (out of office) and on Gil's (Organization says the directory names nobody), and the title bar's
//! search for Bert. What each card read is said by the child (`card …`) and checked against the seeded data; every
//! state is screenshot under `target/ui-shots/ngteams-qa-card-*`. At 1280 px in Teams Light, a's card of c (the
//! mock's Loki) is opened in the same window too: the own account's Loki requests went to its server alone and a's to
//! the mock alone, by the app's request log and the mock's own record (the server's log has no request lines: it
//! drops its dependencies' debug events, `tower_http`'s among them), and the mock never got the own account's token.

use std::fs;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{App, AsyncApp, Entity, WindowHandle};
use ngteams::accounts::{self, Runner, Started};
use ngteams::accounts_file::OwnServer;
use ngteams::setup::Setup;
use teams_auth::secrets::Secret;
use teams_mock::{AC_CHAT, ChatServiceMock, LAB_A, LAB_C, LokiCard};
use teams_ui::chat_view::Item;
use teams_ui::profile_card::{CardTab, Form, Loaded};
use teams_ui::shell::Shell;
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use ui_sandbox::input::{VirtualInput, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};

use super::{
    OWN_CA_ENV, OWN_NAME, OWN_ORIGIN_ENV, OWN_UPN, ROOT_ENV, TraceReader, WAIT, about, accounts_shown, ctl, ensure,
    own, say, shot, start_with,
};

/// The file in the sandbox dir the parent writes the seeded people's ids to (`<upn> <object id>` a line).
const PEOPLE_FILE: &str = "card-people";
/// The files the parent writes to have the child say the faces of the chat shown, the card open, and where the Loki
/// requests went.
const FACES_ASK: &str = "say-card-faces";
const CARD_ASK: &str = "say-card";
const LOKI_ASK: &str = "say-card-loki";
/// The child's list of the app's Loki requests, `<place> <method> <url>` a line.
const LOKI_FILE: &str = "card-loki.txt";

/// A seeded person: user name, display name, and `profile set` arguments.
struct Person {
    upn: &'static str,
    name: &'static str,
    profile: &'static [&'static str],
}

const BERT: &str = "bert.qa@lab.example.test";
const CORA: &str = "cora.qa@lab.example.test";
const EVE: &str = "eve.qa@lab.example.test";
const FINN: &str = "finn.qa@lab.example.test";
const ZOE: &str = "zoe.qa@lab.example.test";
const OLGA: &str = "olga.qa@lab.example.test";
const GIL: &str = "gil.qa@lab.example.test";
const DAN: &str = "dan.qa@lab.example.test";
const LONG: &str = "maximiliana.qa@lab.example.test";

const BERT_NAME: &str = "Bert Sample";
const CORA_NAME: &str = "Cora Chief";
const EVE_NAME: &str = "Eve Director";
const FINN_NAME: &str = "Finn Junior";
const ZOE_NAME: &str = "Zoë Ångström-Øvergaard";
const OLGA_NAME: &str = "Olga Away";
const GIL_NAME: &str = "Gil Loner";
const DAN_NAME: &str = "Dan Fourth";
const LONG_NAME: &str = "Maximiliana Theodora Wilhelmina von Hohenzollern-Sigmaringen-Ostrowski-Papadopoulou the Third";

/// The people, managers before their reports (`profile set --manager` names an account that exists).
const PEOPLE: [Person; 9] = [
    Person {
        upn: EVE,
        name: EVE_NAME,
        profile: &["--job-title", "Director of Engineering", "--department", "Engineering"],
    },
    Person {
        upn: CORA,
        name: CORA_NAME,
        profile: &["--job-title", "Chief Architect", "--manager", EVE],
    },
    Person {
        upn: BERT,
        name: BERT_NAME,
        profile: &[
            "--given-name",
            "Bert",
            "--surname",
            "Sample",
            "--job-title",
            "Engineer",
            "--department",
            "Platform",
            "--office",
            "Room 4",
            "--company",
            "Synthetic Labs",
            "--mobile-phone",
            "+1 555 0100",
            "--business-phone",
            "+1 555 0101",
            "--manager",
            CORA,
        ],
    },
    Person {
        upn: FINN,
        name: FINN_NAME,
        profile: &["--job-title", "Junior Engineer", "--manager", BERT],
    },
    Person {
        upn: ZOE,
        name: ZOE_NAME,
        profile: &["--job-title", "Ingénieure", "--manager", BERT],
    },
    Person {
        upn: OLGA,
        name: OLGA_NAME,
        profile: &["--job-title", "Designer", "--manager", CORA, "--out-of-office", "true"],
    },
    Person {
        upn: GIL,
        name: GIL_NAME,
        profile: &[],
    },
    Person {
        upn: DAN,
        name: DAN_NAME,
        profile: &["--job-title", "Tester"],
    },
    Person {
        upn: LONG,
        name: LONG_NAME,
        profile: &[
            "--job-title",
            "Principal Engineer of Extremely Long Titles and Overflowing Things",
        ],
    },
];

/// What Bert, Olga and Gil write before the app starts.
const BERT_SAYS_ONE: &str = "Hi, the platform review is on Thursday";
const BERT_SAYS: &str = "Launch checklist is ready for a look";
const OLGA_SAYS: &str = "I'm away this week, Cora covers for me";
const GIL_SAYS: &str = "Count me in for the launch";
const GROUP_TOPIC: &str = "Launch QA";

// --- the parent ---

/// A sandbox of `width` by 800 px, the window filling it.
fn sandbox_at(width: u32) -> SandboxResult<Option<Sandbox>> {
    let options = SandboxOptions {
        width,
        height: 800,
        niri_extra: NIRI_FULL_WINDOW,
        fill_output: true,
        ..SandboxOptions::default()
    };
    sandbox_or_skip("ngteams own-server profile card QA", &options)
}

/// Adds the account and [`PEOPLE`] to `server` and sets their profiles, as the operator does; writes their ids to
/// [`PEOPLE_FILE`].
fn seed(sandbox: &Sandbox, server: &own::Own) -> SandboxResult {
    use std::fmt::Write as _;
    let mut ids = String::new();
    let reader = server.add_user(OWN_UPN, OWN_NAME)?;
    let _ = writeln!(ids, "{OWN_UPN} {reader}");
    for person in &PEOPLE {
        let id = server.add_user(person.upn, person.name)?;
        let _ = writeln!(ids, "{} {id}", person.upn);
        if !person.profile.is_empty() {
            let mut args = vec!["profile", "set", person.upn];
            args.extend_from_slice(person.profile);
            server.cli(&args, None)?;
        }
    }
    let shown = server.cli(&["profile", "show", BERT], None)?;
    super::shots_dir().and_then(|dir| fs::write(dir.join("ngteams-qa-card-bert-profile.txt"), &shown))?;
    fs::write(sandbox.path(PEOPLE_FILE), ids)?;
    Ok(())
}

/// A colleague writing on the server as any client of it would, over its HTTP API: signed in with the password
/// grant, the chat service's base from authz, then the chat service with an IC3 token (the engine isn't used here: see the report).
struct Writer {
    ca_file: std::path::PathBuf,
    /// An access token for the chat service (IC3).
    bearer: String,
    chats: String,
}

impl Writer {
    fn signed_in(server: &own::Own, upn: &str) -> SandboxResult<Self> {
        let spaces = grant(server, upn, "https://api.spaces.skype.com")?;
        let authz = curl_json(
            server,
            &["-X", "POST", &format!("{}/api/authsvc/v1.0/authz", server.origin())],
            &["-H", &format!("authorization: Bearer {spaces}"), "--data", ""],
        )?;
        let chats = authz["regionGtms"]["chatServiceAfd"]
            .as_str()
            .ok_or_else(|| format!("authz names no chat service: {authz}"))?
            .trim_end_matches('/')
            .to_owned();
        Ok(Self {
            ca_file: server.ca_file().to_owned(),
            bearer: grant(server, upn, "https://ic3.teams.office.com")?,
            chats,
        })
    }

    fn call(&self, url: &str, body: &serde_json::Value) -> SandboxResult<serde_json::Value> {
        let output = std::process::Command::new("curl")
            .args(["-sS", "--fail-with-body", "--cacert"])
            .arg(&self.ca_file)
            .args(["-H", "content-type: application/json"])
            .args(["-H", &format!("authorization: Bearer {}", self.bearer)])
            .args(["-X", "POST", url, "--data", &body.to_string()])
            .output()?;
        let said = String::from_utf8_lossy(&output.stdout).into_owned();
        ensure(output.status.success(), || {
            format!("POST {url}: {said} {}", String::from_utf8_lossy(&output.stderr))
        })?;
        Ok(serde_json::from_str(&said).unwrap_or(serde_json::Value::Null))
    }

    /// A chat of the writer (`me`) and `others`: their 1:1 chat for one, else a group chat named `topic`.
    fn chat(&self, me: &str, others: &[String], topic: Option<&str>) -> SandboxResult<String> {
        let mut members = vec![serde_json::json!({"id": me, "role": "Admin"})];
        members.extend(others.iter().map(|mri| serde_json::json!({"id": mri, "role": "Admin"})));
        let one = others.len() == 1;
        let mut properties = serde_json::json!({"threadType": "chat", "fixedRoster": one, "uniquerosterthread": one});
        if let Some(topic) = topic {
            properties["topic"] = serde_json::json!(topic);
        }
        let made = self.call(
            &format!("{}/v1/threads", self.chats),
            &serde_json::json!({"members": members, "properties": properties}),
        )?;
        Ok(made["threadResource"]["id"]
            .as_str()
            .ok_or_else(|| format!("the chat made: {made}"))?
            .to_owned())
    }

    fn post(&self, chat: &str, text: &str) -> SandboxResult {
        self.call(
            &format!("{}/v1/users/ME/conversations/{chat}/messages", self.chats),
            &serde_json::json!({"messagetype": "RichText/Html", "contenttype": "text", "content": format!("<p>{text}</p>")}),
        )?;
        Ok(())
    }
}

/// `upn`'s access token for `resource`, from the password grant.
fn grant(server: &own::Own, upn: &str, resource: &str) -> SandboxResult<String> {
    let client_id = teams_auth::token::EntraConfig::teams_native().client_id().to_owned();
    let token = curl_json(
        server,
        &["-X", "POST", &format!("{}/oauth2/v2.0/token", server.origin())],
        &[
            "--data-urlencode",
            "grant_type=password",
            "--data-urlencode",
            &format!("username={upn}"),
            "--data-urlencode",
            &format!("password={}", own::PASSWORD),
            "--data-urlencode",
            &format!("client_id={client_id}"),
            "--data-urlencode",
            &format!("scope={resource}/.default openid profile offline_access"),
        ],
    )?;
    Ok(token["access_token"]
        .as_str()
        .ok_or_else(|| format!("no access token for {resource}: {token}"))?
        .to_owned())
}

/// `curl` to the server with its CA: the JSON it answered.
fn curl_json(server: &own::Own, request: &[&str], rest: &[&str]) -> SandboxResult<serde_json::Value> {
    let output = std::process::Command::new("curl")
        .args(["-sS", "--cacert"])
        .arg(server.ca_file())
        .args(request)
        .args(rest)
        .output()?;
    let said = String::from_utf8_lossy(&output.stdout);
    Ok(serde_json::from_str(&said).map_err(|error| format!("{request:?}: {error}: {said}"))?)
}

/// Bert writes his 1:1 chat with the account and the group chat; Olga and Gil post there too: `(1:1, group)`.
fn write_chats(server: &own::Own, people: &[(String, String)]) -> SandboxResult<(String, String)> {
    let bert = Writer::signed_in(server, BERT)?;
    let me = mri_of(people, BERT)?;
    let one = bert.chat(&me, &[mri_of(people, OWN_UPN)?], None)?;
    let others = [OWN_UPN, DAN, OLGA, GIL, LONG]
        .iter()
        .map(|upn| mri_of(people, upn))
        .collect::<SandboxResult<Vec<_>>>()?;
    let group = bert.chat(&me, &others, Some(GROUP_TOPIC))?;
    bert.post(&one, BERT_SAYS_ONE)?;
    bert.post(&group, BERT_SAYS)?;
    Writer::signed_in(server, OLGA)?.post(&group, OLGA_SAYS)?;
    Writer::signed_in(server, GIL)?.post(&group, GIL_SAYS)?;
    Ok((one, group))
}

/// A coordinate the parent clicks: `NGTEAMS_CARD_QA_<NAME>=x,y` when set, else `default`.
fn at(name: &str, default: (u32, u32)) -> (u32, u32) {
    std::env::var(format!("NGTEAMS_CARD_QA_{name}"))
        .ok()
        .and_then(|value| {
            let (x, y) = value.split_once(',')?;
            Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
        })
        .unwrap_or(default)
}

/// Where the 1:1 chat's header face is, and the full card's tabs, at `width`.
struct Places {
    header_face: (u32, u32),
    tab_profile: (u32, u32),
    tab_organization: (u32, u32),
    /// A point on the chat's empty right side, off every face and card.
    away: (u32, u32),
    /// At a narrow width, the title bar's "Hide the list": the list pane leaves the conversation room, as a user of a
    /// small window does first.
    list_toggle: Option<(u32, u32)>,
    /// The mock account's 1:1 chat's header face (its list pane is wider: its rows have no times' column to fit).
    microsoft_header: (u32, u32),
}

impl Places {
    fn at(width: u32) -> Self {
        let (header_face, tab_profile, tab_organization) = if width <= 480 {
            ((102, 71), (149, 198), (233, 198))
        } else {
            ((422, 71), (353, 198), (437, 198))
        };
        Self {
            header_face: at(&format!("HEADER_{width}"), header_face),
            tab_profile: at(&format!("TAB_PROFILE_{width}"), tab_profile),
            tab_organization: at(&format!("TAB_ORG_{width}"), tab_organization),
            away: at(&format!("AWAY_{width}"), (width - 40, 120)),
            microsoft_header: at(&format!("MS_HEADER_{width}"), (479, 71)),
            list_toggle: (width <= 480).then(|| at(&format!("LIST_TOGGLE_{width}"), (90, 23))),
        }
    }
}

/// One run of the scene: the shots' names end in `suffix`.
struct Run<'a> {
    sandbox: &'a Sandbox,
    running: Running,
    trace: TraceReader,
    input: VirtualInput,
    suffix: String,
    places: Places,
}

impl Run<'_> {
    fn shot(&self, name: &str) -> SandboxResult {
        shot(self.sandbox, &format!("ngteams-qa-card-{name}{}.png", self.suffix))
    }

    /// The child's `<name> …` line answering `ask`.
    fn ask(&self, ask: &str, name: &str) -> SandboxResult<String> {
        thread_pause();
        fs::write(self.sandbox.path(ask), "")?;
        self.running
            .line_where(WAIT, |line| line.starts_with(&format!("{name} ")))
            .map_err(|error| format!("the child didn't say {name}: {error:?}").into())
    }

    /// The card open, once it read its person (or `card none`/`loaded=failed`), within [`WAIT`].
    fn card_read(&self) -> SandboxResult<Card> {
        let start = Instant::now();
        loop {
            let line = self.ask(CARD_ASK, "card")?;
            let card = Card::parse(&line);
            if card.get("loaded") != Some("loading") || start.elapsed() > WAIT {
                say(&line);
                return Ok(card);
            }
        }
    }

    /// Waits for the next `profile-card opened` of `form`.
    fn opened(&mut self, form: &str) -> SandboxResult {
        self.trace
            .next_where(&format!("profile-card opened {form}"), |line| {
                line["view"] == "profile-card" && line["change"] == "opened" && kind_is(line, form)
            })
            .map_err(|error| self.running.fail(&error.to_string()))
    }

    fn tab(&mut self, at: (u32, u32), key: &str) -> SandboxResult {
        self.input.click(at.0, at.1, keys::BTN_LEFT)?;
        self.trace
            .next_where(&format!("profile-card tab {key}"), |line| {
                line["view"] == "profile-card" && line["change"] == "tab" && kind_is(line, key)
            })
            .map_err(|error| self.running.fail(&error.to_string()))
    }

    fn escape(&mut self) -> SandboxResult {
        self.input.key(keys::ESC)?;
        thread_pause();
        let card = self.ask(CARD_ASK, "card")?;
        ensure(card == "card none", || format!("Escape left the card open: {card}"))
    }

    /// Opens the chat `chat` of the account shown.
    fn open_chat(&mut self, chat: &str) -> SandboxResult {
        ctl(self.sandbox, &["open", "chat", chat])?;
        self.trace
            .next_where("chat opened", |line| {
                line["view"] == "chat" && line["change"] == "opened" && line["conversation"] == chat
            })
            .map_err(|error| self.running.fail(&error.to_string()))?;
        thread_pause();
        Ok(())
    }

    /// The centre of the face beside the last message of `mri` in the chat shown.
    fn face_of(&self, mri: &str) -> SandboxResult<(u32, u32)> {
        let start = Instant::now();
        loop {
            let faces = self.ask(FACES_ASK, "faces")?;
            let found = faces.split_whitespace().skip(1).find_map(|face| {
                let (who, at) = face.split_once('=')?;
                if who != mri {
                    return None;
                }
                let (x, y) = at.split_once(',')?;
                Some((x.parse().ok()?, y.parse().ok()?))
            });
            if let Some(found) = found {
                return Ok(found);
            }
            ensure(start.elapsed() < WAIT, || format!("no face of {mri}: {faces}"))?;
        }
    }
}

/// Whether a trace line's kind (a `kinds` list of one, or a `kind`) is `kind`.
fn kind_is(line: &serde_json::Value, kind: &str) -> bool {
    line["kinds"] == serde_json::json!([kind]) || line["kind"] == kind
}

fn thread_pause() {
    std::thread::sleep(Duration::from_millis(600));
}

/// A card as the child said it: `card key=value|key=value…`.
struct Card(Vec<(String, String)>);

impl Card {
    fn parse(line: &str) -> Self {
        let body = line.strip_prefix("card ").unwrap_or_default();
        Self(
            body.split('|')
                .filter_map(|field| {
                    let (key, value) = field.split_once('=')?;
                    Some((key.to_owned(), value.to_owned()))
                })
                .collect(),
        )
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    fn holds(&self, key: &str, wanted: &str) -> SandboxResult {
        ensure(self.get(key) == Some(wanted), || {
            format!(
                "the card's {key} is {:?}, not {wanted:?}; card {:?}",
                self.get(key),
                self.0
            )
        })
    }
}

/// The ids of the seeded people, by user name.
fn ids(sandbox: &Sandbox) -> SandboxResult<Vec<(String, String)>> {
    Ok(fs::read_to_string(sandbox.path(PEOPLE_FILE))?
        .lines()
        .filter_map(|line| {
            let (upn, id) = line.split_once(' ')?;
            Some((upn.to_owned(), id.to_owned()))
        })
        .collect())
}

fn mri_of(people: &[(String, String)], upn: &str) -> SandboxResult<String> {
    people
        .iter()
        .find(|(held, _)| held == upn)
        .map(|(_, id)| format!("8:orgid:{id}"))
        .ok_or_else(|| format!("{upn} wasn't seeded").into())
}

/// The scene at `width`, in Teams Light or Dark (see the module docs).
pub fn card_at(width: u32, dark: bool) -> SandboxResult {
    let Some(sandbox) = sandbox_at(width)? else {
        return Ok(());
    };
    let server = own::Own::start()?;
    ensure(server.log().is_some(), || {
        "this scene needs the real ngteams-server binary (NGTEAMS_QA_SERVER_BIN)".to_owned()
    })?;
    seed(&sandbox, &server)?;
    let people = ids(&sandbox)?;
    let root = tempfile::tempdir()?;
    let kept = [
        (ROOT_ENV, root.path().display().to_string()),
        (OWN_ORIGIN_ENV, server.origin().to_owned()),
        (OWN_CA_ENV, server.ca_file().display().to_string()),
    ];
    let mode = if dark { "card-dark" } else { "card" };
    let (one, group) = write_chats(&server, &people)?;
    say(&format!("card-chats one={one} group={group}"));
    let (mut running, mut trace, _) = start_with(&sandbox, mode, &kept)?;
    let reader = mri_of(&people, OWN_UPN)?;
    accounts_shown(&mut trace, &mut running, &[LAB_A, super::LAB_B, &reader])?;
    let input = sandbox.input()?;
    let suffix = format!("-{width}{}", if dark { "-dark" } else { "" });
    let mut run = Run {
        sandbox: &sandbox,
        running,
        trace,
        input,
        suffix,
        places: Places::at(width),
    };
    let checked = own_cards(&mut run, &server, &people, (&one, &group)).and_then(|()| {
        if width > 480 && !dark {
            microsoft_beside(&mut run)
        } else {
            Ok(())
        }
    });
    if let Some(log) = server.log() {
        fs::write(
            super::shots_dir()?.join(format!("ngteams-qa-card-server{}.log", run.suffix)),
            log,
        )?;
    }
    if let Err(error) = checked {
        let _ = run.shot("failed");
        return Err(run.running.fail(&error.to_string()));
    }
    let Run { mut trace, running, .. } = run;
    super::quit_and_finish(&sandbox, &mut trace, running)
}

/// The own server's account shown: Bert's card from the 1:1 chat's header, the group chat's faces, search.
fn own_cards(
    run: &mut Run,
    server: &own::Own,
    people: &[(String, String)],
    (one, group): (&str, &str),
) -> SandboxResult {
    let reader = mri_of(people, OWN_UPN)?;
    ctl(
        run.sandbox,
        &["account", "switch", &format!("{}/{OWN_UPN}", server.origin())],
    )?;
    run.trace
        .wait_where("switched to the own account", |line| {
            about(line, "account", "switched", &reader)
        })
        .map_err(|error| run.running.fail(&error.to_string()))?;
    bert_from_header(run, one)?;
    group_faces(run, people, group)?;
    search(run)
}

/// The 1:1 chat's header face: Bert's full card, each tab.
fn bert_from_header(run: &mut Run, one: &str) -> SandboxResult {
    run.open_chat(one)?;
    if let Some(toggle) = run.places.list_toggle {
        run.shot("one-to-one-with-list")?;
        run.input.click(toggle.0, toggle.1, keys::BTN_LEFT)?;
        thread_pause();
        run.input.move_to(run.places.away.0, run.places.away.1)?;
    }
    run.shot("one-to-one")?;
    let face = run.places.header_face;
    run.input.click(face.0, face.1, keys::BTN_LEFT)?;
    run.opened("full")?;
    let card = run.card_read()?;
    run.shot("bert-overview")?;
    bert_holds(&card)?;
    let tab = run.places.tab_profile;
    run.tab(tab, "profile")?;
    run.shot("bert-profile")?;
    let tab = run.places.tab_organization;
    run.tab(tab, "organization")?;
    run.shot("bert-organization")?;
    run.escape()
}

/// The group chat: a rest on Bert's face (the compact card), then clicks on his, Olga's and Gil's.
fn group_faces(run: &mut Run, people: &[(String, String)], group: &str) -> SandboxResult {
    let bert = mri_of(people, BERT)?;
    run.open_chat(group)?;
    run.shot("group")?;
    let at = run.face_of(&bert)?;
    run.input.move_to(at.0, at.1)?;
    run.opened("compact")?;
    let compact = run.card_read()?;
    run.shot("bert-compact")?;
    compact.holds("name", BERT_NAME)?;
    compact.holds("form", "compact")?;
    // away from both the face and the card (which opens beside the face, over it near the window's foot)
    run.input.move_to(run.places.away.0, run.places.away.1)?;
    std::thread::sleep(Duration::from_secs(2));
    let gone = run.ask(CARD_ASK, "card")?;
    ensure(gone == "card none", || {
        format!("the compact card stayed after the pointer left: {gone}")
    })?;
    run.input.click(at.0, at.1, keys::BTN_LEFT)?;
    run.opened("full")?;
    let card = run.card_read()?;
    bert_holds(&card)?;
    run.shot("bert-full-from-message")?;
    run.escape()?;

    let olga = mri_of(people, OLGA)?;
    let at = run.face_of(&olga)?;
    run.input.click(at.0, at.1, keys::BTN_LEFT)?;
    run.opened("full")?;
    let card = run.card_read()?;
    run.shot("olga-overview")?;
    card.holds("name", OLGA_NAME)?;
    card.holds("ooo", "true")?;
    card.holds("schedule", "Some(OutOfOffice)")?;
    card.holds("managers", &format!("{CORA_NAME},{EVE_NAME}"))?;
    let tab = run.places.tab_organization;
    run.tab(tab, "organization")?;
    run.shot("olga-organization")?;
    run.escape()?;

    let gil = mri_of(people, GIL)?;
    let at = run.face_of(&gil)?;
    run.input.click(at.0, at.1, keys::BTN_LEFT)?;
    run.opened("full")?;
    let card = run.card_read()?;
    run.shot("gil-overview")?;
    card.holds("name", GIL_NAME)?;
    card.holds("managers", "")?;
    card.holds("directs", "")?;
    let tab = run.places.tab_organization;
    run.tab(tab, "organization")?;
    run.shot("gil-organization")?;
    run.escape()
}

/// The title bar's search for Bert: what it shows, and what `ngteamsctl search` finds.
fn search(run: &mut Run) -> SandboxResult {
    let found = ctl(run.sandbox, &["open", "search", "Bert"]);
    say(&format!(
        "note search-open {:?}",
        found.as_ref().map(|_| "ok").map_err(ToString::to_string)
    ));
    std::thread::sleep(Duration::from_secs(3));
    run.shot("search-bert")?;
    if let Ok(found) = ctl(run.sandbox, &["search", "Bert"]) {
        super::shots_dir()
            .and_then(|dir| fs::write(dir.join(format!("ngteams-qa-card-search{}.txt", run.suffix)), found))?;
    }
    run.input.key(keys::ESC)?;
    Ok(())
}

/// What Bert's card must hold: the server's names, mail, organization, working-with and schedule.
fn bert_holds(card: &Card) -> SandboxResult {
    card.holds("loaded", "read")?;
    card.holds("name", BERT_NAME)?;
    card.holds("given", "Bert")?;
    card.holds("emails", BERT)?;
    card.holds("upn", BERT)?;
    card.holds("job", "Engineer")?;
    card.holds("office", "Room 4")?;
    card.holds("ooo", "false")?;
    card.holds("managers", &format!("{CORA_NAME},{EVE_NAME}"))?;
    card.holds("manager_mail", &format!("{CORA},{EVE}"))?;
    card.holds("directs", &format!("{FINN_NAME},{ZOE_NAME}"))?;
    let working = card.get("working").unwrap_or_default();
    let mut working: Vec<&str> = working.split(',').collect();
    working.sort_unstable();
    let mut wanted = vec![DAN_NAME, OLGA_NAME, GIL_NAME, LONG_NAME];
    wanted.sort_unstable();
    ensure(working == wanted, || {
        format!("Bert works with {working:?}, not {wanted:?}")
    })?;
    ensure(
        card.get("schedule")
            .is_some_and(|status| !status.is_empty() && status != "none"),
        || format!("Bert's card has no schedule: {:?}", card.0),
    )
}

/// The app's Loki requests now, as the child says them: `(own, mock)`; the mock's own count and whether it got the
/// own account's token.
fn loki_counts(run: &Run) -> SandboxResult<(u32, u32, u32, u32)> {
    let line = run.ask(LOKI_ASK, "card-loki")?;
    say(&line);
    let count = |name: &str| {
        super::field(&line, name)
            .and_then(|value| value.parse::<u32>().ok())
            .ok_or_else(|| format!("{name} in {line}"))
    };
    Ok((count("own")?, count("mock")?, count("mock_seen")?, count("own_token")?))
}

/// a's card of c (the mock's Loki) in the same window: only the mock is asked, never the own server; and the own
/// account's cards asked only its server.
fn microsoft_beside(run: &mut Run) -> SandboxResult {
    let (own_before, mock_before, seen_before, token) = loki_counts(run)?;
    ensure(
        own_before > 0 && mock_before == 0 && seen_before == 0 && token == 0,
        || {
            format!(
                "after the own cards: own={own_before} mock={mock_before} mock_seen={seen_before} own_token={token}"
            )
        },
    )?;
    ctl(run.sandbox, &["account", "switch", LAB_A])?;
    run.trace
        .wait_where("switched to a", |line| about(line, "account", "switched", LAB_A))
        .map_err(|error| run.running.fail(&error.to_string()))?;
    run.open_chat(AC_CHAT)?;
    run.shot("microsoft-one-to-one")?;
    let face = run.places.microsoft_header;
    run.input.click(face.0, face.1, keys::BTN_LEFT)?;
    run.opened("full")?;
    let card = run.card_read()?;
    run.shot("microsoft-c-overview")?;
    card.holds("name", "Lab Sea")?;
    let tab = run.places.tab_organization;
    run.tab(tab, "organization")?;
    run.shot("microsoft-c-organization")?;
    run.escape()?;
    let (own_after, mock_after, seen_after, token) = loki_counts(run)?;
    ensure(
        own_after == own_before && mock_after > 0 && seen_after > 0 && token == 0,
        || {
            format!(
                "after a's card: own={own_before}->{own_after} mock={mock_after} mock_seen={seen_after} own_token={token}"
            )
        },
    )?;
    let requests = fs::read_to_string(run.sandbox.path(LOKI_FILE)).unwrap_or_default();
    fs::write(super::shots_dir()?.join("ngteams-qa-card-loki-requests.txt"), requests)?;
    Ok(())
}

// --- the child ---

/// The own server's account of the `card` modes, signed in on the parent's server and started.
pub fn started(runner: &Runner, server: &OwnServer) -> SandboxResult<Vec<Started>> {
    let account = accounts::add_own(&runner.setup, OWN_UPN, server, &Secret::new(own::PASSWORD))?;
    Ok(vec![runner.start(&account).map_err(String::from)?])
}

/// The mock's Loki for a's card of c (and b's).
pub fn serve_mock_loki(mock: &ChatServiceMock, runtime: &tokio::runtime::Runtime) {
    runtime.block_on(mock.serve_loki(&[
        LokiCard {
            mri: super::LAB_B,
            name: "Lab Bee",
            email: "bot-b@lab.example.test",
        },
        LokiCard {
            mri: LAB_C,
            name: "Lab Sea",
            email: "bot-c@lab.example.test",
        },
    ]));
}

/// The window's shell of the account shown.
fn shell_of(window: WindowHandle<KitRoot>, cx: &mut App) -> Option<Entity<Shell>> {
    window
        .update(cx, |kit, _, cx| {
            let root = kit.view().clone().downcast::<Root>().ok()?;
            root.read(cx).shell_in(cx).cloned()
        })
        .ok()
        .flatten()
}

/// `faces <mri>=<x>,<y> …`: the centre of the face beside each other person's last message in the chat shown.
fn faces(window: WindowHandle<KitRoot>, cx: &mut App) -> String {
    let mut said = vec!["faces".to_owned()];
    if let Some(chat) = shell_of(window, cx).and_then(|shell| shell.read(cx).chat().cloned()) {
        let chat = chat.read(cx);
        let mut seen = Vec::new();
        for item in chat.items().iter().rev() {
            let Item::Message(message) = item else { continue };
            let Some(sender) = message.sender.clone().filter(|_| message.header && !message.own) else {
                continue;
            };
            if seen.contains(&sender) {
                continue;
            }
            if let Some(face) = chat.face_bounds(&message.id) {
                let centre = face.center();
                said.push(format!(
                    "{sender}={:.0},{:.0}",
                    f32::from(centre.x),
                    f32::from(centre.y)
                ));
                seen.push(sender);
            }
        }
    }
    said.join(" ")
}

/// `card none`, or `card key=value|…` of the card open: its form, tab, whether it read its person, and what it read.
fn card(window: WindowHandle<KitRoot>, cx: &mut App) -> String {
    let Some(card) = shell_of(window, cx).and_then(|shell| shell.read(cx).profile_card().cloned()) else {
        return "card none".to_owned();
    };
    let card = card.read(cx);
    let form = match card.form() {
        Form::Full => "full",
        Form::Compact => "compact",
    };
    let tab = match card.tab() {
        CardTab::Overview => "overview",
        CardTab::Profile => "profile",
        CardTab::Organization => "organization",
    };
    let mut fields = vec![
        format!("form={form}"),
        format!("tab={tab}"),
        format!("of={}", card.of().mri),
    ];
    match card.loaded() {
        Loaded::Loading => fields.push("loaded=loading".to_owned()),
        Loaded::Failed => fields.push("loaded=failed".to_owned()),
        Loaded::Read(data) => {
            let names = |people: &[teams_core::CardPerson]| {
                people
                    .iter()
                    .map(|person| person.name.clone().unwrap_or_default())
                    .collect::<Vec<_>>()
                    .join(",")
            };
            let mails = |people: &[teams_core::CardPerson]| {
                people
                    .iter()
                    .map(|person| person.email.clone().unwrap_or_default())
                    .collect::<Vec<_>>()
                    .join(",")
            };
            let organization = data.organization.clone().unwrap_or_default();
            let working = data.working_with.clone().unwrap_or_default();
            fields.extend([
                "loaded=read".to_owned(),
                format!("name={}", data.name.clone().unwrap_or_default()),
                format!("given={}", data.given_name.clone().unwrap_or_default()),
                format!("emails={}", data.emails.join(",")),
                format!("upn={}", data.user_principal_name.clone().unwrap_or_default()),
                format!("job={}", data.job_title.clone().unwrap_or_default()),
                format!("office={}", data.office.clone().unwrap_or_default()),
                format!(
                    "ooo={}",
                    data.out_of_office.map_or("none".to_owned(), |ooo| ooo.to_string())
                ),
                format!(
                    "schedule={}",
                    data.schedule
                        .as_ref()
                        .map_or("none".to_owned(), |schedule| format!("{:?}", schedule.current))
                ),
                format!("organization={}", data.organization.is_some()),
                format!("managers={}", names(&organization.managers)),
                format!("manager_mail={}", mails(&organization.managers)),
                format!("directs={}", names(&organization.directs)),
                format!("working={}", names(&working)),
            ]);
        }
    }
    format!("card {}", fields.join("|"))
}

/// Whether `url` reads Loki (the person card's service), on Microsoft's base or an own server's `/loki/`.
fn is_loki(url: &str) -> bool {
    [
        "/loki/",
        "livepersonacard",
        "/api/v2/person",
        "/api/v1/organization",
        "/api/v1/workingwith",
        "/api/v1/schedule",
        "/api/v1/oofstatus",
    ]
    .iter()
    .any(|part| url.contains(part))
}

/// Answers the parent's asks while the app runs: the faces ([`FACES_ASK`]), the card ([`CARD_ASK`]) and the Loki
/// requests ([`LOKI_ASK`]: `card-loki own=<n> mock=<n> mock_seen=<n> own_token=<n>`, from the app's log, the
/// mock's record, and whether any of the mock's requests carried the own account's skypetoken).
pub fn answer(window: WindowHandle<KitRoot>, mock: Arc<ChatServiceMock>, setup: Setup, origin: String, cx: &mut App) {
    use std::fmt::Write as _;
    use teams_auth::secrets::SecretStore as _;
    let Ok(dir) = own::sandbox_dir() else {
        say("no sandbox dir");
        return;
    };
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            if fs::remove_file(dir.join(FACES_ASK)).is_ok() {
                say(&cx.update(|cx| faces(window, cx)));
            }
            if fs::remove_file(dir.join(CARD_ASK)).is_ok() {
                say(&cx.update(|cx| card(window, cx)));
            }
            if fs::remove_file(dir.join(LOKI_ASK)).is_err() {
                continue;
            }
            let (mut own_count, mut mock_count, mut listed) = (0, 0, String::new());
            for (method, url) in own::logged_requests() {
                if !is_loki(&url) {
                    continue;
                }
                let place = if url.starts_with(&origin) {
                    own_count += 1;
                    "own-server"
                } else {
                    mock_count += 1;
                    "microsoft-mock"
                };
                let _ = writeln!(listed, "{place} {method} {url}");
            }
            let _ = fs::write(dir.join(LOKI_FILE), listed);
            let requests = mock.received_requests().await;
            let seen = requests.iter().filter(|request| is_loki(request.url.path())).count();
            let own = setup
                .signed_in_accounts()
                .unwrap_or_default()
                .into_iter()
                .find(|account| account.server.is_some());
            let token = match own {
                Some(account) => setup
                    .secrets()
                    .get(&teams_auth::authz::skypetoken_key(&account.id()))
                    .await
                    .ok()
                    .flatten(),
                None => None,
            };
            let own_token = token.map_or(0, |token| {
                requests
                    .iter()
                    .filter(|request| {
                        request
                            .headers
                            .values()
                            .any(|value| value.to_str().is_ok_and(|value| value.contains(token.expose())))
                    })
                    .count()
            });
            say(&format!(
                "card-loki own={own_count} mock={mock_count} mock_seen={seen} own_token={own_token}"
            ));
        }
    })
    .detach();
}
