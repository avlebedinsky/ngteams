//! A meeting recording downloaded to a file: the recap's More options → Download, offered only when the recording's
//! item says the account may download it (Teams' Stream page, which the recap's Watch in browser opens, offers its
//! Download the same way).
//!
//! - **The bytes** are read by the engine at the download link Graph hands out for the recording's sharing link
//!   ([`Core::file_stream`], as a chat's video is played), [`CHUNK`] at a time, each written as it arrives. A link that
//!   expired is read again by the engine; a stream that broke off is opened again where the file stands, a few times
//!   ([`RETRIES`]) while nothing more arrives. A recording refused is not asked again.
//! - **The file** is written beside its place as `<name>.part`, then renamed over it once whole; a download that failed
//!   or was cancelled leaves nothing behind.
//! - **It runs apart from the view**: the downloads are the app's ([`Downloads`]), so leaving the recap or the chat
//!   doesn't stop one, and the recap shows the ones of its recordings again when it is shown. Writing happens off the
//!   UI thread; the recording keeps playing meanwhile.

use std::collections::HashMap;
use std::fs;
use std::future::Future;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use gpui::{App, AppContext as _, Context, Entity, Global, Task};
use teams_core::EngineError;

use crate::bridge::Core;

/// The most bytes one stream asks for: the engine's longest ([`teams_proto::MAX_STREAM`]).
pub const CHUNK: u64 = teams_proto::MAX_STREAM;

/// How many times running a stream that brought nothing is opened again before the download fails.
pub const RETRIES: u32 = 4;

/// How long the download waits before opening a stream again, by the attempt (doubled each time, from this).
pub const RETRY_AFTER: Duration = Duration::from_secs(1);

/// How many bytes the view is told of at a time (and once the whole file came).
const TELL_EVERY: u64 = 256 * 1024;

/// The buffer between the stream and the file.
const WRITE_BUFFER: usize = 1024 * 1024;

/// Why a download failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The recording was refused (no longer there, or no longer the account's to download).
    Refused,
    /// The network failed on the way, again and again.
    Network,
    /// The file couldn't be written (its words).
    Disk(String),
    /// The user cancelled it.
    Cancelled,
}

impl Failure {
    /// What the recap says of it, under "Couldn't download <name>": why, and what to do.
    #[must_use]
    pub fn words(&self) -> String {
        match self {
            Self::Refused => "You may no longer have access to the recording.".to_owned(),
            Self::Network => "Check your connection and try again.".to_owned(),
            Self::Disk(error) => format!("The file couldn't be saved: {error}"),
            Self::Cancelled => "The download was cancelled.".to_owned(),
        }
    }
}

/// Where a download stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Progress {
    /// Running: the bytes written so far and the file's size, once the first stream told it.
    Running {
        written: u64,
        total: Option<u64>,
    },
    Saved,
    Failed(Failure),
}

impl Progress {
    /// Whether it still runs.
    #[must_use]
    pub const fn running(&self) -> bool {
        matches!(self, Self::Running { .. })
    }

    /// How much of the file is written, from 0 to 1, once its size is known.
    #[must_use]
    pub fn fraction(&self) -> Option<f32> {
        match *self {
            Self::Running {
                written,
                total: Some(total),
            } if total > 0 => {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "a fraction of a progress bar: a file's size is far under f64's exact range"
                )]
                let fraction = (written as f64 / total as f64).clamp(0.0, 1.0);
                #[expect(clippy::cast_possible_truncation, reason = "a fraction within 0..=1")]
                Some(fraction as f32)
            }
            _ => None,
        }
    }
}

/// One recording's download (see the module docs).
pub struct RecordingDownload {
    core: Arc<dyn Core>,
    /// The recording's sharing link.
    link: String,
    /// Where it is saved.
    target: PathBuf,
    progress: Progress,
    running: Option<Task<()>>,
}

impl RecordingDownload {
    /// The recording's sharing link.
    #[must_use]
    pub fn link(&self) -> &str {
        &self.link
    }

    /// Where it is saved.
    #[must_use]
    pub fn target(&self) -> &Path {
        &self.target
    }

