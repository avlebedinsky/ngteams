//! `fence.log` written by several processes at once: each lab account runs a binary of its own (M1 Task 19), and every
//! one of them appends to the lab's one `fence.log`. Each append, and the newline that ends a half-written last line
//! when the log is opened, is made under an advisory lock on the file (`flock`), so the lines stay whole.
//!
//! The writer processes are this test binary itself, run again with [`CHILD_PATH`] set: [`child_writer`] then writes
//! its entries and exits; without it, it does nothing.
//!
//! The URLs here are made up, never a real endpoint's.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::Duration;

use serde_json::Value;
use teams_fence::{Channel, FenceLog, FrameRecord, RequestOutcome, RequestRecord};

/// The log a child process writes to; unset in a normal test run.
const CHILD_PATH: &str = "TEAMS_FENCE_LOG_CHILD_PATH";
/// The child's name, in each URL it logs.
const CHILD_NAME: &str = "TEAMS_FENCE_LOG_CHILD_NAME";
/// How many entries the child writes.
const CHILD_COUNT: &str = "TEAMS_FENCE_LOG_CHILD_COUNT";
/// How long each URL's path is: lines far past a pipe's atomic write size (4 KiB).
const PATH_LENGTH: usize = 64 * 1024;
/// What a child prints before it opens the log.
const OPENING: &str = "opening";
/// How long a test waits before it looks whether a child that should be blocked wrote anything.
const BLOCKED_FOR: Duration = Duration::from_millis(500);

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Not a test of its own: the body of a writer process ([`spawn_writer`]). Writes `CHILD_COUNT` entries, a request
/// and a frame in turn, each one line of over [`PATH_LENGTH`] bytes.
#[test]
fn child_writer() -> TestResult {
    let Some(path) = std::env::var_os(CHILD_PATH) else {
        return Ok(());
    };
    let name = std::env::var(CHILD_NAME)?;
    let count: usize = std::env::var(CHILD_COUNT)?.parse()?;
    println!("{OPENING}");
    std::io::stdout().flush()?;
    let log = FenceLog::open(Path::new(&path))?;
    let padding = "x".repeat(PATH_LENGTH);
    for index in 0..count {
        let url = format!("https://lab.invalid/{padding}/{name}/{index}");
        if index % 2 == 0 {
            log.record_request(&RequestRecord {
                channel: Channel::Http,
                method: "GET",
                url: &url,
                outcome: RequestOutcome::Own("lab_test"),
            })?;
        } else {
            log.record_frame(&FrameRecord {
                url: &url,
                kind: "trouter_ping",
            })?;
        }
    }
    Ok(())
}

/// This test binary, run as a writer process on `path`.
fn spawn_writer(path: &Path, name: &str, count: usize) -> std::io::Result<Child> {
    Command::new(std::env::current_exe()?)
        .args(["--exact", "child_writer", "--nocapture", "--test-threads=1", "--quiet"])
        .env(CHILD_PATH, path)
        .env(CHILD_NAME, name)
        .env(CHILD_COUNT, count.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
}

/// Waits until the child says it is opening the log (it is running, and about to take the lock). The rest of its
/// output is read by [`finish`], so the child never writes to a closed pipe.
fn wait_until_opening(child: &mut Child) -> Result<BufReader<ChildStdout>, Box<dyn std::error::Error>> {
    let stdout = child.stdout.take().ok_or("the child has no stdout")?;
    let mut reader = BufReader::new(stdout);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err("the child ended before it opened the log".into());
        }
        if line.trim() == OPENING {
            return Ok(reader);
        }
    }
}

/// Reads the rest of the child's output and waits for it to succeed.
fn finish(mut child: Child, mut output: BufReader<ChildStdout>) -> TestResult {
    std::io::copy(&mut output, &mut std::io::sink())?;
    assert!(child.wait()?.success(), "the writer succeeds");
    Ok(())
}

