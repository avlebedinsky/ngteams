//! People's names and pictures in the store (M2 Task 12), on a real file in a temporary directory: a row stored and
//! replaced whole, kept across a reopen, and a row in a form this build doesn't know read as never read. Every MRI and
//! name here is made up.

use std::path::PathBuf;

use teams_store::{NameSource, PictureState, Profile, Store, StoredPicture};

const PEER: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
const THIRD: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";

fn store_path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("store").join("a.sqlite")
}

#[test]
fn a_profile_is_stored_whole_and_replaced_whole() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = Store::open(&store_path(&dir)).expect("open");
    assert_eq!(store.profile(PEER).expect("read"), None);

    let picture = StoredPicture {
        media_type: "image/jpeg".to_owned(),
        bytes: vec![0xFF, 0xD8, 0xFF, 0xE0],
    };
    let first = Profile {
        mri: PEER.to_owned(),
        name: Some("Peer Name".to_owned()),
        name_source: Some(NameSource::Graph),
        picture: PictureState::Some(picture.clone()),
        read_at: 1_000,
    };
    store.put_profile(&first).expect("put");
    assert_eq!(store.profile(PEER).expect("read").as_ref(), Some(&first));
    assert_eq!(store.profile(THIRD).expect("read"), None, "one MRI's row only");

    // a later read that found no picture and only the lookup's name replaces it whole
    let second = Profile {
        name: Some("Peer From Lookup".to_owned()),
        name_source: Some(NameSource::ShortProfile),
        picture: PictureState::None,
        read_at: 2_000,
        ..first.clone()
    };
    store.put_profile(&second).expect("put again");
    assert_eq!(store.profile(PEER).expect("read").as_ref(), Some(&second));

    let unread = Profile {
        mri: THIRD.to_owned(),
        name: None,
        name_source: None,
        picture: PictureState::Unread,
        read_at: 0,
    };
    store.put_profile(&unread).expect("put unread");
    assert_eq!(store.profile(THIRD).expect("read").as_ref(), Some(&unread));

    // kept across a reopen; `Debug` shows neither the name nor the picture's bytes
    drop(store);
    let store = Store::open(&store_path(&dir)).expect("reopen");
    let read = store.profile(PEER).expect("read").expect("stored");
    assert_eq!(read, second);
    let shown = format!("{read:?} {picture:?}");
    assert!(!shown.contains("Peer From Lookup"), "{shown}");
    assert!(shown.contains("bytes: 4"), "{shown}");
}

#[test]
fn a_stored_profile_in_a_form_this_build_doesnt_know_is_read_as_never_read() {
    let dir = tempfile::tempdir().expect("tempdir");
    drop(Store::open(&store_path(&dir)).expect("open"));
    let raw = rusqlite::Connection::open(store_path(&dir)).expect("open directly");
    let insert = "INSERT INTO profiles (mri, name, name_source, picture_state, picture_type, picture, read_at)
                  VALUES (?1, ?2, ?3, ?4, NULL, NULL, 5)";
    raw.execute(insert, rusqlite::params![PEER, "Someone", "directory", "blurred"])
        .expect("insert");
    raw.execute(insert, rusqlite::params![THIRD, None::<String>, "graph", "some"])
        .expect("insert");
    drop(raw);
    let store = Store::open(&store_path(&dir)).expect("reopen");
    let odd = store.profile(PEER).expect("read").expect("stored");
    assert_eq!(odd.name_source, None);
    assert_eq!(odd.picture, PictureState::Unread);
    let no_bytes = store.profile(THIRD).expect("read").expect("stored");
    assert_eq!(no_bytes.name_source, Some(NameSource::Graph));
    assert_eq!(
        no_bytes.picture,
        PictureState::Unread,
        "a picture without its bytes isn't one"
    );
}
