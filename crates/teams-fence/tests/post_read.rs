//! The fence's decision on a native read by `POST`: a read reaches nobody, so a permit for the exact, ordered list of
//! MRIs, whoever it names; every decision in `fence.log` (without the MRIs), and no permit when the decision can't be
//! logged.
//!
//! MRIs here are placeholders, never real ones.

use std::io::{self, Write};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};
use teams_fence::{Fence, FenceLog, Mri, PostReadDenyReason, Roster};

const LAB_A: &str = "8:orgid:lab-a";
const LAB_B: &str = "8:orgid:lab-b";
const LAB_C: &str = "8:orgid:lab-c";
const ECHO: &str = "28:echo-service";
const COWORKER: &str = "8:orgid:coworker";
const KIND: &str = "short_profile";

fn mris(texts: &[&str]) -> Vec<Mri> {
    texts.iter().copied().map(Mri::new).collect()
}

/// A `fence.log` the test reads back.
#[derive(Clone, Default)]
struct SharedLog(Arc<Mutex<Vec<u8>>>);

#[cfg(test)]
impl SharedLog {
    fn lines(&self) -> Vec<Value> {
        let bytes = self.0.lock().unwrap_or_else(PoisonError::into_inner).clone();
        String::from_utf8(bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()).unwrap()
    }
}

impl Write for SharedLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A `fence.log` that can't be written.
struct BrokenLog;

impl Write for BrokenLog {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("disk full"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("disk full"))
    }
}

fn fence(log: impl Write + Send + 'static) -> Fence {
    Fence::new(
        Roster::new(mris(&[LAB_A, LAB_B, LAB_C]), mris(&[ECHO])),
        FenceLog::from_writer(log),
    )
}

#[test]
fn a_list_of_lab_accounts_gets_a_permit_for_exactly_that_list() {
    let log = SharedLog::default();
    let fence = fence(log.clone());

    let permit = fence.check_post_read(KIND, &mris(&[LAB_B, LAB_A])).unwrap();

    assert_eq!(permit.kind(), KIND);
    assert_eq!(permit.names(), mris(&[LAB_B, LAB_A]), "in the order asked");
    let lines = log.lines();
    assert_eq!(
        lines,
        [json!({
            "ts_ms": lines[0]["ts_ms"],
            "verdict": "allow",
            "event": "post_read_decision",
            "post_read": KIND,
            "names": 2,
        })]
    );
    assert!(!log.text().contains("lab-a"), "the log never holds the MRIs");
}

#[test]
fn a_read_naming_anyone_is_let_through() {
    let cases: [(&str, &[&str]); 5] = [
        ("nobody", &[]),
        ("an outsider", &[COWORKER]),
        ("an outsider among lab accounts", &[LAB_A, COWORKER, LAB_B]),
        ("an approved service participant", &[ECHO]),
        ("a lab account twice", &[LAB_A, LAB_B, LAB_A]),
    ];
    for (name, names) in cases {
        let log = SharedLog::default();
        let fence = fence(log.clone());

        let permit = fence.check_post_read(KIND, &mris(names)).unwrap();

        assert_eq!(permit.names(), mris(names), "{name}");
        let lines = log.lines();
        assert_eq!(lines.len(), 1, "{name}: every decision is logged");
        assert_eq!(lines[0]["verdict"], "allow", "{name}");
        assert_eq!(lines[0]["names"], names.len(), "{name}");
        assert!(!log.text().contains("coworker"), "{name}: the log never holds the MRIs");
    }
}

#[test]
fn an_allow_that_cant_be_logged_is_denied() {
    let fence = fence(BrokenLog);

    let denied = fence.check_post_read(KIND, &mris(&[LAB_A])).unwrap_err();

    assert_eq!(denied.reason(), PostReadDenyReason::LogUnavailable);
}

#[test]
fn a_denial_shows_no_mri() {
    let fence = fence(BrokenLog);

    let denied = fence.check_post_read(KIND, &mris(&[COWORKER, LAB_A])).unwrap_err();

    let shown = format!("{denied} {denied:?}");
    assert!(!shown.contains("coworker") && !shown.contains("lab-a"), "{shown}");
}
