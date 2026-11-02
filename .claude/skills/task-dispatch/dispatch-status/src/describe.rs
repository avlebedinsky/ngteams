//! A tool call in plain words ("Editing crates/x/src/y.rs", a Bash call's `description`), redacted and clipped.

use std::path::Path;
use std::sync::OnceLock;

use serde_json::Value;

use crate::redact::{clip, shown};

/// The longest "doing now" line.
const WORDS_MAX: usize = 140;

/// The longest raw command shown, for a Bash call without a description.
const COMMAND_MAX: usize = 100;

/// A tool call (its `name` and `input`) in plain words.
pub fn tool_words(name: &str, input: &Value) -> String {
    let text = |key: &str| input.get(key).and_then(Value::as_str).unwrap_or_default();
    let words = match name {
        "Bash" => bash_words(input),
        "Read" => format!("Reading {}", relative(text("file_path"))),
        "Edit" | "MultiEdit" => format!("Editing {}", relative(text("file_path"))),
        "NotebookEdit" => format!("Editing {}", relative(text("notebook_path"))),
        "Write" => format!("Writing {}", relative(text("file_path"))),
        "Grep" => grep_words(text("pattern"), text("path")),
        "Glob" => format!("Finding files matching {}", clip(text("pattern"), 60)),
        "Skill" => skill_words(text("skill")),
        "Agent" | "Task" => format!("Running a helper agent: {}", or(text("description"), "a subtask")),
        "SendMessage" => format!("Messaging {}", or(text("to"), "another agent")),
        "TaskStop" => "Stopping a background command".to_owned(),
        "TaskOutput" => "Reading a background command's output".to_owned(),
        "Monitor" => format!("Watching: {}", or(text("description"), "a condition")),
        "ToolSearch" => "Loading tool definitions".to_owned(),
        "WebFetch" => format!("Fetching a page from {}", host(text("url"))),
        "WebSearch" => format!("Searching the web for {}", text("query")),
        "SubagentHandback" => "Handing back its report".to_owned(),
        "TodoWrite" => "Updating its to-do list".to_owned(),
        _ => match name.strip_prefix("mcp__") {
            Some(rest) => format!("Using {}", rest.rsplit("__").next().unwrap_or(rest)),
            None => format!("Using {name}"),
        },
    };
    shown(&words, WORDS_MAX)
}

/// A Bash call: its `description`, or else the first line of its command (a leading `cd … &&` dropped).
pub fn bash_words(input: &Value) -> String {
    let description = input.get("description").and_then(Value::as_str).unwrap_or_default();
    if !description.trim().is_empty() {
        return shown(description, WORDS_MAX);
    }
    let command = input.get("command").and_then(Value::as_str).unwrap_or_default();
    let first = command
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    format!("Running: {}", shown(drop_cd(first), COMMAND_MAX))
}

fn drop_cd(mut line: &str) -> &str {
    while let Some(rest) = line.strip_prefix("cd ") {
        let Some(at) = rest.find("&&").or_else(|| rest.find(';')) else {
            break;
        };
        line = rest[at..].trim_start_matches(['&', ';']).trim_start();
    }
    line
}

fn grep_words(pattern: &str, path: &str) -> String {
    let within = if path.is_empty() {
        String::new()
    } else {
        format!(" in {}", relative(path))
    };
    format!("Searching the code for “{}”{within}", clip(pattern, 50))
}

fn skill_words(skill: &str) -> String {
    if skill.contains("revmux") {
        "Running revmux".to_owned()
    } else {
        format!("Using the {} skill", or(skill, "a"))
    }
}

fn or<'a>(text: &'a str, fallback: &'a str) -> &'a str {
    if text.trim().is_empty() { fallback } else { text }
}

fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    or(host, "the web")
}

/// The repository's root (this crate's manifest dir, four up: `.claude/skills/task-dispatch/dispatch-status`), with a
/// trailing `/`.
fn repo_root() -> &'static str {
    static ROOT: OnceLock<String> = OnceLock::new();
    ROOT.get_or_init(|| {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let root = manifest.ancestors().nth(4).unwrap_or(manifest);
        format!("{}/", root.display())
    })
}

/// A path as short as it can be read: inside a worktree or the repository relative to it, the session's scratchpad
/// as `scratchpad/…`, the home dir as `~`.
pub fn relative(path: &str) -> String {
    if path.is_empty() {
        return "a file".to_owned();
    }
    if let Some(at) = path.find("/.claude/worktrees/") {
        let inside = &path[at + "/.claude/worktrees/".len()..];
        if let Some((_, rest)) = inside.split_once('/') {
            return rest.to_owned();
        }
    }
    if let Some(rest) = path.strip_prefix(repo_root()) {
        return rest.to_owned();
    }
    if let Some(at) = path.find("/scratchpad/")
        && path.starts_with("/tmp/")
    {
        return format!("scratchpad/{}", &path[at + "/scratchpad/".len()..]);
    }
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && path.starts_with(&format!("{home}/")) => {
            format!("~/{}", &path[home.len() + 1..])
        }
        _ => path.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;

    use super::{relative, repo_root, tool_words};

    #[test]
    fn the_repo_root_is_the_root_of_the_repo_holding_this_skill() {
        assert!(Path::new(repo_root()).join("rust-toolchain.toml").is_file());
    }

    #[test]
    fn says_what_a_tool_call_does() {
        let bash = json!({"command": "cargo test", "description": "Run the tests"});
        assert_eq!(tool_words("Bash", &bash), "Run the tests");
        let bare = json!({"command": "cd /x && cargo test -p a --token abc\nmore"});
        assert_eq!(tool_words("Bash", &bare), "Running: cargo test -p a --token [redacted]");
        let secret = json!({"command": "curl -H 'Authorization: Bearer abcdefghijkl' x"});
        assert_eq!(
            tool_words("Bash", &secret),
            "Running: curl -H 'Authorization: [redacted]' x"
        );
        let edit = json!({"file_path": "/r/.claude/worktrees/agent-1/crates/a/src/b.rs"});
        assert_eq!(tool_words("Edit", &edit), "Editing crates/a/src/b.rs");
        assert_eq!(
            tool_words("Skill", &json!({"skill": "revmux:revmux"})),
            "Running revmux"
        );
        assert_eq!(
            tool_words("Grep", &json!({"pattern": "fn main"})),
            "Searching the code for “fn main”"
        );
        assert_eq!(
            tool_words("WebFetch", &json!({"url": "https://a.b/c"})),
            "Fetching a page from a.b"
        );
        assert_eq!(tool_words("mcp__srv__do_it", &json!({})), "Using do_it");
    }

    #[test]
    fn shortens_paths() {
        assert_eq!(relative("/tmp/claude-1000/p/s/scratchpad/x.json"), "scratchpad/x.json");
        assert_eq!(relative(""), "a file");
        assert_eq!(relative("/etc/hosts"), "/etc/hosts");
    }
}
