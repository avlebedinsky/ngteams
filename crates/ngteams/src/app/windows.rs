//! Distinct Windows shell identities keep lab/read-only metadata separate from the installed client.
use crate::accounts::Runner;
use gpui::App;
use teams_ui::window::PERSONAL_APP_ID;

pub(super) fn desktop_id(runner: &Runner, read_only: bool, cx: &App) -> &'static str {
    static LAB_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    static READ_ONLY_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let id = if read_only {
        READ_ONLY_ID.get_or_init(|| format!("com.nickalie.ngteams.readonly.{}", std::process::id()))
    } else if runner.setup.secrets().application() == crate::lab::LAB_KEYRING_APPLICATION {
        LAB_ID.get_or_init(|| format!("com.nickalie.ngteams.lab.{}", std::process::id()))
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
