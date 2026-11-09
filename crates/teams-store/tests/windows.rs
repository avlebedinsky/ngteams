//! Windows storage permissions cover history, WAL/SHM, run locks, pictures and snapshots.
#![cfg(all(test, windows))]

#[path = "../../teams-platform/tests/support/privacy.rs"]
mod privacy;

#[test]
fn database_side_files_locks_pictures_and_snapshots_remain_private() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("data/store.sqlite");
    let store = teams_store::Store::open(&path).unwrap();
    privacy::assert_private(path.parent().unwrap(), false);
    for suffix in ["", "-wal", "-shm"] {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        // SQLite creates side files with the process token's default owner (Administrators in an elevated
        // process). Their inherited DACL must still grant only the current user and SYSTEM full access.
        privacy::assert_private(Path::new(&name), !suffix.is_empty());
    }
    let runs = root.path().join("data/store.sqlite-runs");
    privacy::assert_private(&runs, false);
    privacy::assert_private(&runs.join(format!("{}.lock", store.run_id())), false);
    let cache = teams_store::PictureCache::file_beside(&path);
    let _pictures = teams_store::PictureCache::open(&cache, 1024).unwrap();
    privacy::assert_private(&cache, false);
    let snapshot = root.path().join("snapshot.sqlite");
    teams_store::snapshot::snapshot(&path, &snapshot).unwrap();
    privacy::assert_private(&snapshot, false);
}

use std::path::Path;
