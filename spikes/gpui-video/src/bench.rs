//! The benchmark's statistics and the parsers its runner needs: percentiles and rates, which frames reached the
//! screen, GPUI's `frame duration` lines (`ZED_MEASUREMENTS=1`), the binary's `summary` line, per-thread CPU time
//! from `/proc/<pid>/task/<tid>/schedstat`, and GPU memory from DRM fdinfo and from `nvidia-smi`.

use std::{collections::HashMap, str::FromStr, time::Duration};

/// Percentiles (nearest rank), maximum and mean of a set of durations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stats {
    pub count: usize,
    pub p50: Duration,
    pub p95: Duration,
    pub max: Duration,
    pub mean: Duration,
}

impl Stats {
    /// The statistics of `samples`, in any order; `None` when there are none.
    #[must_use]
    pub fn of(samples: &[Duration]) -> Option<Self> {
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        let max = *sorted.last()?;
        let total: Duration = sorted.iter().sum();
        let count = sorted.len();
        Some(Self {
            count,
            p50: percentile(&sorted, 50),
            p95: percentile(&sorted, 95),
            max,
            mean: total / u32::try_from(count).unwrap_or(u32::MAX),
        })
    }
}

/// The nearest-rank `percent`th percentile of non-empty `sorted`.
fn percentile(sorted: &[Duration], percent: usize) -> Duration {
    let rank = (percent * sorted.len()).div_ceil(100).max(1);
    sorted[rank.min(sorted.len()) - 1]
}

/// Events per second; 0 over no time.
#[must_use]
pub fn per_second(count: usize, elapsed: Duration) -> f64 {
    let seconds = elapsed.as_secs_f64();
    if seconds > 0.0 {
        f64::from(u32::try_from(count).unwrap_or(u32::MAX)) / seconds
    } else {
        0.0
    }
}

/// CPU time `busy` over wall time `elapsed`, in percent of one core; 0 over no time.
#[must_use]
pub fn cpu_percent(busy: Duration, elapsed: Duration) -> f64 {
    let seconds = elapsed.as_secs_f64();
    if seconds > 0.0 {
        busy.as_secs_f64() * 100.0 / seconds
    } else {
        0.0
    }
}

/// Which video frames of each tile reached the screen: every new counter drawn is a shown frame, a jump over
/// counters counts the frames in between as skipped (replaced before a frame was drawn), and each shown frame's
/// latency (from the source handing it over to the draw that shows it) is kept.
#[derive(Debug, Clone)]
pub struct ShowTracker {
    last: Vec<Option<u32>>,
    shown: usize,
    skipped: usize,
    latencies: Vec<Duration>,
}

impl ShowTracker {
    #[must_use]
    pub fn new(tiles: usize) -> Self {
        Self {
            last: vec![None; tiles],
            shown: 0,
            skipped: 0,
            latencies: Vec::new(),
        }
    }

    /// Tile `tile` is drawn with frame `counter`, `latency` after the source handed it over.
    pub fn show(&mut self, tile: usize, counter: u32, latency: Duration) {
        let Some(last) = self.last.get_mut(tile) else {
            return;
        };
        if *last == Some(counter) {
            return;
        }
        if let Some(previous) = *last {
            let gap = counter.wrapping_sub(previous).saturating_sub(1);
            self.skipped += usize::try_from(gap).unwrap_or(usize::MAX);
        }
        *last = Some(counter);
        self.shown += 1;
        self.latencies.push(latency);
    }

    /// Starts counting afresh (after the warm-up), keeping the last frame of each tile.
    pub fn reset_counts(&mut self) {
        self.shown = 0;
        self.skipped = 0;
        self.latencies.clear();
    }

    #[must_use]
    pub fn shown(&self) -> usize {
        self.shown
    }

    #[must_use]
    pub fn skipped(&self) -> usize {
        self.skipped
    }

    #[must_use]
    pub fn last(&self) -> &[Option<u32>] {
        &self.last
    }

    #[must_use]
    pub fn latencies(&self) -> &[Duration] {
        &self.latencies
    }
}

