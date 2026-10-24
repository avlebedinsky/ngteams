//! The settings and their file: defaults for a missing or unreadable file, a bad value reported with its default kept,
//! the file `0600` in a `0700` directory, written whole; the model writing each change at once. Every file here is in
//! a test's own temporary directory.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use gpui::{AppContext as _, TestAppContext};
use teams_ui::settings::{
    DEFAULT_IDLE_MINUTES, DIR_MODE, FILE_MODE, FirstRunState, LogLevel, ManualStatus, Notifications, Problem, Settings,
    SettingsFile, SettingsModel, parse, render,
};

const CHAT: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";

fn mode(path: &Path) -> u32 {
    fs::metadata(path).expect("metadata").permissions().mode() & 0o777
}

/// The settings file of lab account a under `dir`, as the lab app keeps it.
fn file_in(dir: &tempfile::TempDir) -> (SettingsFile, PathBuf) {
    let path = dir.path().join("ui").join("a.toml");
    (SettingsFile::new(&path), path)
}

fn changed() -> Settings {
    Settings {
        selected_chat: Some(CHAT.to_owned()),
        send_typing: false,
        send_read_receipts: false,
        send_presence: false,
        first_run: FirstRunState::Confirmed,
        notifications: Notifications::Mentions,
        idle_minutes: 15,
        manual_status: ManualStatus::DoNotDisturb,
        log_level: LogLevel::Debug,
    }
}

#[test]
fn the_log_level_is_info_by_default_and_read_from_the_file() {
    assert_eq!(Settings::default().log_level, LogLevel::Info);
    for level in LogLevel::ALL {
        let settings = Settings {
            log_level: level,
            ..Settings::default()
        };
        let text = render(&settings);
        assert!(text.contains(&format!("log_level = \"{}\"", level.key())), "{text}");
        let (read, problems) = parse(&text).expect("TOML");
        assert_eq!(read, settings);
        assert!(problems.is_empty());
    }
    // `trace` is `debug`; any other name is reported and the default kept
    let (read, problems) = parse("log_level = \"trace\"").expect("TOML");
    assert_eq!(read.log_level, LogLevel::Debug);
    assert!(problems.is_empty());
    let (read, problems) = parse("log_level = \"loud\"").expect("TOML");
    assert_eq!(read.log_level, LogLevel::Info);
    assert!(matches!(&problems[..], [Problem::BadValue { key: "log_level", .. }]));
}

#[test]
fn a_missing_file_gives_the_defaults_and_no_problem() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (file, path) = file_in(&dir);
    let loaded = file.load();
    assert_eq!(loaded.settings, Settings::default());
    assert!(loaded.problems.is_empty());
    assert!(!path.exists(), "reading writes nothing");
    let defaults = Settings::default();
    assert!(defaults.send_typing && defaults.send_read_receipts);
    assert_eq!(defaults.notifications, Notifications::All);
    assert_eq!(defaults.idle_minutes, DEFAULT_IDLE_MINUTES);
    assert_eq!(defaults.manual_status, ManualStatus::Automatic);
    assert_eq!(defaults.selected_chat, None);
}

#[test]
fn saved_settings_are_read_back_from_a_private_file_in_a_private_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (file, path) = file_in(&dir);
    file.save(&changed()).expect("saved");
    assert_eq!(mode(&path), FILE_MODE);
    assert_eq!(mode(path.parent().expect("dir")), DIR_MODE);
    let loaded = file.load();
    assert_eq!(loaded.settings, changed());
    assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
    // nothing but the file is left in its directory
    let names: Vec<_> = fs::read_dir(path.parent().expect("dir"))
        .expect("listed")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    assert_eq!(names, ["a.toml"]);
}

#[test]
fn every_choice_goes_through_the_file() {
    for notifications in Notifications::ALL {
        for manual_status in ManualStatus::ALL {
            let settings = Settings {
                notifications,
                manual_status,
                ..Settings::default()
            };
            let (read, problems) = parse(&render(&settings)).expect("TOML");
            assert_eq!(read, settings);
            assert!(problems.is_empty());
        }
    }
}

#[test]
fn a_file_that_isn_t_toml_or_can_t_be_read_gives_the_defaults_reported() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (file, path) = file_in(&dir);
    fs::create_dir_all(path.parent().expect("dir")).expect("dir");
    fs::write(&path, "send_typing = [unclosed").expect("written");
    let loaded = file.load();
    assert_eq!(loaded.settings, Settings::default());
    assert!(
        matches!(&loaded.problems[..], [Problem::Unreadable { why, .. }] if why.contains("TOML")),
        "{:?}",
        loaded.problems
    );

    fs::remove_file(&path).expect("removed");
    fs::create_dir(&path).expect("a directory where the file goes");
    let loaded = file.load();
    assert_eq!(loaded.settings, Settings::default());
    assert!(matches!(&loaded.problems[..], [Problem::Unreadable { .. }]));
}

