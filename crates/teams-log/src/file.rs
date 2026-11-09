//! The log's file: private to the user, rotated by size, written from a thread of its own.
//!
//! - **Never waits on the file**: a line is handed to the writer thread through a bounded channel
//!   ([`CHANNEL_LINES`]); when the channel is full the line is dropped and counted ([`Sink::dropped`]), and the
//!   writer writes the count as a line of its own before its next line, so GPUI's foreground never waits on a disk.
//! - **Rotated by size** ([`Rotation`]): before a line would take the file past [`Rotation::max_bytes`], the file
//!   becomes `<name>.1` (`.1` becomes `.2`, …), the oldest past [`Rotation::keep`] is dropped, and a new file is
//!   started, each `0600`.
//! - **Private**: Unix directories are `0700` and files `0600`; Windows uses protected ACLs granting only the user
//!   and SYSTEM access. A file found open to others is tightened when it is opened.

use std::fs::{self, File};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::time::{Duration, SystemTime};

use teams_platform::private::{self, Open};

/// The files' mode.
pub const FILE_MODE: u32 = 0o600;
/// The directory's mode.
pub const DIR_MODE: u32 = 0o700;
/// How many lines may wait for the writer before new ones are dropped.
pub const CHANNEL_LINES: usize = 4096;
/// How long a flush waits for the writer.
pub const FLUSH_WAIT: Duration = Duration::from_secs(2);

/// When the file is rotated, and how many older files are kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rotation {
    pub max_bytes: u64,
    pub keep: usize,
}

impl Default for Rotation {
    /// 8 MiB, 4 older files.
    fn default() -> Self {
        Self {
            max_bytes: 8 * 1024 * 1024,
            keep: 4,
        }
    }
}

/// What the writer is handed.
enum Message {
    Line(String),
    /// Write what came before, then answer.
    Flush(SyncSender<()>),
}

/// The writer's side of [`Sink::open_paused`]: the writer starts once it is let go or dropped.
#[derive(Debug)]
pub struct Paused(Option<SyncSender<()>>);

impl Paused {
    /// Lets the writer start.
    pub fn resume(mut self) {
        if let Some(go) = self.0.take() {
            // a writer that is gone has nothing to start
            let _ = go.send(());
        }
    }
}

/// The log's file as the logger writes it (see the module docs).
#[derive(Debug)]
pub struct Sink {
    lines: SyncSender<Message>,
    dropped: Arc<AtomicU64>,
    path: PathBuf,
}

impl Sink {
    /// Opens (or creates) `<dir>/<name>` for appending, the directory `0700` and the file `0600`, and starts its
    /// writer thread.
    ///
    /// # Errors
    /// When the directory or the file can't be made, opened or made private, or the thread can't be started.
    pub fn open(dir: &Path, name: &str, rotation: Rotation) -> io::Result<Self> {
        let (sink, paused) = Self::open_paused(dir, name, rotation, CHANNEL_LINES)?;
        paused.resume();
        Ok(sink)
    }

    /// As [`Sink::open`], with room for `capacity` waiting lines, the writer held until [`Paused::resume`]: lines
    /// pushed before then wait in the channel, and are dropped (and counted) once it is full.
    ///
    /// # Errors
    /// As [`Sink::open`].
    pub fn open_paused(dir: &Path, name: &str, rotation: Rotation, capacity: usize) -> io::Result<(Self, Paused)> {
        let path = dir.join(name);
        let (file, size) = open_private(dir, &path)?;
        let (lines, received) = mpsc::sync_channel(capacity);
        let (go, wait) = mpsc::sync_channel(1);
        let dropped = Arc::new(AtomicU64::new(0));
        let mut writer = Writer {
            file,
            size,
            path: path.clone(),
            rotation,
            dropped: Arc::clone(&dropped),
            reported: 0,
        };
        std::thread::Builder::new()
            .name("teams-log".to_owned())
            .spawn(move || {
                // the sink dropped before it was let go: the writer still writes what waits
                let _ = wait.recv();
                writer.run(&received);
            })?;
        Ok((Self { lines, dropped, path }, Paused(Some(go))))
    }

