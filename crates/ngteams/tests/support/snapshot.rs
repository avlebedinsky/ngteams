//! Snapshot helpers fail without replacing an existing destination or treating unreadable sources as absent.

use super::*;

#[test]
fn unreadable_sources_and_obstructed_destinations_report_the_exact_path() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let dest = root.path().join("dest");
    fs::write(&source, b"_synthetic new content").unwrap();
    fs::write(&dest, b"_synthetic retained content").unwrap();
    let error = copy_file(&source, &dest).unwrap_err();
    assert!(matches!(&error, SnapshotError::Io { path, .. } if path == &dest));
    assert!(error.to_string().contains(&dest.display().to_string()));
    assert_eq!(fs::read(&dest).unwrap(), b"_synthetic retained content");
    let unreadable = root.path().join("a-directory");
    fs::create_dir(&unreadable).unwrap();
    let missing = root.path().join("not-created");
    assert!(matches!(copy_file(&unreadable, &missing), Err(SnapshotError::Io { path, .. }) if path == unreadable));
    assert!(!missing.exists());
}

#[test]
fn filesystem_obstructions_fail_instead_of_becoming_empty_directories() {
    let root = tempfile::tempdir().unwrap();
    let dest = root.path().join("dest");
    fs::write(&dest, b"_synthetic obstruction").unwrap();
    let source = dest.clone();
    let child = dest.join("child");
    assert!(matches!(private_dir(&child), Err(SnapshotError::Io { path, .. }) if path == child));
    assert!(matches!(files_in(&source, "json"), Err(SnapshotError::Io { path, .. }) if path == source));
}

#[test]
fn a_snapshot_does_not_succeed_after_an_accounts_source_becomes_unreadable() {
    let root = tempfile::tempdir().unwrap();
    let paths = PersonalPaths::new(root.path().join("config"), root.path().join("data"));
    fs::create_dir_all(paths.accounts_file()).unwrap();
    let target = root.path().join("snapshot");
    let error = snapshot(&paths, &target).unwrap_err();
    assert!(matches!(error, SnapshotError::Io { path, .. } if path == paths.accounts_file()));
    assert!(!layout(&target).accounts_file().exists());
}

#[test]
fn source_inventory_keeps_only_regular_files_with_the_requested_extension_in_sorted_order() {
    let root = tempfile::tempdir().unwrap();
    for name in ["z.json", "a.json", "unrelated.txt", "no-extension"] {
        fs::write(root.path().join(name), b"_synthetic inventory").unwrap();
    }
    fs::create_dir(root.path().join("directory.json")).unwrap();
    let files = files_in(root.path(), "json").unwrap();
    assert_eq!(
        files.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(),
        ["a.json", "z.json"]
    );
    assert!(files.iter().all(|(name, path)| path == &root.path().join(name)));
}