#[test]
fn a_bad_value_is_reported_and_its_default_kept_the_rest_taken() {
    let text = format!(
        "selected_chat = {CHAT:?}\nsend_typing = \"yes\"\nsend_read_receipts = false\nnotifications = \"loud\"\n\
         idle_minutes = 0\nmanual_status = \"busy\"\ncolour = \"purple\"\n"
    );
    let (settings, problems) = parse(&text).expect("TOML");
    assert_eq!(
        settings,
        Settings {
            selected_chat: Some(CHAT.to_owned()),
            send_read_receipts: false,
            manual_status: ManualStatus::Busy,
            ..Settings::default()
        }
    );
    let keys: Vec<String> = problems
        .iter()
        .map(|problem| match problem {
            Problem::BadValue { key, .. } => (*key).to_owned(),
            Problem::UnknownKey { key } => format!("unknown {key}"),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(keys, ["unknown colour", "idle_minutes", "notifications", "send_typing"]);
    for problem in &problems {
        assert!(problem.to_string().contains("setting") || problem.to_string().contains("default"));
    }

    for bad in [
        "idle_minutes = 241",
        "idle_minutes = -5",
        "idle_minutes = 2.5",
        "selected_chat = \"\"",
        "selected_chat = \"has space\"",
        "selected_chat = 7",
        "manual_status = \"Busy\"",
    ] {
        let (settings, problems) = parse(bad).expect("TOML");
        assert_eq!(settings, Settings::default(), "{bad}");
        assert_eq!(problems.len(), 1, "{bad}");
    }
}

#[test]
fn a_file_readable_by_others_is_tightened_when_read() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (file, path) = file_in(&dir);
    file.save(&changed()).expect("saved");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("loosened");
    let loaded = file.load();
    assert_eq!(loaded.settings, changed());
    assert_eq!(mode(&path), FILE_MODE);
}

#[test]
fn a_save_tightens_a_directory_open_to_others() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (file, path) = file_in(&dir);
    let parent = path.parent().expect("dir");
    fs::create_dir_all(parent).expect("dir");
    fs::set_permissions(parent, fs::Permissions::from_mode(0o755)).expect("loosened");
    file.save(&Settings::default()).expect("saved");
    assert_eq!(mode(parent), DIR_MODE);
}

#[gpui::test]
fn the_model_writes_each_change_at_once_and_only_a_change(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (file, path) = file_in(&dir);
    let model = cx.new(|_| SettingsModel::load(file));
    assert!(!path.exists());

    let changes = std::rc::Rc::new(std::cell::Cell::new(0));
    let seen = std::rc::Rc::clone(&changes);
    let _observing = cx.update(|cx| cx.observe(&model, move |_, _| seen.set(seen.get() + 1)));
    // the same value again changes nothing: nothing is written, nobody is told
    cx.update(|cx| model.update(cx, |model, cx| model.change(|settings| settings.send_typing = true, cx)));
    assert!(!path.exists());
    assert_eq!(changes.get(), 0);

    cx.update(|cx| {
        model.update(cx, |model, cx| {
            model.change(|settings| settings.notifications = Notifications::Off, cx);
        });
    });
    assert_eq!(changes.get(), 1);
    assert_eq!(
        SettingsFile::new(&path).load().settings.notifications,
        Notifications::Off
    );
}

#[gpui::test]
fn a_save_that_fails_is_reported_and_the_change_holds(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    // no file yet (the defaults, no problem), and its directory can't be made: the parent is read-only
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o500)).expect("read-only");
    let (file, path) = file_in(&dir);
    let model = cx.new(|_| SettingsModel::load(file));
    assert!(cx.read(|cx| model.read(cx).problems().is_empty()));
    cx.update(|cx| model.update(cx, |model, cx| model.change(|settings| settings.idle_minutes = 30, cx)));
    let unsaved = |model: &SettingsModel| {
        model
            .problems()
            .iter()
            .filter(|problem| matches!(problem, Problem::Unsaved { .. }))
            .count()
    };
    cx.read(|cx| {
        let model = model.read(cx);
        assert_eq!(model.settings().idle_minutes, 30);
        assert_eq!(unsaved(model), 1, "{:?}", model.problems());
    });
    // a later failure replaces it: one at a time
    cx.update(|cx| model.update(cx, |model, cx| model.change(|settings| settings.idle_minutes = 60, cx)));
    assert_eq!(cx.read(|cx| unsaved(model.read(cx))), 1);
    assert!(!path.exists());
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).expect("writable again");
}