    /// The file being written.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Hands `line` to the writer without waiting; a full channel (or a writer that is gone) drops it, counted.
    pub fn push(&self, line: String) {
        match self.lines.try_send(Message::Line(line)) {
            Ok(()) => {}
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Hands `line` to the writer, waiting for room up to [`FLUSH_WAIT`] (a panic's lines, which must not be lost to a
    /// burst); dropped and counted after that.
    pub fn push_waiting(&self, line: String) {
        let mut message = Message::Line(line);
        let until = std::time::Instant::now() + FLUSH_WAIT;
        loop {
            match self.lines.try_send(message) {
                Ok(()) => return,
                Err(TrySendError::Full(back)) if std::time::Instant::now() < until => {
                    message = back;
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                    self.dropped.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            }
        }
    }

    /// Lines dropped so far because the writer fell behind.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Waits up to [`FLUSH_WAIT`] for every line handed over so far to be written; whether it was.
    #[must_use]
    pub fn flush(&self) -> bool {
        let (done, flushed) = mpsc::sync_channel(1);
        let until = std::time::Instant::now() + FLUSH_WAIT;
        let mut message = Message::Flush(done);
        loop {
            match self.lines.try_send(message) {
                Ok(()) => break,
                Err(TrySendError::Full(back)) if std::time::Instant::now() < until => {
                    message = back;
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(_) => return false,
            }
        }
        let left = until.saturating_duration_since(std::time::Instant::now());
        !matches!(
            flushed.recv_timeout(left),
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected)
        )
    }
}

/// The directory made (or kept) private, and the file opened for appending, private, with its size.
fn open_private(dir: &Path, path: &Path) -> io::Result<(File, u64)> {
    private::create_dir_all(dir)?;
    private::restrict_dir(dir)?;
    let file = private::open(path, Open::Append)?;
    private::restrict_file(path)?;
    let size = file.metadata()?.len();
    Ok((file, size))
}

/// The writer thread's state.
struct Writer {
    file: File,
    size: u64,
    path: PathBuf,
    rotation: Rotation,
    dropped: Arc<AtomicU64>,
    /// Dropped lines already written as a count.
    reported: u64,
}

impl Writer {
    /// Writes what comes until the sink is gone; the count of the lines dropped is written once the lines that came
    /// before them are (when the channel runs empty, and before a flush is answered).
    fn run(&mut self, received: &Receiver<Message>) {
        loop {
            let message = match received.try_recv() {
                Ok(message) => message,
                Err(TryRecvError::Empty) => {
                    self.report_dropped();
                    match received.recv() {
                        Ok(message) => message,
                        Err(_) => return,
                    }
                }
                Err(TryRecvError::Disconnected) => return,
            };
            match message {
                Message::Line(line) => self.write(&line),
                Message::Flush(done) => {
                    self.report_dropped();
                    // a flush that fails leaves the lines where the system has them; nothing more to do here
                    let _ = self.file.flush();
                    let _ = done.send(());
                }
            }
        }
    }

    /// Writes the lines dropped since the last count, as a line of its own.
    fn report_dropped(&mut self) {
        let dropped = self.dropped.load(Ordering::Relaxed);
        if dropped > self.reported {
            let count = dropped - self.reported;
            self.reported = dropped;
            let line = crate::format_line(
                SystemTime::now(),
                log::Level::Warn,
                "teams_log",
                &format!("{count} log line(s) dropped: the writer fell behind"),
            );
            self.write(&line);
        }
    }

    fn write(&mut self, line: &str) {
        let length = line.len() as u64 + 1;
        if self.size > 0 && self.size + length > self.rotation.max_bytes {
            // a rotation that fails keeps writing to the file it has
            let _ = self.rotate();
        }
        let mut bytes = Vec::with_capacity(line.len() + 1);
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
        if self.file.write_all(&bytes).is_ok() {
            self.size += length;
        }
    }

    /// `<name>` becomes `<name>.1`, each older one moves up, the one past [`Rotation::keep`] is dropped, and a new
    /// file is started.
    ///
    /// The new file is made first (as `<name>.new`), so a failure to make it (no descriptors, no inodes) moves
    /// nothing; one that fails later puts `<name>` back where it can. `<name>` gone (taken away from outside) is made
    /// again without moving the older ones, so a rotation that keeps failing never shifts the archives over and over.
    fn rotate(&mut self) -> io::Result<()> {
        let suffixed = |suffix: &str| {
            let mut name = self.path.as_os_str().to_owned();
            name.push(suffix);
            PathBuf::from(name)
        };
        let fresh = suffixed(".new");
        let file = new_private(&fresh)?;
        let gone = matches!(fs::symlink_metadata(&self.path), Err(error) if error.kind() == io::ErrorKind::NotFound);
        let moved = if gone || self.rotation.keep == 0 {
            // started again in its place, nothing older moved (none kept: the old file dropped as the new one takes
            // its name)
            fs::rename(&fresh, &self.path)
        } else {
            self.shift(&fresh, suffixed)
        };
        if let Err(error) = moved {
            let _ = fs::remove_file(&fresh);
            return Err(error);
        }
        self.file = file;
        self.size = 0;
        Ok(())
    }

    /// Moves the older files up (the one past [`Rotation::keep`] dropped), `<name>` to `<name>.1` and `fresh` to
    /// `<name>`; `<name>.1` goes back to `<name>` when that last step fails.
    fn shift(&self, fresh: &Path, suffixed: impl Fn(&str) -> PathBuf) -> io::Result<()> {
        let numbered = |number: usize| suffixed(&format!(".{number}"));
        for number in (1..self.rotation.keep).rev() {
            match fs::rename(numbered(number), numbered(number + 1)) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                _ => {}
            }
        }
        fs::rename(&self.path, numbered(1))?;
        fs::rename(fresh, &self.path).inspect_err(|_| {
            // the writer keeps its file: under its own name again
            let _ = fs::rename(numbered(1), &self.path);
        })
    }
}

/// A new, empty, private file at `path` for appending (one left there by a rotation that stopped half way is
/// replaced).
fn new_private(path: &Path) -> io::Result<File> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    let file = private::open(path, Open::NewAppend)?;
    private::restrict_file(path)?;
    Ok(file)
}
