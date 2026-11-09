//! Native Windows ACLs survive log rotation, including files kept open by the writer.
#![cfg(all(test, windows))]

#[path = "../../teams-platform/tests/support/privacy.rs"]
mod privacy;

#[test]
fn rotated_logs_and_their_directory_are_private() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("logs");
    let sink = teams_log::file::Sink::open(&dir, "app.log", teams_log::Rotation { max_bytes: 4, keep: 2 }).unwrap();
    for _ in 0..4 {
        sink.push("abc".into());
    }
    assert!(sink.flush());
    privacy::assert_private(&dir, false);
    for name in ["app.log", "app.log.1", "app.log.2"] {
        let path = dir.join(name);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "abc\n");
        privacy::assert_private(&path, false);
    }
}
