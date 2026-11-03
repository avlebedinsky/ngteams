//! The `abc-call` scenario: account a calls the lab's a–b–c group chat (the one `abc-chat` recorded in `chats.toml`)
//! from the web client, b and c the callees. It runs as `ab-call` does ([`super::ab_call`]: the same steps, holds and
//! judgement), from the group chat's header instead of the 1:1 chat's.
//!
//! It starts only when the lab's live source reads the recorded chat's members
//! ([`super::abc_chat::messaging_blocked`]): the fence judges the start by that read, so a chat it can't read would
//! only deny it.

use crate::config::LabChats;

use crate::config::{Alias, LabConfig, LabLocations, LabPaths};

use super::ab_call::{CALLER, CallPlan, Modality, RunCall, mri, run_plan};
use super::ab_chat::setup;
use super::abc_chat::{chat_link_by_id, lab_source, messaging_blocked};
use super::{RunOptions, ScenarioError, ScenarioName, ScenarioReport};

/// The accounts a calls in the group chat.
pub const CALLEES: [Alias; 2] = [Alias::B, Alias::C];

/// Runs `abc-call` (see the module docs).
///
/// # Errors
/// When the run can't start: no `chats.toml` entry for the group chat, no lab config or no MRI for a, b or c, no
/// Chrome, no fence, a browser that can't be armed.
pub async fn run(paths: &LabPaths, options: RunOptions) -> Result<ScenarioReport, ScenarioError> {
    let chats = LabChats::load(paths).map_err(|error| setup(error.to_string()))?;
    let chat_id = chats
        .abc
        .ok_or_else(|| setup("chats.toml records no a-b-c chat: run `proto-lab run abc-chat` first".to_owned()))?;
    if let Some(reason) = messaging_blocked(&lab_source()?, &chat_id) {
        let mut report = ScenarioReport::new(ScenarioName::AbcCall);
        report.failures.push(reason);
        return Ok(report);
    }
    let config = LabConfig::load(&LabLocations::from_env()?)?;
    let callees = CALLEES
        .into_iter()
        .map(|alias| Ok((alias, mri(&config, alias)?)))
        .collect::<Result<_, ScenarioError>>()?;
    let plan = CallPlan {
        scenario: ScenarioName::AbcCall,
        chat_link: chat_link_by_id(&chat_id),
        call: RunCall {
            caller: mri(&config, CALLER)?,
            group_chat: Some(chat_id.clone()),
        },
        callees,
        // a group chat's header offers an audio call, and "Meet now", a meeting (M4): no video call to start
        held_starts: &[Modality::Audio],
        step: options.call,
        options,
        pauses: super::ab_call::CallPauses::default(),
    };
    run_plan(paths, &plan).await
}
