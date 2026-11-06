//! The settings and their file: defaults for a missing or unreadable file, a bad value reported with its default kept,
//! the file `0600` in a `0700` directory, written whole; the model writing each change at once. Every file here is in
//! a test's own temporary directory.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use gpui::{AppContext as _, TestAppContext};
use teams_media::devices::catalog::{DeviceDirectory as _, DeviceInfo, DeviceKind, DeviceList, FixedDirectory};
use teams_media::video::effects::{Backdrop, Background};
use teams_ui::settings::{
    AccountSettings, DEFAULT_IDLE_MINUTES, DIR_MODE, FILE_MODE, FirstRunState, LogLevel, ManualStatus, Notifications,
    Problem, Settings, SettingsFile, SettingsModel, TimeFormat, VideoEffect, attach_devices, parse, render,
};
use teams_ui::theme::{self, Appearance, THEMES};
use teams_ui::translate::{Backend, TranslatorChoice};

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
        video_calls: false,
        microphone: Some("alsa_input.usb-headset".to_owned()),
        speaker: None,
        camera: Some("v4l2_camera".to_owned()),
        video_effect: VideoEffect::Stock("waves"),
        appearance: Appearance {
            follow_system: false,
            theme: "nord",
            light_theme: "solarized-light",
            dark_theme: "high-contrast",
            font_size: 17,
            font_family: Some("Noto Sans".to_owned()),
            animations: false,
        },
        time_format: TimeFormat::Twelve,
        caption_translate_to: Some("de"),
        caption_translator: TranslatorChoice::Only(Backend::Codex),
        recap_summarizer: TranslatorChoice::Only(Backend::Claude),
        accounts: [(
            "8:orgid:00000000-0000-4000-8000-00000000000b".to_owned(),
            AccountSettings {
                selected_chat: Some(CHAT.to_owned()),
                manual_status: ManualStatus::Busy,
            },
        )]
        .into(),
        first_account: None,
    }
}

#[test]
fn the_video_effect_is_none_by_default_and_kept_by_its_key() {
    assert_eq!(Settings::default().video_effect, VideoEffect::None);
    for (text, effect) in [
        ("video_effect = \"blur\"", VideoEffect::Blur),
        ("video_effect = \"stock/forest\"", VideoEffect::Stock("forest")),
        (
            "video_effect = \"added/0a1b2c3d.jpg\"",
            VideoEffect::Added("0a1b2c3d.jpg".to_owned()),
        ),
        ("video_effect = \"none\"", VideoEffect::None),
    ] {
        let (read, problems) = parse(text).expect("TOML");
        assert_eq!(read.video_effect, effect, "{text}");
        assert!(problems.is_empty(), "{text}");
        assert_eq!(
            parse(&render(&read)).expect("TOML").0.video_effect,
            effect,
            "{text} round-trips"
        );
    }
    for bad in [
        "stock/nothing",
        "added/../settings.toml",
        "added/.hidden",
        "sparkles",
        "added/a/b.jpg",
    ] {
        let (read, problems) = parse(&format!("video_effect = \"{bad}\"")).expect("TOML");
        assert_eq!(read.video_effect, VideoEffect::None, "{bad} keeps the default");
        assert_eq!(problems.len(), 1, "{bad} is reported");
    }
}

#[test]
fn a_picture_the_user_added_is_looked_for_beside_the_settings() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (file, path) = file_in(&dir);
    let added = Settings {
        video_effect: VideoEffect::Added("0a1b.jpg".to_owned()),
        ..Settings::default()
    };
    file.save(&added).expect("saved");
    let model = SettingsModel::load(file);
    let backgrounds = path.parent().expect("a directory").join("backgrounds");
    assert_eq!(model.backgrounds_dir(), Some(backgrounds.clone()));
    assert_eq!(
        model.background(),
        Background::Picture(Backdrop::File(backgrounds.join("0a1b.jpg")))
    );
    let in_memory = SettingsModel::in_memory(added);
    assert_eq!(in_memory.backgrounds_dir(), None);
    assert_eq!(
        in_memory.background(),
        Background::Blur,
        "nothing to look in: blurred, never as it is"
    );
}

