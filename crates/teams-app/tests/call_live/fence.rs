//! `fence.log` over the run: what the apps' fences and b's page's fence decided.
//!
//! - Every line is whole JSON.
//! - No breach, nothing contained, no arming failure.
//! - No deny of a write (a line with a `kind`): every write the apps and the page asked for was one the fence maps
//!   and permitted. A deny of the browser's own (a line with a `channel`: the reviewed expected-deny list's, the
//!   run's forced ones) only while b's page ran, which judged them itself (its run exited 0).
//! - Every write permitted: a call write only on a call the fence registered or offered (its `call_operation` scope:
//!   the registry resolved it), a test call's start (`new`, the caller and the echo service), a call or message only
//!   in the a–b chat or the recorded group chat, an availability only of a lab account's own.
//! - The rest is counted: requests let through because they reach nobody (`allowed`, by shape), the typed delivery
//!   responses, the frames logged.

use std::collections::BTreeMap;
use std::fs;

use serde_json::{Value, json};

use super::Lab;

/// When b's page ran (Unix ms, from its start to its exit).
pub type Window = (i64, i64);

/// What the run's lines hold, counted.
#[derive(Debug, Default)]
struct Counts {
    lines: usize,
    /// Writes permitted, by kind and scope (`call_follow_up update_endpoint_state`, `message ab`, …).
    writes: BTreeMap<String, usize>,
    /// Browser denies during b's page's runs, by reason.
    page_denies: BTreeMap<String, usize>,
    allowed: BTreeMap<String, usize>,
    delivery_responses: BTreeMap<String, usize>,
    other_events: BTreeMap<String, usize>,
    problems: Vec<String>,
}

/// Judges `fence.log` from byte `start` on (see the module docs); what it holds, counted, and every problem found.
pub fn judge(lab: &Lab, start: u64, pages: &[Window]) -> (Value, Vec<String>) {
    let log = fs::read(&lab.fence_log).unwrap();
    let run = &log[usize::try_from(start).unwrap().min(log.len())..];
    let mut counts = Counts::default();
    if !(run.is_empty() || run.ends_with(b"\n")) {
        counts.problems.push("fence.log ends in a half line".to_owned());
    }
    for line in String::from_utf8_lossy(run).lines() {
        counts.lines += 1;
        match serde_json::from_str::<Value>(line) {
            Ok(entry) => counts.add(lab, &entry, pages),
            Err(error) => counts.problems.push(format!("not a whole line ({error}): {line}")),
        }
    }
    let summary = json!({
        "lines": counts.lines,
        "writes": counts.writes,
        "page_denies": counts.page_denies,
        "allowed": counts.allowed,
        "delivery_responses": counts.delivery_responses,
        "other_events": counts.other_events,
    });
    (summary, counts.problems)
}

impl Counts {
    fn add(&mut self, lab: &Lab, entry: &Value, pages: &[Window]) {
        let verdict = entry["verdict"].as_str().unwrap_or_default();
        let at = entry["ts_ms"].as_i64().unwrap_or_default();
        match verdict {
            "allow" => self.allow(lab, entry),
            "deny" if entry.get("kind").is_some() => self.problems.push(format!("a write denied: {entry}")),
            "deny" if entry.get("channel").is_some() && in_page(pages, at) => {
                let reason = entry["reason"].as_str().unwrap_or("?").to_owned();
                *self.page_denies.entry(reason).or_default() += 1;
            }
            _ => self.problems.push(format!("a {verdict} line: {entry}")),
        }
    }

    fn allow(&mut self, lab: &Lab, entry: &Value) {
        if let Some(kind) = entry["kind"].as_str() {
            match Self::write_scope(lab, kind, &entry["scope"]) {
                Ok(scope) => *self.writes.entry(format!("{kind} {scope}")).or_default() += 1,
                Err(problem) => self.problems.push(format!("{problem}: {entry}")),
            }
            return;
        }
        let text = |key: &str| entry[key].as_str().unwrap_or_default().to_owned();
        match entry["event"].as_str() {
            Some("allowed") => {
                let shape = format!("{} {}{}", text("method"), text("host"), text("path"));
                *self.allowed.entry(shape).or_default() += 1;
            }
            Some("delivery_response") => *self.delivery_responses.entry(text("response")).or_default() += 1,
            Some(event) => *self.other_events.entry(event.to_owned()).or_default() += 1,
            None => self.problems.push(format!("an allow of nothing known: {entry}")),
        }
    }

