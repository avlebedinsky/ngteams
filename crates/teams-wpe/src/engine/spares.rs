//! The web processes `WebKit` keeps with no page of the engine's, ended with the engine: a spare it prewarmed for the
//! next page (it turns prewarming on by itself once a page swapped processes on a cross-site navigation) and one it
//! parked in its process cache once that page closed (`WebKitWebContext` enables the cache). No API names them, so
//! they are found among this process's descendants (`WebKit`'s UI process is this one, the app's or the worker's) by
//! the name of `WebKit`'s web process. Left to notice the UI process go, they would take the exit that does not end
//! cleanly for an open page's web process ([`super::page::Page::end_web_process`]). The network process is not one
//! of them: it ends on its own, flushing the stores first.

use std::collections::HashMap;

/// The name of `WebKit`'s web process, its executable's (`/usr/lib/wpe-webkit-2.0/WPEWebProcess`).
const WEB_PROCESS: &str = "WPEWebProcess";

/// Ends every web process under this process at once, as `WebKit` ends one (`SIGKILL`).
pub(super) fn end() {
    for pid in web_processes_under(std::process::id()) {
        let Ok(pid) = i32::try_from(pid) else { continue };
        // SAFETY: `kill` only sends a signal, to a web process this process started (its descendant).
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

/// The processes named [`WEB_PROCESS`] among the descendants of `root`.
fn web_processes_under(root: u32) -> Vec<u32> {
    let processes = processes();
    processes
        .iter()
        .filter(|(_, (_, name))| name == WEB_PROCESS)
        .filter(|(pid, _)| descends_from(**pid, root, &processes))
        .map(|(pid, _)| *pid)
        .collect()
}

/// Whether `pid`'s chain of parents in `processes` reaches `root`.
fn descends_from(pid: u32, root: u32, processes: &HashMap<u32, (u32, String)>) -> bool {
    let mut next = processes.get(&pid).map(|(parent, _)| *parent);
    // a chain longer than the table would be a loop: a pid reused while the table was read
    for _ in 0..processes.len() {
        match next {
            Some(parent) if parent == root => return true,
            Some(parent) if parent > 1 => next = processes.get(&parent).map(|(parent, _)| *parent),
            _ => return false,
        }
    }
    false
}

/// Every process's parent and name, from `/proc`.
fn processes() -> HashMap<u32, (u32, String)> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return HashMap::new();
    };
    entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter_map(|pid| {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            Some((pid, parent_and_name(&stat)?))
        })
        .collect()
}

/// The parent and the name in a `/proc/<pid>/stat` line: the name sits in the outermost parentheses (and may hold
/// some itself), the parent is the second field after them.
fn parent_and_name(stat: &str) -> Option<(u32, String)> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let name = stat.get(open + 1..close)?.to_owned();
    let parent = stat.get(close + 2..)?.split_whitespace().nth(1)?.parse().ok()?;
    Some((parent, name))
}

#[cfg(test)]
mod tests {
    use std::process::{Child, Command};
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn a_stat_line_gives_its_parent_and_its_name_parentheses_and_all() {
        assert_eq!(
            parent_and_name("42 (WPEWebProcess) S 41 42 7 0 -1"),
            Some((41, "WPEWebProcess".to_owned()))
        );
        assert_eq!(
            parent_and_name("43 (a (b) c) R 1 43 43 0"),
            Some((1, "a (b) c".to_owned()))
        );
        assert_eq!(parent_and_name("garbage"), None);
    }

    #[test]
    fn descendants_are_found_through_their_parents_and_a_loop_ends() {
        let table: HashMap<u32, (u32, String)> = [
            (10, (1, "app".to_owned())),
            (11, (10, "bwrap".to_owned())),
            (12, (11, "bwrap".to_owned())),
            (13, (12, WEB_PROCESS.to_owned())),
            (20, (1, WEB_PROCESS.to_owned())),
            (30, (31, "x".to_owned())),
            (31, (30, "y".to_owned())),
        ]
        .into_iter()
        .collect();
        assert!(descends_from(13, 10, &table));
        assert!(descends_from(12, 10, &table));
        assert!(!descends_from(20, 10, &table));
        assert!(!descends_from(30, 10, &table));
        assert!(!descends_from(99, 10, &table));
    }

    /// `sleep` run under the web process's name.
    fn stand_in(dir: &std::path::Path, name: &str) -> Child {
        let program = dir.join(name);
        std::fs::copy("/bin/sleep", &program).unwrap();
        Command::new(&program).arg("30").spawn().unwrap()
    }

    fn ended(child: &mut Child) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if child.try_wait().unwrap().is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn the_web_processes_under_this_process_end_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let mut web = stand_in(dir.path(), WEB_PROCESS);
        let mut other = stand_in(dir.path(), "WPENetworkProcess");
        // the name shows once the child has run its program
        let deadline = Instant::now() + Duration::from_secs(10);
        while !web_processes_under(std::process::id()).contains(&web.id()) {
            assert!(Instant::now() < deadline, "the stand-in never showed under its name");
            std::thread::sleep(Duration::from_millis(10));
        }
        end();
        assert!(ended(&mut web), "the web process stand-in still runs");
        assert!(other.try_wait().unwrap().is_none(), "another child was ended too");
        other.kill().unwrap();
        other.wait().unwrap();
    }
}
