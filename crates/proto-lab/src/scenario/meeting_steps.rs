//! The `meeting-steps` scenario: one account's web client in a meeting, driven step by step from outside over a Unix
//! socket as `team-manage` is ([`super::team_manage`]'s steps), to look at what a meeting's pre-join, its lobby, its
//! options and a channel's meeting send (`docs/protocol/calling.md`). Two of them, one per account and socket, play the
//! organizer and an invitee at once. Its reports and screenshots go to the capture dir; nothing of them enters the repo.
//!
//! **What it may send.** Members are read live, so a write passes only by the lab rule: a meeting's or a channel's call
//! whose chat's members, read fresh and in full, are lab accounts; anything the rule can't read is denied and only
//! shows what the page tried. Before the steps, the page proves the fence: a probe post to its 1:1 chat with
//! `PROTO_LAB_PEER` (`b`, or `a` for b) is force-denied and must fail in the page. A `hold` step force-denies what
//! a run wants to see but not send. Every deny is expected: the run judges nothing, it records.
//!
//! `PROTO_LAB_ACCOUNT` picks the account (`a`, `b` or `c`; `a` when unset); its microphone is Chrome's fake one playing
//! a tone, its camera Chrome's fake one.

use std::path::PathBuf;

use crate::browser::Media;
use crate::classify::Classifier;
use crate::config::{Alias, LabConfig, LabLocations, LabPaths};
use crate::session::{LabSession, SessionOptions};

use super::ab_chat::{
    CHAT_OPEN, COMPOSE, chat_link, chat_url, deny_probes, enforce, forget_unsent_probes, load, nonce, one_to_one_id,
    probe_text, setup,
};
use super::ab_meeting::{cancel_scheduled, created_chat, created_join_link, schedule};
use super::ab_trouter::size;
use super::echo_call::{USER_AGENT, write_burst_tone};
use super::team_manage::serve;
use super::{DenyPolicy, RunOptions, ScenarioError, ScenarioName, ScenarioReport};

/// The services `seen` looks at: the middle tier, CSA, the chat service, and the call services behind the flight proxy.
const MEETING_SERVICES: [&str; 8] = [
    "/api/mt/",
    "/api/csa/",
    "/api/chatsvc/",
    "api.flightproxy.teams.microsoft.com/",
    ".conv.skype.com/",
    ".cc.skype.com/",
    "/meetingOptions/",
    "/api/v1/meetings",
];

fn alias_of(name: &str, default: Alias) -> Alias {
    match std::env::var(name).as_deref() {
        Ok("a") => Alias::A,
        Ok("b") => Alias::B,
        Ok("c") => Alias::C,
        _ => default,
    }
}

/// Runs `meeting-steps` (see the module docs).
///
/// # Errors
/// When the run can't start: no lab config, no Chrome, no fence, a browser that can't be armed, no socket named.
pub async fn run(paths: &LabPaths, options: RunOptions) -> Result<ScenarioReport, ScenarioError> {
    super::require_lab_scenario(super::ScenarioName::MeetingSteps)?;
    let config = LabConfig::load(&LabLocations::from_env()?)?;
    let alias = alias_of("PROTO_LAB_ACCOUNT", Alias::A);
    let peer = alias_of("PROTO_LAB_PEER", if alias == Alias::B { Alias::A } else { Alias::B });
    let tone = write_burst_tone(paths).map_err(|error| setup(format!("can't write the tone: {error}")))?;
    let session_options = SessionOptions {
        classifier: Classifier::new(),
        live_members: true,
        capture: Some(ScenarioName::MeetingSteps.as_str().to_owned()),
        capture_request_bodies: true,
        display: options.display,
        media: Media::Fake { audio: Some(tone) },
        user_agent: Some(USER_AGENT.to_owned()),
    };
    let mri = |alias| config.account(alias).mri.clone().unwrap_or_default();
    let (me, them) = (mri(alias), mri(peer));
    let peer_upn = config.account(peer).upn.clone();
    let mut page = LabSession::open_with(paths, alias, session_options).await?;
    let socket: PathBuf = std::env::var_os("PROTO_LAB_STEPS_SOCKET")
        .map(PathBuf::from)
        .ok_or_else(|| ScenarioError::Setup("PROTO_LAB_STEPS_SOCKET names no socket".to_owned()))?;
    let mut report = ScenarioReport::new(ScenarioName::MeetingSteps);
    let nonce = nonce();
    let result = match deny_probes(&page, Some(&probe_text(&nonce))) {
        Ok(()) => {
            let who = Who {
                alias,
                me: &me,
                them: &them,
                peer_upn: &peer_upn,
            };
            steps(&who, &mut page, &mut report, &nonce, &socket, options).await
        }
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        report.failures.push(error.to_string());
    }
    report.close_account(alias, page, &DenyPolicy::AllExpected).await;
    let _ = std::fs::remove_file(&socket);
    Ok(report)
}

