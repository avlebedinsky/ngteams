//! Distinct Windows shell identities keep lab/read-only metadata separate from the installed client.
use crate::accounts::Runner;
use gpui::App;
use teams_ui::window::PERSONAL_APP_ID;

pub(super) fn desktop_id(runner: &Runner, read_only: bool, cx: &App) -> &'static str {
    let id = if read_only {
        "com.nickalie.ngteams.readonly"
    } else if runner.setup.secrets().application() == crate::lab::LAB_KEYRING_APPLICATION {
        "com.nickalie.ngteams.lab"
    } else {
        PERSONAL_APP_ID
    };
    if !cx.is_test()
        && let Err(error) = teams_platform::windows_desktop::set_app_id(id)
    {
        log::warn!("Windows app identity: {error}");
    }
    id
}
