//! What the shell and a chat view are made of over a rig (`common`), for the test crates that build either.

use std::sync::Arc;

use gpui::AppContext as _;
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::{ShellParts, ViewTrace};

use crate::common::Rig;

/// The rig's core (the bridge's request side, as the app hands it) and models, `settings` in memory, `trace`.
pub fn parts(rig: &Rig, settings: Settings, trace: Arc<ViewTrace>) -> ShellParts {
    ShellParts {
        core: rig.cx.read(|cx| rig.bridge.read(cx).core()),
        models: rig.models.clone(),
        settings: rig.cx.update(|cx| cx.new(|_| SettingsModel::in_memory(settings))),
        trace,
    }
}