    /// The scope a permitted write of `kind` names, as the counts say it; a problem when it isn't one the run's
    /// writes may have.
    fn write_scope(lab: &Lab, kind: &str, scope: &Value) -> Result<String, String> {
        if scope == "new" {
            return if kind == "call" {
                Ok("new".to_owned())
            } else {
                Err(format!("a {kind} on a new target"))
            };
        }
        if let Some(operation) = scope["call_operation"]["operation"].as_str() {
            return Ok(operation.to_owned());
        }
        if let Some(mri) = scope["account"]["mri"].as_str() {
            return if kind == "own_availability" && lab.mris.iter().any(|lab_mri| lab_mri == mri) {
                Ok("own".to_owned())
            } else {
                Err(format!("a {kind} on an account"))
            };
        }
        let conversation = scope["conversation"]["id"].as_str().unwrap_or_default();
        if conversation == lab.ab_chat {
            Ok("ab".to_owned())
        } else if conversation == lab.group_chat {
            Ok("group".to_owned())
        } else {
            Err(format!("a {kind} off the a–b and the group chat"))
        }
    }
}

/// Whether `at` (Unix ms) falls in one of b's page's runs.
fn in_page(pages: &[Window], at: i64) -> bool {
    pages.iter().any(|(from, to)| (*from..=*to).contains(&at))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lab(dir: &std::path::Path) -> Lab {
        Lab {
            app: dir.join("app"),
            proto_lab: dir.join("proto-lab"),
            ab_chat: "19:a_b@unq.gbl.spaces".to_owned(),
            group_chat: "19:group@thread.v2".to_owned(),
            mris: ["8:orgid:a", "8:orgid:b", "8:orgid:c"].map(str::to_owned),
            fence_log: dir.join("fence.log"),
            out: dir.to_owned(),
            shots: dir.to_owned(),
            tools: None,
        }
    }

    #[test]
    fn the_run_s_writes_pass_and_anything_else_is_a_problem() {
        let dir = tempfile::tempdir().unwrap();
        let lab = lab(dir.path());
        let lines = [
            r#"{"ts_ms":5,"verdict":"allow","kind":"call","scope":"new","added":2}"#,
            r#"{"ts_ms":6,"verdict":"allow","kind":"call","scope":{"conversation":{"id":"19:a_b@unq.gbl.spaces"}},"added":2}"#,
            r#"{"ts_ms":7,"verdict":"allow","kind":"call_answer","scope":{"call_operation":{"operation":"accept","url":"https://x"}},"added":0}"#,
            r#"{"ts_ms":8,"verdict":"allow","kind":"own_availability","scope":{"account":{"mri":"8:orgid:b"}},"added":0}"#,
            r#"{"ts_ms":9,"verdict":"allow","event":"delivery_response","response":"call_acceptance_acknowledgement"}"#,
            r#"{"ts_ms":10,"verdict":"allow","event":"allowed","method":"POST","host":"x","path":"/y"}"#,
            r#"{"ts_ms":11,"verdict":"deny","channel":"http","method":"PUT","url":"https://x","reason":"unclassified"}"#,
        ];
        fs::write(&lab.fence_log, format!("{}\n", lines.join("\n"))).unwrap();
        let (summary, problems) = judge(&lab, 0, &[(10, 20)]);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(summary["writes"]["call new"], 1);
        assert_eq!(summary["writes"]["call ab"], 1);
        assert_eq!(summary["writes"]["call_answer accept"], 1);
        assert_eq!(summary["page_denies"]["unclassified"], 1);
        // the same browser deny outside the page's run, a denied write, a write elsewhere, a breach
        let bad = [
            r#"{"ts_ms":30,"verdict":"deny","channel":"http","method":"PUT","url":"https://x","reason":"unclassified"}"#,
            r#"{"ts_ms":31,"verdict":"deny","kind":"call","scope":"new","added":2}"#,
            r#"{"ts_ms":32,"verdict":"allow","kind":"message","scope":{"conversation":{"id":"19:other@thread.v2"}},"added":0}"#,
            r#"{"ts_ms":33,"verdict":"allow","kind":"own_availability","scope":{"account":{"mri":"8:orgid:x"}},"added":0}"#,
            r#"{"ts_ms":34,"verdict":"breach","event":"arming_failure"}"#,
            "{\"half",
        ];
        fs::write(&lab.fence_log, bad.join("\n")).unwrap();
        let (_, problems) = judge(&lab, 0, &[(10, 20)]);
        assert_eq!(problems.len(), 7, "{problems:#?}");
    }
}
