//! What a tool call says about the phase its agent is in.

use std::path::Path;

use serde_json::Value;

/// A step a tool call shows the agent took.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    /// A file edited outside `/tmp`.
    Edit,
    /// A file edited under `/tmp` (the scratchpad): counts only for an agent without a worktree.
    ScratchEdit,
    /// `cargo build`, `check`, `run` or an unchecked `fmt`.
    Build,
    /// `cargo fmt --check`, `clippy`, `test` or `lint-report.sh`: the gate after `land.sh`, a build before it.
    Check,
    /// A probe of what was built (`curl`): verifies for an agent without a worktree.
    Probe,
    /// The `revmux:revmux` skill, a `revmux` command, or a helper agent preparing a round.
    Revmux,
    /// `git commit` (or the commit skill).
    Commit,
    /// `land.sh`.
    Land,
}

/// The signals of one tool call.
pub fn tool_signals(name: &str, input: &Value) -> Vec<Signal> {
    let text = |key: &str| input.get(key).and_then(Value::as_str).unwrap_or_default();
    match name {
        "Bash" => bash_signals(text("command")),
        "Edit" | "MultiEdit" | "Write" => vec![edit_signal(text("file_path"))],
        "NotebookEdit" => vec![edit_signal(text("notebook_path"))],
        "Skill" if text("skill").contains("revmux") => vec![Signal::Revmux],
        "Skill" if text("skill").ends_with("commit") => vec![Signal::Commit],
        "Agent" | "Task" if text("description").to_ascii_lowercase().contains("revmux") => vec![Signal::Revmux],
        _ => Vec::new(),
    }
}

fn edit_signal(path: &str) -> Signal {
    if path.starts_with("/tmp/") {
        Signal::ScratchEdit
    } else {
        Signal::Edit
    }
}

/// A shell command's signals, segment by segment (`&&`, `||`, `;`, `|`, newlines and parentheses split it).
pub fn bash_signals(command: &str) -> Vec<Signal> {
    let mut signals = Vec::new();
    for segment in command.split(['\n', ';', '|', '&', '(', ')']) {
        let words: Vec<&str> = segment.split_whitespace().collect();
        let words = without_prefix(&words);
        if let Some(signal) = segment_signal(words) {
            signals.push(signal);
        }
    }
    if edits_files(command) {
        signals.push(if command.contains("/tmp/") {
            Signal::ScratchEdit
        } else {
            Signal::Edit
        });
    }
    signals
}

/// The words after `VAR=value` assignments and wrappers (`env`, `exec`, `time`, `command`, `nice -n N`, `setsid`,
/// `timeout [flags] DURATION`, `flock [flags] LOCKFILE`).
fn without_prefix<'a, 'b>(mut words: &'b [&'a str]) -> &'b [&'a str] {
    loop {
        let Some((&head, rest)) = words.split_first() else {
            return words;
        };
        words = match head {
            "env" | "exec" | "time" | "command" | "setsid" | "nohup" => rest,
            "nice" => skip_flags(rest, &["-n"]),
            "timeout" => skip_one(skip_flags(rest, &["-s", "-k", "--signal", "--kill-after"])),
            "flock" => skip_one(skip_flags(rest, &["-w", "-E", "--timeout", "--conflict-exit-code"])),
            _ if is_assignment(head) => rest,
            _ => return words,
        };
    }
}

/// The words after the flags at their start, a flag of `with_value` taking the next word too.
fn skip_flags<'a, 'b>(mut words: &'b [&'a str], with_value: &[&str]) -> &'b [&'a str] {
    while let Some((&head, rest)) = words.split_first() {
        if !head.starts_with('-') {
            break;
        }
        words = if with_value.contains(&head) {
            skip_one(rest)
        } else {
            rest
        };
    }
    words
}

fn skip_one<'a, 'b>(words: &'b [&'a str]) -> &'b [&'a str] {
    words.split_first().map_or(words, |(_, rest)| rest)
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    })
}

fn segment_signal(words: &[&str]) -> Option<Signal> {
    let (&head, rest) = words.split_first()?;
    let program = Path::new(head).file_name()?.to_str()?;
    match program {
        "revmux" => Some(Signal::Revmux),
        "cargo" => cargo_signal(rest),
        "git" => git_signal(rest),
        "curl" => Some(Signal::Probe),
        _ if program.ends_with("lint-report.sh") => Some(Signal::Check),
        _ if program.ends_with("land.sh") => Some(Signal::Land),
        _ => None,
    }
}

fn cargo_signal(args: &[&str]) -> Option<Signal> {
    let subcommand = args.iter().find(|arg| !arg.starts_with(['+', '-']))?;
    match *subcommand {
        "clippy" | "test" | "nextest" => Some(Signal::Check),
        "fmt" if args.contains(&"--check") => Some(Signal::Check),
        "fmt" | "check" | "build" | "run" => Some(Signal::Build),
        _ => None,
    }
}

/// `git commit`, its `-C dir` and `-c key=value` options before it skipped.
fn git_signal(args: &[&str]) -> Option<Signal> {
    let args = skip_flags(args, &["-C", "-c", "--git-dir", "--work-tree"]);
    (args.first() == Some(&"commit")).then_some(Signal::Commit)
}

/// A shell command that writes files itself: `sed -i`, a redirect into a file by `cat` or `tee`, `git apply`, `patch`.
fn edits_files(command: &str) -> bool {
    [
        "sed -i",
        "cat >",
        "cat <<",
        "tee ",
        "git apply",
        "patch -",
        ".write(",
        "write_text(",
    ]
    .iter()
    .any(|pattern| command.contains(pattern))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Signal, bash_signals, tool_signals};

    #[test]
    fn reads_the_steps_out_of_commands() {
        assert_eq!(
            bash_signals("cd /w && CARGO_BUILD_JOBS=4 cargo check -p a"),
            [Signal::Build]
        );
        assert_eq!(
            bash_signals("timeout --signal=INT 600 cargo test -p a"),
            [Signal::Check]
        );
        assert_eq!(bash_signals("cargo +nightly fmt --all --check"), [Signal::Check]);
        assert_eq!(bash_signals("git -C /w commit -m x"), [Signal::Commit]);
        assert_eq!(
            bash_signals("/r/.claude/skills/task-dispatch/scripts/land.sh"),
            [Signal::Land]
        );
        assert_eq!(bash_signals("scripts/lint-report.sh"), [Signal::Check]);
        assert_eq!(bash_signals("revmux run --task x"), [Signal::Revmux]);
        assert_eq!(bash_signals("git log --oneline | head"), []);
        assert_eq!(bash_signals("grep revmux SKILL.md"), []);
        assert_eq!(bash_signals("sed -i s/a/b/ src/x.rs"), [Signal::Edit]);
    }

    #[test]
    fn reads_the_steps_out_of_tool_calls() {
        let edit = json!({"file_path": "/home/n/r/src/x.rs"});
        assert_eq!(tool_signals("Edit", &edit), [Signal::Edit]);
        let scratch = json!({"file_path": "/tmp/claude-1000/s/scratchpad/x.py"});
        assert_eq!(tool_signals("Write", &scratch), [Signal::ScratchEdit]);
        assert_eq!(
            tool_signals("Skill", &json!({"skill": "revmux:revmux"})),
            [Signal::Revmux]
        );
        assert_eq!(tool_signals("SubagentHandback", &json!({})), []);
        assert_eq!(tool_signals("Read", &json!({"file_path": "/x"})), []);
    }
}
