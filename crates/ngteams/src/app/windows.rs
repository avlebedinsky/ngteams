//! Distinct Windows shell identities keep lab/read-only metadata separate from the installed client.
use crate::accounts::Runner;
use gpui::App;
use teams_ui::window::PERSONAL_APP_ID;

pub(super) fn desktop_id(runner: &Runner, read_only: bool, cx: &App) -> &'static str {
    shell_id(&runner.setup, read_only, cx)
}

pub(crate) fn shell_id(setup: &crate::setup::Setup, read_only: bool, cx: &App) -> &'static str {
    static LAB_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    static READ_ONLY_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let id = if read_only {
        READ_ONLY_ID.get_or_init(|| format!("com.nickalie.ngteams.readonly.{}", std::process::id()))
    } else if setup.secrets().application() == crate::lab::LAB_KEYRING_APPLICATION {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{accounts_file::PersonalPaths, secrets::AppSecrets, setup::Setup};

    #[test]
    fn shell_identity_stays_with_its_mode_before_and_after_sign_in() {
        let directory = tempfile::tempdir().unwrap();
        let setup = Setup::new(PersonalPaths::new(
            directory.path().join("config"),
            directory.path().join("data"),
        ))
        .unwrap();
        let (socket, _peer) = teams_auth::broker::private_pair().unwrap();
        let lab = setup.clone().with_secrets(AppSecrets::lab(socket).unwrap());
        let cx = gpui::TestAppContext::single();
        cx.read(|cx| {
            let lab_id = shell_id(&lab, false, cx);
            assert!(lab_id.starts_with("com.nickalie.ngteams.lab."));
            assert!(lab_id.ends_with(&format!(".{}", std::process::id())));
            let read_only = shell_id(&setup, true, cx);
            assert!(read_only.starts_with("com.nickalie.ngteams.readonly."));
            assert_ne!(read_only, lab_id);
            assert_eq!(shell_id(&setup, false, cx), PERSONAL_APP_ID);
            assert_eq!(shell_id(&lab, false, cx), lab_id);
            assert_eq!(shell_id(&lab, true, cx), read_only);
        });
    }
}