#[test]
fn the_calls_video_is_on_by_default_and_its_switch_shown_only_where_the_calls_have_it() {
    assert!(Settings::default().video_calls, "video on until the user turns it off");
    let (read, problems) = parse("video_calls = false").expect("TOML");
    assert!(!read.video_calls);
    assert!(problems.is_empty());
    let (read, problems) = parse("video_calls = \"no\"").expect("TOML");
    assert!(read.video_calls, "a bad value keeps the default");
    assert_eq!(problems.len(), 1);
    assert!(render(&read).contains("video_calls = true"));
    assert!(!SettingsModel::in_memory(Settings::default()).shows_video_switch());
    assert!(
        SettingsModel::in_memory(Settings::default())
            .with_video_switch()
            .shows_video_switch()
    );
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

#[test]
fn the_calls_devices_follow_the_system_by_default_and_a_chosen_one_is_kept_by_name() {
    let defaults = Settings::default();
    for kind in DeviceKind::ALL {
        assert_eq!(defaults.device(kind), None, "{kind:?} follows the system");
    }
    let text = render(&defaults);
    for key in ["microphone", "speaker", "camera"] {
        assert!(!text.contains(key), "nothing written for the system's: {text}");
    }
    let (read, problems) =
        parse("microphone = \"alsa_input.usb-Headset-00.mono-fallback\"\ncamera = \"v4l2 cam\"").expect("TOML");
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(
        read.device(DeviceKind::Microphone),
        Some("alsa_input.usb-Headset-00.mono-fallback")
    );
    assert_eq!(read.device(DeviceKind::Speaker), None);
    assert_eq!(read.device(DeviceKind::Camera), Some("v4l2 cam"));
    assert_eq!(parse(&render(&read)).expect("TOML").0, read, "read back as written");

    for bad in ["speaker = 3", "speaker = \"\"", "speaker = \"a\\nb\""] {
        let (read, problems) = parse(bad).expect("TOML");
        assert_eq!(read.speaker, None, "{bad}: the system's kept");
        assert_eq!(
            problems,
            [Problem::BadValue {
                key: "speaker",
                why: "not a device's name"
            }],
            "{bad}"
        );
    }
}

fn device(kind: DeviceKind, name: &str, description: &str) -> DeviceInfo {
    DeviceInfo {
        kind,
        name: name.to_owned(),
        description: description.to_owned(),
    }
}

#[gpui::test]
fn attached_devices_take_each_choice_at_once_and_on_every_change_and_their_list_follows(cx: &mut TestAppContext) {
    let directory = FixedDirectory::new(DeviceList::new(vec![device(
        DeviceKind::Microphone,
        "mic",
        "Microphone",
    )]));
    let settings = Settings {
        camera: Some("cam".to_owned()),
        ..Settings::default()
    };
    let model = cx.new(|_| SettingsModel::in_memory(settings));
    assert!(
        model.read_with(cx, |model, _| model.devices().is_none()),
        "no devices before they are attached"
    );
    cx.update(|cx| attach_devices(&model, std::sync::Arc::new(directory.clone()), cx));
    assert_eq!(
        directory.choices_made(),
        [
            (DeviceKind::Microphone, None),
            (DeviceKind::Speaker, None),
            (DeviceKind::Camera, Some("cam".to_owned()))
        ],
        "every kind's choice handed over at once"
    );
    assert_eq!(
        model.read_with(cx, |model, _| model.devices().map(|list| list.all().len())),
        Some(1)
    );

    cx.update(|cx| {
        model.update(cx, |model, cx| {
            model.change(
                |settings| settings.set_device(DeviceKind::Microphone, Some("mic".to_owned())),
                cx,
            );
        });
    });
    assert_eq!(directory.chosen(DeviceKind::Microphone).as_deref(), Some("mic"));
    let made = directory.choices_made().len();
    cx.update(|cx| {
        model.update(cx, |model, cx| {
            model.change(|settings| settings.send_typing = false, cx);
        });
    });
    assert_eq!(
        directory.choices_made().len(),
        made,
        "a change of another setting moves no device"
    );

    // a device plugged in: the list the settings show follows
    directory.set_devices(DeviceList::new(vec![
        device(DeviceKind::Microphone, "mic", "Microphone"),
        device(DeviceKind::Speaker, "usb", "USB headset"),
    ]));
    cx.run_until_parked();
    assert_eq!(
        model.read_with(cx, |model, _| model.devices().map(|list| list.all().len())),
        Some(2)
    );
}

#[test]
fn each_account_keeps_its_own_chat_and_status_and_the_first_one_the_top_level_ones() {
    const FIRST: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
    const OTHER: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
    // a file of one account: its values are the first account's
    let (mut settings, problems) = parse("selected_chat = \"19:first@thread.v2\"\nmanual_status = \"busy\"\n").unwrap();
    assert_eq!(problems, []);
    settings.make_first(FIRST);
    assert_eq!(settings.selected_chat_of(FIRST), Some("19:first@thread.v2"));
    assert_eq!(settings.manual_status_of(FIRST), ManualStatus::Busy);
    assert_eq!(settings.selected_chat_of(OTHER), None);
    assert_eq!(settings.manual_status_of(OTHER), ManualStatus::default());
    settings.set_selected_chat(OTHER, Some("19:other@thread.v2".to_owned()));
    settings.set_manual_status(OTHER, ManualStatus::DoNotDisturb);
    assert_eq!(settings.selected_chat, Some("19:first@thread.v2".to_owned()));

    // written and read back: the other account's under its MRI
    let text = render(&settings);
    let (mut read, problems) = parse(&text).unwrap();
    assert_eq!(problems, []);
    assert_eq!(read, settings);
    read.make_first(FIRST);
    assert_eq!(read, settings);
    assert_eq!(read.selected_chat_of(OTHER), Some("19:other@thread.v2"));
    assert_eq!(read.manual_status_of(OTHER), ManualStatus::DoNotDisturb);
}

#[test]
fn another_account_made_first_trades_places_with_the_first() {
    const FIRST: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
    const OTHER: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
    let (mut read, problems) = parse(
        "selected_chat = \"19:first@thread.v2\"\nmanual_status = \"busy\"\n\
         first_account = \"8:orgid:00000000-0000-4000-8000-00000000000a\"\n\
         [accounts.\"8:orgid:00000000-0000-4000-8000-00000000000b\"]\nselected_chat = \"19:other@thread.v2\"\n",
    )
    .unwrap();
    assert_eq!(problems, []);
    assert_eq!(read.first_account.as_deref(), Some(FIRST));
    read.make_first(OTHER);
    assert_eq!(read.first_account.as_deref(), Some(OTHER));
    assert_eq!(read.selected_chat, Some("19:other@thread.v2".to_owned()));
    assert_eq!(read.selected_chat_of(FIRST), Some("19:first@thread.v2"));
    assert_eq!(read.manual_status_of(FIRST), ManualStatus::Busy);
}

#[test]
fn an_account_s_table_that_is_bad_is_reported_and_left_out() {
    let (bad, problems) = parse("[accounts.\"8:orgid:x\"]\nmanual_status = \"sleeping\"\n").unwrap();
    assert!(bad.accounts.is_empty());
    assert_eq!(problems.len(), 1);
}

#[test]
fn an_account_removed_leaves_nothing_of_its_own_and_the_next_takes_the_first_s_place() {
    const FIRST: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
    const OTHER: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
    let mut settings = Settings::default();
    settings.make_first(FIRST);
    settings.set_selected_chat(FIRST, Some("19:first@thread.v2".to_owned()));
    settings.set_manual_status(FIRST, ManualStatus::Busy);
    settings.set_selected_chat(OTHER, Some("19:other@thread.v2".to_owned()));
    settings.set_manual_status(OTHER, ManualStatus::DoNotDisturb);
    // the other one removed: its table goes
    let mut removed = settings.clone();
    removed.forget_account(OTHER, None);
    assert!(removed.accounts.is_empty());
    assert_eq!(removed.selected_chat_of(FIRST), Some("19:first@thread.v2"));
    // the first one removed: the other takes the top level, and nothing of the first stays
    settings.forget_account(FIRST, Some(OTHER));
    assert_eq!(settings.first_account.as_deref(), Some(OTHER));
    assert_eq!(settings.selected_chat.as_deref(), Some("19:other@thread.v2"));
    assert_eq!(settings.manual_status, ManualStatus::DoNotDisturb);
    assert!(settings.accounts.is_empty());
    // the last one removed: the top level back to its defaults
    settings.forget_account(OTHER, None);
    assert_eq!((settings.selected_chat, settings.first_account), (None, None));
    assert_eq!(settings.manual_status, ManualStatus::default());
}

#[test]
fn the_theme_follows_the_system_by_default_and_every_choice_of_how_the_app_looks_goes_through_the_file() {
    let defaults = Settings::default().appearance;
    assert!(defaults.follow_system);
    assert_eq!(
        (defaults.light_theme, defaults.dark_theme),
        (theme::LIGHT_KEY, theme::DARK_KEY)
    );
    assert_eq!(defaults.font_size, theme::FONT_SIZE);
    assert_eq!(defaults.font_family, None, "the system's UI font");
    assert!(
        !render(&Settings::default()).contains("font_family"),
        "nothing written for the system's"
    );

    for spec in &THEMES {
        for follow_system in [false, true] {
            let settings = Settings {
                appearance: Appearance {
                    follow_system,
                    theme: spec.key,
                    light_theme: spec.key,
                    dark_theme: spec.key,
                    font_size: *theme::FONT_SIZES.end(),
                    font_family: Some("DejaVu Sans".to_owned()),
                    animations: !follow_system,
                },
                ..Settings::default()
            };
            let (read, problems) = parse(&render(&settings)).expect("TOML");
            assert_eq!(read, settings);
            assert!(problems.is_empty(), "{problems:?}");
        }
    }
}

#[test]
fn the_time_format_follows_the_system_by_default_and_each_choice_goes_through_the_file() {
    assert_eq!(Settings::default().time_format, TimeFormat::System);
    for format in TimeFormat::ALL {
        let settings = Settings {
            time_format: format,
            ..Settings::default()
        };
        let text = render(&settings);
        assert!(text.contains(&format!("time_format = \"{}\"", format.key())), "{text}");
        let (read, problems) = parse(&text).expect("TOML");
        assert_eq!(read, settings);
        assert!(problems.is_empty(), "{problems:?}");
    }
    for bad in ["time_format = \"am-pm\"", "time_format = 12", "time_format = \"12h\""] {
        let (read, problems) = parse(bad).expect("TOML");
        assert_eq!(read, Settings::default(), "{bad}");
        assert!(
            matches!(&problems[..], [Problem::BadValue { key: "time_format", .. }]),
            "{bad}: {problems:?}"
        );
    }
}

#[test]
fn caption_translation_is_off_by_default_and_its_language_and_tool_go_through_the_file() {
    let defaults = Settings::default();
    assert_eq!(defaults.caption_translate_to, None);
    assert_eq!(defaults.caption_translator, TranslatorChoice::Automatic);
    assert!(!render(&defaults).contains("caption_translate_to"));
    for choice in TranslatorChoice::ALL {
        let settings = Settings {
            caption_translate_to: Some("pt-br"),
            caption_translator: choice,
            ..Settings::default()
        };
        let text = render(&settings);
        assert!(text.contains("caption_translate_to = \"pt-br\""), "{text}");
        let (read, problems) = parse(&text).expect("TOML");
        assert_eq!(read, settings);
        assert!(problems.is_empty(), "{problems:?}");
    }
    for (bad, key) in [
        ("caption_translate_to = \"klingon\"", "caption_translate_to"),
        ("caption_translate_to = 3", "caption_translate_to"),
        ("caption_translator = \"luna\"", "caption_translator"),
    ] {
        let (read, problems) = parse(bad).expect("TOML");
        assert_eq!(read, Settings::default(), "{bad}");
        assert!(
            matches!(&problems[..], [Problem::BadValue { key: found, .. }] if *found == key),
            "{bad}: {problems:?}"
        );
    }
}

#[test]
fn the_recap_summarizer_is_automatic_by_default_and_goes_through_the_file() {
    assert_eq!(Settings::default().recap_summarizer, TranslatorChoice::Automatic);
    for choice in TranslatorChoice::ALL {
        let settings = Settings {
            recap_summarizer: choice,
            ..Settings::default()
        };
        let text = render(&settings);
        assert!(
            text.contains(&format!("recap_summarizer = \"{}\"", choice.key())),
            "{text}"
        );
        let (read, problems) = parse(&text).expect("TOML");
        assert_eq!(read, settings);
        assert!(problems.is_empty(), "{problems:?}");
    }
    let (read, problems) = parse("recap_summarizer = \"opus\"").expect("TOML");
    assert_eq!(read, Settings::default());
    assert!(
        matches!(
            &problems[..],
            [Problem::BadValue {
                key: "recap_summarizer",
                ..
            }]
        ),
        "{problems:?}"
    );
}

#[test]
fn a_theme_this_app_doesn_t_know_or_a_font_size_out_of_range_keeps_its_default() {
    for (bad, key) in [
        ("theme = \"purple\"", "theme"),
        ("light_theme = \"Teams Light\"", "light_theme"),
        ("dark_theme = 3", "dark_theme"),
        ("follow_system_theme = \"yes\"", "follow_system_theme"),
        ("font_size = 9", "font_size"),
        ("font_size = 25", "font_size"),
        ("font_size = 14.5", "font_size"),
        ("font_family = \"\"", "font_family"),
        ("font_family = \"a\\nb\"", "font_family"),
        ("animations = \"off\"", "animations"),
    ] {
        let (read, problems) = parse(bad).expect("TOML");
        assert_eq!(read, Settings::default(), "{bad}");
        assert!(
            matches!(&problems[..], [Problem::BadValue { key: found, .. }] if *found == key),
            "{bad}: {problems:?}"
        );
    }
}

#[test]
fn the_font_size_steps_within_its_range() {
    let mut appearance = Appearance::default();
    assert_eq!(appearance.stepped_font_size(1), theme::FONT_SIZE + 1);
    appearance.font_size = *theme::FONT_SIZES.end();
    assert_eq!(appearance.stepped_font_size(1), *theme::FONT_SIZES.end());
    appearance.font_size = *theme::FONT_SIZES.start();
    assert_eq!(appearance.stepped_font_size(-1), *theme::FONT_SIZES.start());
}
