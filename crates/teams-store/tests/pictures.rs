//! The picture cache on a real file in a temporary directory: a picture kept across a reopen, found only by its key,
//! which is written only as its hash; one past its age dropped; one whose bytes changed dropped; the least recently
//! used let go past the limit; a file that isn't a cache made anew; a newer build's left alone; the file private. Every
//! key and byte here is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use teams_store::{PictureCache, StoreError};

const KEY: &str = "ams/0-eus-d1-00000000000000000000000000000001/imgo";
const OTHER: &str = "ams/0-eus-d1-00000000000000000000000000000002/imgo";

fn cache_path(dir: &tempfile::TempDir) -> PathBuf {
    PictureCache::file_beside(&dir.path().join("store").join("a.sqlite"))
}

fn picture(len: usize) -> Vec<u8> {
    vec![0x89; len]
}

fn put(cache: &mut PictureCache, key: &str, picture: &[u8]) -> Result<(), StoreError> {
    cache.put(key, picture)
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).expect("metadata").permissions().mode() & 0o777
}

#[test]
fn the_cache_lives_beside_the_store_in_a_directory_of_its_own() {
    assert_eq!(
        PictureCache::file_beside(Path::new("/data/store/a.sqlite")),
        Path::new("/data/store/a.sqlite-cache/pictures.sqlite"),
    );
}

#[test]
fn a_picture_is_kept_across_a_reopen_and_found_by_its_key_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = cache_path(&dir);
    let mut cache = PictureCache::open(&path, 1024).expect("open");
    assert_eq!(cache.get(KEY, None).expect("get"), None);
    put(&mut cache, KEY, &picture(10)).expect("put");
    assert_eq!(cache.get(KEY, None).expect("get"), Some(picture(10)));
    assert_eq!(cache.get(OTHER, None).expect("get"), None, "another key finds nothing");
    drop(cache);

    let cache = PictureCache::open(&path, 1024).expect("reopen");
    assert_eq!(
        cache.get(KEY, None).expect("get"),
        Some(picture(10)),
        "kept across runs"
    );
    assert_eq!(cache.bytes().expect("bytes"), 10);
    drop(cache);

    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(path.parent().expect("dir")), 0o700);
    let written = std::fs::read(&path).expect("read");
    assert!(
        !written.windows(KEY.len()).any(|window| window == KEY.as_bytes()),
        "the key is written only as its hash"
    );
}

#[test]
fn a_picture_put_again_replaces_the_one_kept() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cache = PictureCache::open(&cache_path(&dir), 1024).expect("open");
    put(&mut cache, KEY, &picture(10)).expect("put");
    let gif = b"GIF89a".to_vec();
    put(&mut cache, KEY, &gif).expect("put again");
    assert_eq!(cache.get(KEY, None).expect("get"), Some(gif));
    assert_eq!(cache.bytes().expect("bytes"), 6);
}

#[test]
fn a_picture_past_the_age_asked_for_is_dropped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cache = PictureCache::open(&cache_path(&dir), 1024).expect("open");
    put(&mut cache, KEY, &picture(10)).expect("put");
    assert_eq!(
        cache.get(KEY, Some(Duration::from_secs(3600))).expect("get"),
        Some(picture(10)),
        "young enough"
    );
    assert_eq!(cache.get(KEY, Some(Duration::ZERO)).expect("get"), None, "too old");
    assert_eq!(cache.get(KEY, None).expect("get"), None, "and dropped");
    assert_eq!(cache.bytes().expect("bytes"), 0);
}

#[test]
fn a_picture_whose_bytes_changed_is_dropped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = cache_path(&dir);
    let mut cache = PictureCache::open(&path, 1024).expect("open");
    put(&mut cache, KEY, &picture(10)).expect("put");
    put(&mut cache, OTHER, &picture(10)).expect("put");
    {
        let conn = rusqlite::Connection::open(&path).expect("raw open");
        // one picture's bytes changed in place, its length kept; the other's cut short
        conn.execute(
            "UPDATE pictures SET bytes = zeroblob(10) WHERE size = 10 AND id = 1",
            [],
        )
        .expect("changed");
        conn.execute("UPDATE pictures SET bytes = zeroblob(4) WHERE id = 2", [])
            .expect("cut");
    }
    assert_eq!(cache.get(KEY, None).expect("get"), None, "changed: dropped");
    assert_eq!(cache.get(OTHER, None).expect("get"), None, "cut short: dropped");
    assert_eq!(cache.bytes().expect("bytes"), 0, "both rows gone");
    put(&mut cache, KEY, &picture(10)).expect("put again");
    assert_eq!(cache.get(KEY, None).expect("get"), Some(picture(10)));
}

#[test]
fn the_least_recently_used_go_first_past_the_limit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cache = PictureCache::open(&cache_path(&dir), 100).expect("open");
    let keys = ["k/0", "k/1", "k/2"];
    for key in keys {
        put(&mut cache, key, &picture(40)).expect("put");
        // apart in time, so each is used after the one before
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(cache.get(keys[0], None).expect("get"), None, "the oldest let go");
    assert_eq!(cache.bytes().expect("bytes"), 80);

    // asking for the older one makes the newer one the least recently used
    std::thread::sleep(Duration::from_millis(5));
    assert!(cache.get(keys[1], None).expect("get").is_some());
    std::thread::sleep(Duration::from_millis(5));
    put(&mut cache, "k/3", &picture(40)).expect("put");
    assert!(cache.get(keys[1], None).expect("get").is_some(), "used lately, kept");
    assert_eq!(
        cache.get(keys[2], None).expect("get"),
        None,
        "least recently used, let go"
    );

    // one larger than the limit isn't kept at all
    put(&mut cache, "k/huge", &picture(101)).expect("put");
    assert_eq!(cache.get("k/huge", None).expect("get"), None);
    assert!(cache.bytes().expect("bytes") <= 100);
}

#[test]
fn a_file_that_isnt_a_cache_is_made_anew() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = cache_path(&dir);
    std::fs::create_dir_all(path.parent().expect("dir")).expect("dir");
    std::fs::write(&path, vec![0x5A; 8192]).expect("garbage");
    let mut cache = PictureCache::open(&path, 1024).expect("made anew");
    assert_eq!(cache.get(KEY, None).expect("get"), None);
    put(&mut cache, KEY, &picture(10)).expect("put");
    assert_eq!(cache.get(KEY, None).expect("get"), Some(picture(10)));
}

#[test]
fn a_newer_builds_cache_is_left_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = cache_path(&dir);
    let mut cache = PictureCache::open(&path, 1024).expect("open");
    put(&mut cache, KEY, &picture(10)).expect("put");
    drop(cache);
    {
        let conn = rusqlite::Connection::open(&path).expect("raw open");
        conn.pragma_update(None, "user_version", 99).expect("version");
    }
    let error = PictureCache::open(&path, 1024).expect_err("refused");
    assert!(
        matches!(
            error,
            StoreError::SchemaTooNew {
                found: 99,
                supported: 1
            }
        ),
        "{error}"
    );
    let conn = rusqlite::Connection::open(&path).expect("raw open");
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM pictures", [], |row| row.get(0))
        .expect("count");
    assert_eq!(rows, 1, "its pictures left as they were");
}
