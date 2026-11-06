//! A meeting recording's download, its stream loop over the fake engine (`common`): the bytes read stream after
//! stream into the file, a stream that broke off opened again where the file stands, the recording refused, the
//! network failing again and again, and a file that can't be written; the sizes and words the recap shows. Every link
//! and byte here is made up.

#![cfg(test)]

#[expect(dead_code, reason = "the shared fake engine of the view tests: these use part of it")]
mod common;

use std::cell::RefCell;
use std::future;
use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::FakeCore;
use teams_ui::recording_download::{Failure, Progress, RETRIES, RETRY_AFTER, byte_size, fetch, part_path};

const LINK: &str = "https://madeup-my.sharepoint.com/:v:/g/personal/lab_a/MadeUpRecording";

fn locked<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn block_on<T>(future: impl Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(future)
}

fn core_with(bytes: &[u8], limit: Option<usize>) -> Arc<FakeCore> {
    let core = Arc::new(FakeCore::default());
    locked(&core.file_bytes).insert(LINK.to_owned(), bytes.to_vec());
    *locked(&core.stream_limit) = limit;
    core
}

fn asked(core: &FakeCore, what: &str) -> usize {
    locked(&core.asked).iter().filter(|asked| **asked == what).count()
}

/// What `fetch` did: its result, the bytes written, every progress told and every wait.
struct Fetched {
    result: Result<(), Failure>,
    written: Vec<u8>,
    told: Vec<(u64, u64)>,
    waits: Vec<Duration>,
}

fn fetched(core: &FakeCore) -> Fetched {
    let waits = RefCell::new(Vec::new());
    let mut told = Vec::new();
    let mut written = Vec::new();
    let result = block_on(fetch(
        core,
        LINK,
        &mut written,
        |wait| {
            waits.borrow_mut().push(wait);
            future::ready(())
        },
        |at, total| told.push((at, total)),
    ));
    Fetched {
        result,
        written,
        told,
        waits: waits.into_inner(),
    }
}

#[test]
fn the_recording_is_read_whole_stream_after_stream() {
    let bytes: Vec<u8> = (0..100_000_u32).map(|at| u8::try_from(at % 253).unwrap()).collect();
    let core = core_with(&bytes, None);
    let done = fetched(&core);
    assert_eq!(done.result, Ok(()));
    assert_eq!(done.written, bytes);
    assert_eq!(asked(&core, "file_stream"), 1);
    assert_eq!(done.told.last(), Some(&(100_000, 100_000)));
    assert!(done.waits.is_empty());
}

#[test]
fn a_stream_that_broke_off_is_opened_again_where_the_file_stands() {
    let bytes: Vec<u8> = (0..100_000_u32).map(|at| u8::try_from(at % 241).unwrap()).collect();
    let core = core_with(&bytes, Some(30_000));
    let done = fetched(&core);
    assert_eq!(done.result, Ok(()));
    assert_eq!(done.written, bytes, "every byte once, in order");
    assert_eq!(asked(&core, "file_stream"), 4);
    assert!(
        done.waits.is_empty(),
        "a stream that brought something is opened again at once"
    );
    assert!(done.told.windows(2).all(|pair| pair[0].0 < pair[1].0));
}

#[test]
fn a_refused_recording_fails_at_once() {
    let core = Arc::new(FakeCore::default());
    let done = fetched(&core);
    assert_eq!(done.result, Err(Failure::Refused));
    assert_eq!(asked(&core, "file_stream"), 1);
}

#[test]
fn streams_that_bring_nothing_fail_after_waiting_longer_each_time() {
    let core = core_with(b"made-up recording", Some(0));
    let done = fetched(&core);
    assert_eq!(done.result, Err(Failure::Network));
    assert_eq!(asked(&core, "file_stream"), RETRIES as usize + 1);
    assert_eq!(
        done.waits,
        vec![RETRY_AFTER, RETRY_AFTER * 2, RETRY_AFTER * 4, RETRY_AFTER * 8]
    );
}

/// A file that refuses every write.
struct Full;

impl Write for Full {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("no space left"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_file_that_can_t_be_written_fails_with_its_words() {
    let core = core_with(b"made-up recording", None);
    let result = block_on(fetch(core.as_ref(), LINK, &mut Full, |_| future::ready(()), |_, _| {}));
    assert_eq!(result, Err(Failure::Disk("no space left".to_owned())));
}

#[test]
fn sizes_read_as_the_recap_shows_them() {
    for (bytes, said) in [
        (0, "0 B"),
        (1023, "1023 B"),
        (1024, "1 KB"),
        (1500, "2 KB"),
        (4_659_019, "4.4 MB"),
        (3 * 1024 * 1024 * 1024 / 2, "1.5 GB"),
    ] {
        assert_eq!(byte_size(bytes), said);
    }
}

#[test]
fn progress_and_words_read_as_the_recap_shows_them() {
    let running = |written, total| Progress::Running { written, total };
    assert_eq!(running(50, Some(200)).fraction(), Some(0.25));
    assert_eq!(running(50, None).fraction(), None);
    assert_eq!(running(300, Some(200)).fraction(), Some(1.0));
    assert_eq!(running(0, Some(0)).fraction(), None);
    assert!(running(0, None).running());
    assert!(!Progress::Saved.running());
    assert_eq!(Progress::Saved.fraction(), None);
    assert_eq!(
        part_path(Path::new("/downloads/Weekly sync.mp4")),
        Path::new("/downloads/Weekly sync.mp4.part")
    );
    for failure in [Failure::Refused, Failure::Network, Failure::Cancelled] {
        assert!(failure.words().ends_with('.'), "{failure:?}");
    }
    assert_eq!(
        Failure::Disk("disk full".to_owned()).words(),
        "The file couldn't be saved: disk full"
    );
    assert_eq!(teams_ui::recap::recording_file_name("Weekly/sync "), "Weekly-sync.mp4");
    assert_eq!(teams_ui::recap::recording_file_name("  "), "Recording.mp4");
}