/// Parses a `Duration`'s `Debug` text: `15ns`, `812.25µs`, `4.5ms`, `1.25s`.
#[must_use]
pub fn parse_debug_duration(text: &str) -> Option<Duration> {
    let (number, nanos_per_unit) = [
        ("ns", 1),
        ("µs", 1_000),
        ("us", 1_000),
        ("ms", 1_000_000),
        ("s", 1_000_000_000),
    ]
    .iter()
    .find_map(|(unit, scale)| text.strip_suffix(unit).map(|number| (number, *scale)))?;
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    let digits_only = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
    if whole.is_empty() || !digits_only(whole) || !digits_only(fraction) {
        return None;
    }
    let whole: u128 = whole.parse().ok()?;
    let fraction_value: u128 = if fraction.is_empty() { 0 } else { fraction.parse().ok()? };
    let fraction_scale = 10_u128.checked_pow(u32::try_from(fraction.len()).ok()?)?;
    let nanos = whole * nanos_per_unit + fraction_value * nanos_per_unit / fraction_scale;
    Some(Duration::from_nanos(u64::try_from(nanos).ok()?))
}

/// The duration in a GPUI `frame duration: <Debug duration>` line (printed with `ZED_MEASUREMENTS=1`).
#[must_use]
pub fn parse_frame_duration(line: &str) -> Option<Duration> {
    parse_debug_duration(line.strip_prefix("frame duration: ")?.trim())
}

/// A `key=value` field of a line couldn't be used.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum FieldError {
    #[error("no field {0}")]
    Missing(String),
    #[error("field {key}={value} is not a number")]
    Invalid { key: String, value: String },
}

/// The `key=value` fields of a `summary` line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fields(HashMap<String, String>);

impl Fields {
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&String> {
        self.0.get(key)
    }

    /// Field `key` as a number.
    ///
    /// # Errors
    /// The field is missing or isn't a number.
    pub fn number<T: FromStr>(&self, key: &str) -> Result<T, FieldError> {
        let value = self.0.get(key).ok_or_else(|| FieldError::Missing(key.to_owned()))?;
        value.parse().map_err(|_| FieldError::Invalid {
            key: key.to_owned(),
            value: value.clone(),
        })
    }
}

/// The fields of a `summary key=value ...` line; `None` for any other line or a word without `=`.
#[must_use]
pub fn parse_fields(line: &str) -> Option<Fields> {
    let rest = line.strip_prefix("summary ")?;
    rest.split_whitespace()
        .map(|field| {
            field
                .split_once('=')
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
        })
        .collect::<Option<HashMap<_, _>>>()
        .map(Fields)
}

/// The run time in `/proc/<pid>/task/<tid>/schedstat` (its first field, in nanoseconds).
#[must_use]
pub fn parse_schedstat_runtime(text: &str) -> Option<Duration> {
    text.split_whitespace().next()?.parse().ok().map(Duration::from_nanos)
}

/// A size in DRM fdinfo: bytes, or a number with ` KiB` / ` MiB` / ` GiB`.
fn fdinfo_bytes(value: &str) -> Option<u64> {
    let mut parts = value.split_whitespace();
    let number: u64 = parts.next()?.parse().ok()?;
    let scale = match parts.next() {
        None => 1,
        Some("KiB") => 1 << 10,
        Some("MiB") => 1 << 20,
        Some("GiB") => 1 << 30,
        Some(_) => return None,
    };
    number.checked_mul(scale)
}

/// From one DRM fdinfo file: the driver, the client id (several fds can share one client) and the memory resident
/// for that client, summed over every region (`drm-resident-<region>`). `None` for a non-DRM fd.
#[must_use]
pub fn fdinfo_gpu_memory(text: &str) -> Option<(String, u64, u64)> {
    let mut driver = None;
    let mut client = None;
    let mut resident = 0_u64;
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key {
            "drm-driver" => driver = Some(value.to_owned()),
            "drm-client-id" => client = value.parse().ok(),
            _ if key.starts_with("drm-resident-") => {
                resident = resident.saturating_add(fdinfo_bytes(value).unwrap_or(0));
            }
            _ => {}
        }
    }
    Some((driver?, client?, resident))
}

/// The GPU memory `nvidia-smi -q -d PIDS` reports for `pid` (`Used GPU Memory : <n> MiB` under its `Process ID`),
/// in bytes. Graphics clients are listed only there, not by `--query-compute-apps`.
#[must_use]
pub fn nvidia_smi_memory(output: &str, pid: u32) -> Option<u64> {
    let mut current = None;
    for line in output.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        match key.trim() {
            "Process ID" => current = value.trim().parse::<u32>().ok(),
            "Used GPU Memory" if current == Some(pid) => {
                let mib = value.trim().strip_suffix(" MiB")?;
                return mib.trim().parse::<u64>().ok().map(|mib| mib << 20);
            }
            _ => {}
        }
    }
    None
}