/// Each line's URL, `<name>/<index>` of the writer and entry it came from; fails on a line that isn't one whole entry.
fn entries(text: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut entries = Vec::new();
    for line in text.lines() {
        let entry: Value = serde_json::from_str(line).map_err(|error| format!("a line isn't whole: {error}"))?;
        let url = entry["url"].as_str().ok_or("an entry without a url")?;
        let tail = url.rsplitn(3, '/').take(2).collect::<Vec<_>>();
        let [index, name] = tail.as_slice() else {
            return Err(format!("an entry's url has no writer: {url}").into());
        };
        entries.push(format!("{name}/{index}"));
    }
    Ok(entries)
}

#[test]
fn appends_from_several_processes_at_once_stay_whole_lines() -> TestResult {
    const COUNT: usize = 60;
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("fence.log");
    let mut writers = Vec::new();
    for name in ["a", "b", "c"] {
        writers.push(spawn_writer(&path, name, COUNT)?);
    }
    for mut writer in writers {
        assert!(writer.wait()?.success(), "every writer finishes");
    }
    let text = fs::read_to_string(&path)?;
    assert!(text.ends_with('\n'), "the log ends with a whole line");
    let mut seen = entries(&text)?;
    assert_eq!(seen.len(), 3 * COUNT, "one line per entry, none lost or merged");
    seen.sort();
    let mut expected: Vec<String> = ["a", "b", "c"]
        .iter()
        .flat_map(|name| (0..COUNT).map(move |index| format!("{name}/{index}")))
        .collect();
    expected.sort();
    assert_eq!(seen, expected, "every entry of every writer, each once");
    Ok(())
}

#[test]
fn an_append_waits_while_another_process_holds_the_lock() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("fence.log");
    let held = OpenOptions::new().create(true).append(true).open(&path)?;
    held.lock()?;
    let mut writer = spawn_writer(&path, "a", 1)?;
    let output = wait_until_opening(&mut writer)?;
    std::thread::sleep(BLOCKED_FOR);
    assert_eq!(
        fs::metadata(&path)?.len(),
        0,
        "nothing is written while another holds the lock"
    );
    assert!(writer.try_wait()?.is_none(), "the writer waits for the lock");
    held.unlock()?;
    finish(writer, output)?;
    assert_eq!(entries(&fs::read_to_string(&path)?)?, ["a/0"]);
    Ok(())
}

#[test]
fn opening_ends_a_half_written_line_only_under_the_lock() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("fence.log");
    fs::write(&path, "{\"half")?;
    let held = OpenOptions::new().append(true).open(&path)?;
    held.lock()?;
    let mut writer = spawn_writer(&path, "b", 1)?;
    let output = wait_until_opening(&mut writer)?;
    std::thread::sleep(BLOCKED_FOR);
    assert_eq!(
        fs::read_to_string(&path)?,
        "{\"half",
        "the half line isn't touched while another holds the lock"
    );
    assert!(writer.try_wait()?.is_none(), "the open waits for the lock");
    held.unlock()?;
    finish(writer, output)?;
    let text = fs::read_to_string(&path)?;
    let (half, rest) = text.split_once('\n').ok_or("the half line is ended")?;
    assert_eq!(half, "{\"half");
    assert_eq!(entries(rest)?, ["b/0"], "the entry is a line of its own");
    Ok(())
}

#[test]
fn an_append_after_another_process_left_a_half_line_starts_a_line_of_its_own() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("fence.log");
    let log = FenceLog::open(&path)?;
    // another writer's append that failed halfway, after this log was opened
    OpenOptions::new().append(true).open(&path)?.write_all(b"{\"half")?;
    log.record_frame(&FrameRecord {
        url: "wss://lab.invalid/v4/c/c/0",
        kind: "trouter_ping",
    })?;
    let text = fs::read_to_string(&path)?;
    let (half, rest) = text.split_once('\n').ok_or("the half line is ended")?;
    assert_eq!(half, "{\"half");
    assert_eq!(entries(rest)?, ["c/0"], "the entry doesn't run into the half line");
    Ok(())
}