#[gpui::test]
fn a_file_that_couldn_t_be_read_is_never_written_over(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (file, path) = file_in(&dir);
    // the user's settings, with a slip that makes the file not TOML
    let text = "notifications = \"mentions\"\nidle_minutes = 30\n[broken\n";
    fs::create_dir_all(path.parent().expect("parent")).expect("dir");
    fs::write(&path, text).expect("written");
    let model = cx.new(|_| SettingsModel::load(file));
    cx.read(|cx| {
        let model = model.read(cx);
        assert_eq!(model.settings(), &Settings::default());
        assert!(matches!(model.problems(), [Problem::Unreadable { .. }]));
    });

    // a chat opened (as the shell keeps it) and a setting changed: both hold in memory, the file stays as it was
    cx.update(|cx| {
        model.update(cx, |model, cx| {
            model.change(|settings| settings.selected_chat = Some(CHAT.to_owned()), cx);
            model.change(|settings| settings.send_typing = false, cx);
        });
    });
    assert_eq!(fs::read_to_string(&path).expect("read"), text);
    cx.read(|cx| {
        let model = model.read(cx);
        assert_eq!(model.settings().selected_chat.as_deref(), Some(CHAT));
        assert!(!model.settings().send_typing);
        let problems = model.problems();
        assert!(matches!(problems[0], Problem::Unreadable { .. }), "{problems:?}");
        assert!(
            matches!(&problems[1..], [Problem::Unsaved { why, .. }] if why.contains("couldn't be read")),
            "{problems:?}"
        );
    });
}

#[test]
fn only_the_captured_manual_statuses_are_kept_and_each_is_the_policy_s_choice() {
    use teams_core::Choice;
    use teams_proto::ManualAvailability;
    // "Be right back" and "Appear offline" were never captured (M2 Task 18): reported, Automatic kept
    for uncaptured in ["be-right-back", "appear-offline"] {
        let (read, problems) = parse(&format!("manual_status = \"{uncaptured}\"")).expect("TOML");
        assert_eq!(read.manual_status, ManualStatus::Automatic, "{uncaptured}");
        assert!(
            matches!(
                &problems[..],
                [Problem::BadValue {
                    key: "manual_status",
                    ..
                }]
            ),
            "{uncaptured}: {problems:?}"
        );
    }
    let choices: Vec<Choice> = ManualStatus::ALL.iter().map(|status| status.choice()).collect();
    assert_eq!(
        choices,
        [
            Choice::Automatic,
            Choice::Manual(ManualAvailability::Available),
            Choice::Manual(ManualAvailability::Busy),
            Choice::Manual(ManualAvailability::DoNotDisturb),
            Choice::Manual(ManualAvailability::Away),
        ]
    );
}

#[test]
fn presence_is_sent_and_the_first_run_waits_by_default_and_both_go_through_the_file() {
    let defaults = Settings::default();
    assert!(defaults.send_presence);
    assert_eq!(defaults.first_run, FirstRunState::Waiting);
    let (read, problems) = parse("send_presence = false\nfirst_run = \"confirmed\"").expect("TOML");
    assert!(!read.send_presence);
    assert_eq!(read.first_run, FirstRunState::Confirmed);
    assert!(problems.is_empty());
    let (read, problems) = parse("first_run = \"skipped\"").expect("TOML");
    assert_eq!(read.first_run, FirstRunState::Waiting);
    assert!(matches!(&problems[..], [Problem::BadValue { key: "first_run", .. }]));
}

#[gpui::test]
fn nothing_writes_on_its_own_until_the_first_run_is_confirmed(cx: &mut TestAppContext) {
    // the lab app doesn't ask: each switch says
    let lab = SettingsModel::in_memory(Settings::default());
    assert!(!lab.awaiting_first_run());
    assert!(lab.sends_typing() && lab.sends_read_receipts() && lab.sends_presence());
    // the personal build asks: nothing until it is confirmed, whatever the switches say
    let model = cx.new(|_| SettingsModel::in_memory(Settings::default()).requiring_first_run());
    cx.read(|cx| {
        let model = model.read(cx);
        assert!(model.awaiting_first_run());
        assert!(!model.sends_typing() && !model.sends_read_receipts() && !model.sends_presence());
    });
    model.update(cx, |model, cx| {
        model.change(|settings| settings.send_read_receipts = false, cx);
        model.confirm_first_run(cx);
    });
    cx.read(|cx| {
        let model = model.read(cx);
        assert!(!model.awaiting_first_run());
        assert!(model.sends_typing() && !model.sends_read_receipts() && model.sends_presence());
        assert_eq!(model.settings().first_run, FirstRunState::Confirmed);
    });
    // confirmed once, kept in the file
    let dir = tempfile::tempdir().expect("tempdir");
    let (file, _) = file_in(&dir);
    let saved = cx.new(|_| SettingsModel::load(file.clone()).requiring_first_run());
    saved.update(cx, SettingsModel::confirm_first_run);
    assert!(!SettingsModel::load(file).requiring_first_run().awaiting_first_run());
}