/// The account and its peer.
struct Who<'a> {
    alias: Alias,
    me: &'a str,
    them: &'a str,
    peer_upn: &'a str,
}

/// Opens the account's chat with `them` for the probe: an earlier run's unsent probe at the head of the chat's queue
/// would hold this run's back for good, so any is forgotten first and the chat opened again.
async fn open_chat(
    drive: &mut super::dsl::Driver,
    report: &mut ScenarioReport,
    them: &str,
    options: RunOptions,
) -> Result<(), ScenarioError> {
    for again in [false, true] {
        drive
            .navigate("the account opens its chat with its peer", &chat_link(them))
            .await?;
        drive
            .wait_for("the compose box", COMPOSE, options.load_timeout.max(CHAT_OPEN))
            .await?;
        if again {
            break;
        }
        let forgotten = forget_unsent_probes(drive).await?;
        if forgotten == 0 {
            break;
        }
        report.notes.push(format!(
            "forgot {forgotten} record(s) of earlier runs' unsent probes; the chat opened again"
        ));
    }
    Ok(())
}

async fn steps(
    who: &Who<'_>,
    page: &mut LabSession,
    report: &mut ScenarioReport,
    nonce: &str,
    socket: &std::path::Path,
    options: RunOptions,
) -> Result<(), ScenarioError> {
    let (alias, me, them) = (who.alias, who.me, who.them);
    let chat = one_to_one_id(me, them)
        .ok_or_else(|| setup("lab.toml has no MRI for the account or its peer: run `proto-lab login`".to_owned()))?;
    load(alias, page, options.load_timeout).await?;
    size(page).await?;
    let mut drive = page.driver();
    open_chat(&mut drive, report, them, options).await?;
    let names_chat = chat_url(&chat).map_err(|error| setup(error.to_string()))?;
    enforce(page, &mut drive, &probe_text(nonce), &names_chat).await?;
    report
        .notes
        .push("enforcement: the probe post was denied by the fence and failed in the page".to_owned());
    let made = match std::env::var("PROTO_LAB_SCHEDULE") {
        Ok(title) if !title.is_empty() => {
            Some(made_meeting(page, &mut drive, report, &title, who.peer_upn, options).await?)
        }
        _ => None,
    };
    // what the probe sent is behind us: `seen` starts after it
    let mut drive = page.driver();
    let served = serve(alias, page, &mut drive, report, socket, &MEETING_SERVICES).await;
    let to_cancel = made.or_else(|| std::env::var("PROTO_LAB_CANCEL").ok().filter(|chat| !chat.is_empty()));
    if let Some(chat) = to_cancel {
        let mut drive = page.driver();
        let cancelled = cancel_scheduled(page, &mut drive, report, &chat, options).await;
        report.notes.push(match &cancelled {
            Ok(()) => format!("the meeting {chat} was cancelled"),
            Err(error) => format!("the meeting {chat} wasn't cancelled: {error}"),
        });
        cancelled?;
    }
    served
}

/// Schedules a meeting of the account's with its peer (`PROTO_LAB_SCHEDULE` its title) and writes its chat and join
/// link to `meeting.txt` in the capture dir, for the steps to go on with; its chat, which the run cancels as it ends.
async fn made_meeting(
    page: &mut LabSession,
    drive: &mut super::dsl::Driver,
    report: &mut ScenarioReport,
    title: &str,
    upn: &str,
    options: RunOptions,
) -> Result<String, ScenarioError> {
    let body = schedule(page, drive, report, title, upn, options).await?;
    let link = created_join_link(body.as_deref()).ok_or_else(|| setup("the creation named no join link".to_owned()))?;
    let chat = created_chat(body.as_deref()).ok_or_else(|| setup("the creation named no chat".to_owned()))?;
    if let Some(dir) = page.capture_dir() {
        std::fs::write(dir.join("meeting.txt"), format!("{chat}\n{link}\n"))
            .map_err(|error| setup(format!("can't write meeting.txt: {error}")))?;
    }
    report.notes.push(format!("scheduled the meeting {chat}"));
    Ok(chat)
}
