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
    DEFAULT_IDLE_MINUTES, DIR_MODE, FILE_MODE, ManualStatus, Notifications, Problem, Settings, SettingsFile,
    SettingsModel, parse, render,
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
        notifications: Notifications::Mentions,
        idle_minutes: 15,
        manual_status: ManualStatus::DoNotDisturb,
    }
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
    // the settings directory's place is taken by a file
    fs::write(dir.path().join("ui"), "not a directory").expect("written");
    let (file, _) = file_in(&dir);
    let model = cx.new(|_| SettingsModel::load(file));
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
        // the file couldn't be read either: its directory's place is a file
        assert!(matches!(model.problems()[0], Problem::Unreadable { .. }));
    });
    // a later failure replaces it: one at a time
    cx.update(|cx| model.update(cx, |model, cx| model.change(|settings| settings.idle_minutes = 60, cx)));
    assert_eq!(cx.read(|cx| unsaved(model.read(cx))), 1);
}
