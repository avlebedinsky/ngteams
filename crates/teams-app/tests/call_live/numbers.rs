//! The run's numbers: CPU per process from `/proc` while a call runs (with the load average beside it), the frames
//! per second a video tile presented, from its trace lines, and their percentiles (`stats` is the shared harness's).

use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::apps::{at_ms, is, strings};
use super::common::percentile;
pub use super::common::stats;

/// `value` as a float (a count or a span far under 2³²).
pub fn float(value: u64) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}

/// [`stats`] of fractional values, at one decimal.
pub fn stats_f(mut values: Vec<f64>) -> Value {
    values.sort_by(f64::total_cmp);
    if values.is_empty() {
        return Value::Null;
    }
    let round = |value: f64| (value * 10.0).round() / 10.0;
    json!({
        "n": values.len(),
        "min": round(values[0]),
        "p50": round(percentile(&values, 50)),
        "p95": round(percentile(&values, 95)),
        "max": round(values[values.len() - 1]),
    })
}

/// The 1-minute load average now.
pub fn load_average() -> f64 {
    fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|text| text.split_whitespace().next()?.parse().ok())
        .unwrap_or(f64::NAN)
}

/// A process's CPU time so far (user and system), in clock ticks; `None` once it is gone.
fn cpu_ticks(pid: u32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // the fields after the command's closing parenthesis: state is the 3rd field, utime the 14th, stime the 15th
    let rest = &stat[stat.rfind(')')? + 2..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    Some(fields.get(11)?.parse::<u64>().ok()? + fields.get(12)?.parse::<u64>().ok()?)
}

/// Clock ticks per second.
fn ticks_per_second() -> f64 {
    // SAFETY: sysconf reads a constant of the system; it touches no memory of ours.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    u32::try_from(ticks)
        .ok()
        .filter(|ticks| *ticks > 0)
        .map_or(100.0, f64::from)
}

/// Per process, its CPU share each second; and the load average at each sample.
type Samples = (BTreeMap<String, Vec<f64>>, Vec<f64>);

/// CPU use of named processes, sampled once a second on a thread of its own until stopped: per process, the share
/// of one CPU each second (100 = one CPU busy); and the load average at each sample.
pub struct CpuSampler {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Samples>>,
}

impl CpuSampler {
    /// Starts sampling `processes` (name, pid).
    pub fn start(processes: Vec<(String, u32)>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let thread = thread::spawn(move || sample(&processes, &stopping));
        Self {
            stop,
            thread: Some(thread),
        }
    }

    /// Stops and gives, per process, [`stats_f`] of its CPU share, and the load average's.
    pub fn finish(mut self) -> Value {
        self.stop.store(true, Ordering::Relaxed);
        let (shares, loads) = self
            .thread
            .take()
            .and_then(|thread| thread.join().ok())
            .unwrap_or_default();
        let per_process: BTreeMap<String, Value> = shares
            .into_iter()
            .map(|(name, values)| (name, stats_f(values)))
            .collect();
        json!({"cpu_percent": per_process, "load_average": stats_f(loads)})
    }
}

/// Samples `processes` once a second until `stopping` is set.
fn sample(processes: &[(String, u32)], stopping: &AtomicBool) -> Samples {
    let hz = ticks_per_second();
    let mut last: Vec<Option<u64>> = processes.iter().map(|(_, pid)| cpu_ticks(*pid)).collect();
    let mut at = Instant::now();
    let mut shares: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut loads = Vec::new();
    while !stopping.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_secs(1));
        let elapsed = at.elapsed().as_secs_f64();
        at = Instant::now();
        loads.push(load_average());
        for ((name, pid), before) in processes.iter().zip(last.iter_mut()) {
            let now = cpu_ticks(*pid);
            let used = now.zip(*before).map(|(now, then)| float(now.saturating_sub(then)));
            if let Some(used) = used {
                shares
                    .entry(name.clone())
                    .or_default()
                    .push(used / hz / elapsed * 100.0);
            }
            *before = now;
        }
    }
    (shares, loads)
}

impl Drop for CpuSampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// The frames per second `tile` of call `call` presented, between each two of its `video-tile presented` lines
/// (every 30th frame) from `lines`.
pub fn tile_fps(lines: &[Value], call: &Value, tile: &str) -> Vec<f64> {
    let presented: Vec<(i64, u64)> = lines
        .iter()
        .filter(|line| is(line, "video-tile", "presented") && line["call"] == *call && strings(line, "kinds") == [tile])
        .filter_map(|line| Some((at_ms(line), line["frames"].as_u64()?)))
        .collect();
    presented
        .windows(2)
        .filter_map(|pair| {
            let ((then, before), (now, after)) = (pair[0], pair[1]);
            let span = u64::try_from(now - then).ok().filter(|span| *span > 0)?;
            (after > before).then(|| float(after - before) * 1_000.0 / float(span))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fractional_stats_are_nearest_rank_at_one_decimal() {
        assert_eq!(stats_f(Vec::new()), Value::Null);
        assert_eq!(
            stats_f(vec![1.04, 2.0]),
            json!({"n": 2, "min": 1.0, "p50": 1.0, "p95": 2.0, "max": 2.0})
        );
    }

    #[test]
    fn a_tile_s_rate_is_read_between_its_presented_lines() {
        let line = |at: i64, frames: u64, tile: &str| json!({"view": "video-tile", "change": "presented", "kinds": [tile], "call": 7, "frames": frames, "at_ms": at});
        let lines = vec![
            line(1_000, 1, "remote"),
            line(2_000, 30, "remote"),
            line(3_000, 60, "remote"),
            line(3_000, 30, "self"),
            json!({"view": "video-tile", "change": "presented", "kinds": ["remote"], "call": 8, "frames": 90, "at_ms": 4_000}),
        ];
        assert_eq!(tile_fps(&lines, &json!(7), "remote"), vec![29.0, 30.0]);
        assert!(tile_fps(&lines, &json!(7), "self").is_empty());
    }

    #[test]
    fn this_process_s_cpu_is_read() {
        assert!(cpu_ticks(std::process::id()).is_some());
        assert!(cpu_ticks(u32::MAX).is_none());
        assert!(load_average() >= 0.0);
    }
}