    /// The file's name, as the recap says it.
    #[must_use]
    pub fn name(&self) -> String {
        self.target.file_name().map_or_else(
            || self.target.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
    }

    #[must_use]
    pub const fn progress(&self) -> &Progress {
        &self.progress
    }

    /// Starts (or, after a failure, starts again) writing the recording to its file.
    pub fn start(&mut self, cx: &mut Context<Self>) {
        self.progress = Progress::Running {
            written: 0,
            total: None,
        };
        // the latest progress, and a wake for the view when it moved on (wakes not taken yet fold into one)
        let latest = Arc::new(Mutex::new((0, 0)));
        let (wake, woken) = async_channel::bounded::<()>(1);
        let executor = cx.background_executor().clone();
        let job = cx.background_spawn(save(
            Arc::clone(&self.core),
            self.link.clone(),
            self.target.clone(),
            Told {
                latest: Arc::clone(&latest),
                wake,
            },
            move |wait| executor.timer(wait),
        ));
        self.running = Some(cx.spawn(async move |this, cx| {
            while woken.recv().await.is_ok() {
                let (written, total) = *latest.lock().unwrap_or_else(PoisonError::into_inner);
                let told = this.update(cx, |this, cx| {
                    this.progress = Progress::Running {
                        written,
                        total: Some(total),
                    };
                    cx.notify();
                });
                if told.is_err() {
                    return;
                }
            }
            let saved = job.await;
            let _ = this.update(cx, |this, cx| {
                this.progress = match saved {
                    Ok(()) => Progress::Saved,
                    Err(failure) => Progress::Failed(failure),
                };
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Stops a running download; what it wrote is removed.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if !self.progress.running() {
            return;
        }
        // dropping the task drops its stream (the engine stops reading) and its file
        self.running = None;
        self.progress = Progress::Failed(Failure::Cancelled);
        let _ = fs::remove_file(part_path(&self.target));
        cx.notify();
    }
}

/// The app's recording downloads, by the recording's sharing link: the last one of each.
#[derive(Default)]
pub struct Downloads(HashMap<String, Entity<RecordingDownload>>);

impl Global for Downloads {}

/// The download of the recording whose sharing link is `link`, when one was started this run (and not dismissed).
#[must_use]
pub fn download_of(link: &str, cx: &App) -> Option<Entity<RecordingDownload>> {
    cx.try_global::<Downloads>()?.0.get(link).cloned()
}

/// Starts downloading the recording whose sharing link is `link` to `target`, in place of an earlier one of it that
/// ended (one still running is kept and handed back).
pub fn start(core: Arc<dyn Core>, link: String, target: PathBuf, cx: &mut App) -> Entity<RecordingDownload> {
    if let Some(running) = download_of(&link, cx).filter(|download| download.read(cx).progress.running()) {
        return running;
    }
    let download = cx.new(|cx| {
        let mut download = RecordingDownload {
            core,
            link: link.clone(),
            target,
            progress: Progress::Saved,
            running: None,
        };
        download.start(cx);
        download
    });
    cx.default_global::<Downloads>().0.insert(link, download.clone());
    download
}

/// Forgets the download of the recording whose sharing link is `link` (its note dismissed), cancelling it if it runs.
pub fn dismiss(link: &str, cx: &mut App) {
    let Some(download) = cx
        .try_global::<Downloads>()
        .and_then(|downloads| downloads.0.get(link).cloned())
    else {
        return;
    };
    download.update(cx, RecordingDownload::cancel);
    cx.default_global::<Downloads>().0.remove(link);
}

/// Where the file is written until it is whole: `<name>.part` beside it.
#[must_use]
pub fn part_path(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    target.with_file_name(name)
}

/// Where a running download tells the view how far it got: the latest bytes written and the file's size, and a wake.
struct Told {
    latest: Arc<Mutex<(u64, u64)>>,
    wake: async_channel::Sender<()>,
}

/// Writes the recording to `target` (through its [`part_path`]), telling `told` how far it got (every [`TELL_EVERY`]
/// bytes, and once the whole file came); what it wrote is removed when it fails.
async fn save<F: Future<Output = ()>>(
    core: Arc<dyn Core>,
    link: String,
    target: PathBuf,
    told: Told,
    sleep: impl FnMut(Duration) -> F,
) -> Result<(), Failure> {
    let part = part_path(&target);
    let disk = |error: io::Error| Failure::Disk(error.to_string());
    // the downloads folder may not be there yet
    if let Some(folder) = part.parent() {
        fs::create_dir_all(folder).map_err(disk)?;
    }
    let file = fs::File::create(&part).map_err(disk)?;
    let mut sink = BufWriter::with_capacity(WRITE_BUFFER, file);
    let mut said = 0;
    let fetched = fetch(core.as_ref(), &link, &mut sink, sleep, |written, total| {
        *told.latest.lock().unwrap_or_else(PoisonError::into_inner) = (written, total);
        if written.saturating_sub(said) >= TELL_EVERY || written >= total {
            said = written;
            // a wake the view hasn't taken yet already brings it the latest
            let _ = told.wake.try_send(());
        }
    })
    .await;
    drop(told);
    let saved = fetched.and_then(|()| {
        let file = sink.into_inner().map_err(|error| disk(error.into_error()))?;
        file.sync_all().map_err(disk)?;
        drop(file);
        fs::rename(&part, &target).map_err(disk)
    });
    if saved.is_err() {
        let _ = fs::remove_file(&part);
    }
    saved
}

/// Reads the recording whose sharing link is `link` into `sink` from its start, stream after stream ([`CHUNK`] each),
/// calling `told` with the bytes written and the file's size after each piece. A stream that broke off is opened again
/// where the file stands, after `sleep`ing a while ([`RETRY_AFTER`], doubled each time); [`RETRIES`] in a row that
/// bring nothing fail it ([`Failure::Network`]), as does the recording refused ([`Failure::Refused`]) or `sink`
/// refusing a write ([`Failure::Disk`]).
///
/// # Errors
/// Why it failed, as above.
pub async fn fetch<F: Future<Output = ()>>(
    core: &dyn Core,
    link: &str,
    sink: &mut impl Write,
    mut sleep: impl FnMut(Duration) -> F,
    mut told: impl FnMut(u64, u64),
) -> Result<(), Failure> {
    let mut written = 0_u64;
    let mut total: Option<u64> = None;
    let mut idle = 0_u32;
    while total.is_none_or(|total| written < total) {
        let before = written;
        match core.file_stream(link.to_owned(), written, CHUNK).await {
            Ok(mut stream) if stream.offset == written => {
                total = Some(stream.total);
                while let Some(piece) = stream.next().await {
                    // a piece that failed ends the stream: it is opened again below
                    let Ok(piece) = piece else { break };
                    sink.write_all(&piece)
                        .map_err(|error| Failure::Disk(error.to_string()))?;
                    written += piece.len() as u64;
                    told(written, stream.total);
                }
            }
            Ok(_) | Err(EngineError::FileUnavailable) => return Err(Failure::Refused),
            Err(_) => {}
        }
        if total.is_some_and(|total| written >= total) {
            break;
        }
        if written > before {
            idle = 0;
            continue;
        }
        idle += 1;
        if idle > RETRIES {
            return Err(Failure::Network);
        }
        sleep(RETRY_AFTER * 2_u32.pow(idle - 1)).await;
    }
    sink.flush().map_err(|error| Failure::Disk(error.to_string()))
}

/// A size as the recap says it: "512 KB", "4.4 MB", "1.2 GB".
#[must_use]
pub fn byte_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    let tenths = |unit: u64| {
        let tenths = bytes.saturating_mul(10) / unit;
        format!("{}.{}", tenths / 10, tenths % 10)
    };
    match bytes {
        bytes if bytes < KB => format!("{bytes} B"),
        bytes if bytes < MB => format!("{} KB", bytes.div_ceil(KB)),
        bytes if bytes < GB => format!("{} MB", tenths(MB)),
        _ => format!("{} GB", tenths(GB)),
    }
}
