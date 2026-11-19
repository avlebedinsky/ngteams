//! "Use AI features" holds for every AI caller, also one added later: the sources keep each AI call behind the one
//! switch (`teams_ui::local_ai::enabled`).
//!
//! - Only `local_ai` builds a `Target`, looks for the tools or lists models. Every other caller gets its target from
//!   `local_ai::pick`, which refuses all work while the switch is off.
//! - Each entry point of `local_ai` that can start a tool or ask an API asks the switch first.
//!
//! The view tests (`shell.rs`, `message_correct.rs`) show the switch at work: the controls hidden and nothing run.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::fs;
use std::path::{Path, PathBuf};

/// What starts AI work without the switch: only `local_ai` may write these.
const GATED: [&str; 7] = [
    "pick_from(",
    "Target::Program(",
    "Target::Api(",
    "ApiTarget {",
    "Tools::detect(",
    "list_models(",
    "list_unless(",
];

/// The callers outside `local_ai` that may write one of [`GATED`], each once, in a function that asks the switch
/// first.
const ASKS_ITSELF: [(&str, &str, &str); 1] = [
    // "Test connection" in the AI providers' settings
    (
        "teams-ui/src/settings_view/ai_providers.rs",
        "list_unless(",
        "pub fn test_connection(",
    ),
];

/// The entry points of `local_ai` that can start a tool or ask an API: each asks the switch first.
const ENTRY_POINTS: [(&str, &str); 7] = [
    ("local_ai/providers.rs", "pub fn pick("),
    ("local_ai/providers.rs", "pub fn of(provider: &Provider"),
    ("local_ai/providers.rs", "pub fn relist(provider: &Provider"),
    ("local_ai.rs", "pub fn of(cx: &mut App) -> Self"),
    ("local_ai.rs", "pub fn look_again("),
    ("local_ai/catalog.rs", "pub fn of(cx: &mut App) -> Self"),
    ("local_ai/catalog.rs", "pub fn relist("),
];

fn crates() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn sources(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
}

fn in_local_ai(path: &Path) -> bool {
    let shown = path.to_string_lossy().replace('\\', "/");
    shown.ends_with("teams-ui/src/local_ai.rs") || shown.contains("teams-ui/src/local_ai/")
}

#[test]
fn only_local_ai_starts_ai_work_and_every_other_caller_goes_through_the_switch() {
    let mut files = Vec::new();
    for app in ["teams-ui", "ngteams"] {
        sources(&crates().join(app).join("src"), &mut files);
    }
    assert!(files.len() > 50, "the sources were found");
    let mut bypasses = Vec::new();
    for file in files.iter().filter(|file| !in_local_ai(file)) {
        let text = fs::read_to_string(file).unwrap();
        let shown = file.to_string_lossy().replace('\\', "/");
        for gated in GATED.iter().filter(|gated| text.contains(**gated)) {
            let allowed = ASKS_ITSELF
                .iter()
                .find(|(path, call, _)| shown.ends_with(path) && call == gated);
            if let Some((_, _, function)) = allowed {
                asks_first_then_calls_once(&text, function, gated, &shown);
            } else {
                bypasses.push(format!("{shown}: {gated}"));
            }
        }
    }
    assert!(
        bypasses.is_empty(),
        "AI work started outside local_ai, past \"Use AI features\": get the target from local_ai::pick \
         instead: {bypasses:#?}"
    );
}

/// `text` writes `gated` once, in `function`, whose body asks the switch first.
fn asks_first_then_calls_once(text: &str, function: &str, gated: &str, shown: &str) {
    assert_eq!(
        text.matches(gated).count(),
        1,
        "{shown} writes {gated} more than once: ask the switch at each call"
    );
    let start = text
        .find(function)
        .unwrap_or_else(|| panic!("{shown} has no {function}"));
    let body = &text[start..];
    let end = body.find("\n    }\n").unwrap_or(body.len());
    let first: String = body.lines().take(4).collect::<Vec<_>>().join("\n");
    assert!(
        first.contains("local_ai::enabled(cx)"),
        "{shown}: {function} does not ask the switch first:\n{first}"
    );
    assert!(body[..end].contains(gated), "{shown} writes {gated} outside {function}");
}

#[test]
fn each_entry_point_of_local_ai_asks_the_switch_first() {
    let src = crates().join("teams-ui/src");
    for (file, signature) in ENTRY_POINTS {
        let text = fs::read_to_string(src.join(file)).unwrap();
        let start = text
            .find(signature)
            .unwrap_or_else(|| panic!("{file} has no {signature}"));
        let body: String = text[start..].lines().take(4).collect::<Vec<_>>().join("\n");
        assert!(
            body.contains("enabled(cx)"),
            "{file}: {signature} does not ask the switch first:\n{body}"
        );
    }
}
