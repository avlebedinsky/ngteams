//! Records calls in the configured lab group chat, reusing the two-account call driver with both configured callees.

use crate::config::LabChats;

use crate::config::{Alias, LabConfig, LabLocations, LabPaths};

use super::ab_call::{CALLER, CallPlan, Modality, RunCall, mri, run_plan};
use super::ab_chat::setup;
use super::abc_chat::chat_link_by_id;
use super::{RunOptions, ScenarioError, ScenarioName, ScenarioReport};

/// The accounts a calls in the group chat.
pub const CALLEES: [Alias; 2] = [Alias::B, Alias::C];

/// Runs `abc-call` (see the module docs).
///
/// # Errors
/// When the run can't start: no `chats.toml` entry for the group chat, no lab config or no MRI for a, b or c, no
/// Chrome, a browser whose capture setup fails.
pub async fn run(paths: &LabPaths, options: RunOptions) -> Result<ScenarioReport, ScenarioError> {
    let chats = LabChats::load(paths).map_err(|error| setup(error.to_string()))?;
    let chat_id = chats
        .abc
        .ok_or_else(|| setup("chats.toml records no a-b-c chat: run `proto-lab run abc-chat` first".to_owned()))?;

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
        bystanders: super::ab_call::bystanders(options.call),
        // a group chat's header offers an audio call, and "Meet now", a meeting (M4): no video call to start
        held_starts: &[Modality::Audio],
        step: options.call,
        options,
        pauses: super::ab_call::CallPauses::default(),
    };
    run_plan(paths, &plan).await
}
