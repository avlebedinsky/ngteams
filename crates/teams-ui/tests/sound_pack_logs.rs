//! Synthetic stored packs: absent events are quiet; promised sounds that cannot be read warn without local names.

use std::fs;
use std::os::unix::fs::{PermissionsExt as _, symlink};

use teams_media::devices::wav::WavWriter;
use teams_ui::sounds::{SoundEvent, pack};

#[test]
fn broken_and_unreadable_sounds_warn_without_the_pack_name_or_path() {
    let dir = tempfile::tempdir().unwrap();
    let logs = dir.path().join("logs");
    let sink = teams_log::install(&teams_log::Config {
        dir: logs.clone(),
        setting: teams_log::Level::Warn,
        env: None,
        rotation: teams_log::Rotation::default(),
    })
    .unwrap();
    let kept = dir.path().join("private-pack-name");
    fs::create_dir(&kept).unwrap();
    fs::write(
        kept.join("pack.toml"),
        "name = 'Private pack name'\n[sounds]\nmessage = 'message.wav'\n",
    )
    .unwrap();
    let path = kept.join("message.wav");
    let mut writer = WavWriter::create(&path).unwrap();
    writer.write(&[0.1; 480]).unwrap();
    writer.finish().unwrap();
    let read = |event| pack::read_sound(dir.path(), "private-pack-name", event);
    let logged = || {
        assert!(sink.flush_written());
        fs::read_to_string(logs.join(teams_log::FILE_NAME)).unwrap()
    };
    assert!(read(SoundEvent::Mention).is_none());
    assert!(logged().is_empty(), "an absent event is an ordinary partial pack");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    assert!(read(SoundEvent::Message).is_none());
    assert!(logged().contains("can't be opened"));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::remove_file(&path).unwrap();
    assert!(read(SoundEvent::Message).is_none());
    symlink(kept.join("pack.toml"), &path).unwrap();
    assert!(read(SoundEvent::Message).is_none());
    fs::remove_file(&path).unwrap();
    let file = fs::File::create(&path).unwrap();
    file.set_len(32 * 1024 * 1024).unwrap();
    assert!(read(SoundEvent::Message).is_none());
    fs::write(&path, b"damaged").unwrap();
    assert!(read(SoundEvent::Message).is_none());
    let text = logged();
    assert_eq!(text.lines().count(), 5);
    for private in ["private-pack-name", "Private pack name", dir.path().to_str().unwrap()] {
        assert!(!text.contains(private), "no pack or local path in a fallback log");
    }
}
