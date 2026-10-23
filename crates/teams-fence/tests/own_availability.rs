//! The account's own availability: a write the fence permits without a member read (it reaches nobody), but only as
//! the account's own. It is `WriteKind::OwnAvailability` on `Scope::Account`, allowed only for the fence's own account
//! (a fence opened for a lab account, `Fence::open_lab_account`), a lab account of the roster, with nobody added;
//! denied for every other account, scope or fence, and nothing else lands on `Scope::Account`. No decision here asks
//! the membership source.
//!
//! MRIs here are placeholders, never real ones.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use teams_fence::lab::{Alias, LabConfig};
use teams_fence::{
    DenyReason, FailClosed, Fence, FenceLog, Membership, MembershipFuture, MembershipSource, Mri, Roster, Scope,
    Target, WriteKind,
};

const LAB_A: &str = "8:orgid:lab-a";
const LAB_B: &str = "8:orgid:lab-b";
const LAB_C: &str = "8:orgid:lab-c";
const ECHO: &str = "28:echo-service";
const COWORKER: &str = "8:orgid:coworker";

/// A source that lists everyone in the lab and counts how often it was asked: no own-availability decision may ask.
#[derive(Default)]
struct Counted(AtomicUsize);

impl Counted {
    fn asked(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

impl MembershipSource for Counted {
    fn members<'a>(&'a self, _scope: &'a Scope) -> MembershipFuture<'a> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::ready(Membership::Complete(vec![
            Mri::new(LAB_A),
            Mri::new(LAB_B),
            Mri::new(LAB_C),
        ])))
    }
}

fn roster() -> Roster {
    Roster::new([LAB_A, LAB_B, LAB_C].map(Mri::new), [Mri::new(ECHO)])
}

fn account(mri: &str) -> Scope {
    Scope::Account { mri: mri.to_owned() }
}

fn own(mri: &str) -> Target {
    Target::new(WriteKind::OwnAvailability, account(mri))
}

/// A fence of the roster logging to `dir`'s `fence.log`, its own account `own` (none for `None`).
fn fence(dir: &Path, own: Option<&str>) -> Fence {
    let fence = Fence::new(roster(), FenceLog::open(&dir.join("fence.log")).unwrap());
    match own {
        Some(mri) => fence.with_own_account(Mri::new(mri)),
        None => fence,
    }
}

fn entries(dir: &Path) -> Vec<serde_json::Value> {
    fs::read_to_string(dir.join("fence.log"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[tokio::test]
async fn the_fences_own_account_may_set_its_availability_without_a_member_read() {
    let dir = tempfile::tempdir().unwrap();
    let fence = fence(dir.path(), Some(LAB_A));
    assert_eq!(fence.own_account(), Some(&Mri::new(LAB_A)));
    let source = Counted::default();
    let permit = fence.check(own(LAB_A), &source).await.unwrap();
    assert_eq!(permit.target(), &own(LAB_A));
    // the fail-closed source fetches nothing, and the own availability needs nothing fetched
    assert!(fence.check(own(LAB_A), &FailClosed).await.is_ok());
    assert_eq!(source.asked(), 0, "the own availability is never a member read");

    let logged = entries(dir.path());
    assert_eq!(logged.len(), 2);
    for entry in &logged {
        assert_eq!(entry["verdict"], "allow", "{entry}");
        assert_eq!(entry["kind"], "own_availability", "{entry}");
        assert_eq!(
            entry["scope"],
            serde_json::json!({"account": {"mri": LAB_A}}),
            "{entry}"
        );
        assert_eq!(entry["added"], 0, "{entry}");
    }
}

#[tokio::test]
async fn every_other_account_scope_or_fence_is_denied_as_not_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let fence_a = fence(dir.path(), Some(LAB_A));
    let source = Counted::default();
    let chat = Scope::Conversation {
        id: "19:lab-chat@thread.v2".to_owned(),
    };
    let denied = [
        ("another lab account", own(LAB_B)),
        ("an approved service", own(ECHO)),
        ("someone outside the lab", own(COWORKER)),
        ("a lookalike of its own", own("8:orgid:LAB-A")),
        ("with someone added", own(LAB_A).with_added([Mri::new(LAB_B)])),
        ("with itself added", own(LAB_A).with_added([Mri::new(LAB_A)])),
        ("on a chat", Target::new(WriteKind::OwnAvailability, chat)),
        ("on a new target", Target::new(WriteKind::OwnAvailability, Scope::New)),
        (
            "on a call",
            Target::new(WriteKind::OwnAvailability, Scope::Call { id: "call".to_owned() }),
        ),
    ];
    for (case, target) in denied {
        let refused = fence_a.check(target.clone(), &source).await.unwrap_err();
        assert_eq!(refused.reason(), &DenyReason::NotOwnAccount, "{case}");
        assert_eq!(refused.target(), &target, "{case}");
    }
    assert_eq!(
        source.asked(),
        0,
        "no own-availability decision asks the membership source"
    );
    assert!(
        entries(dir.path())
            .iter()
            .all(|entry| entry["verdict"] == "deny" && entry["reason"] == "not_own_account")
    );

    // a fence of no account (the browser fence's, `Fence::open_lab`) permits nobody's
    let other = tempfile::tempdir().unwrap();
    let no_account = fence(other.path(), None);
    assert_eq!(no_account.own_account(), None);
    for mri in [LAB_A, LAB_B, LAB_C] {
        let refused = no_account.check(own(mri), &source).await.unwrap_err();
        assert_eq!(refused.reason(), &DenyReason::NotOwnAccount, "{mri}");
    }

    // a fence whose own account isn't a lab account of its roster permits nothing either
    let third = tempfile::tempdir().unwrap();
    let outsider = fence(third.path(), Some(COWORKER));
    let refused = outsider.check(own(COWORKER), &source).await.unwrap_err();
    assert_eq!(refused.reason(), &DenyReason::NotOwnAccount);
    assert_eq!(source.asked(), 0);
}

#[tokio::test]
async fn nothing_but_the_own_availability_lands_on_the_account() {
    let dir = tempfile::tempdir().unwrap();
    let fence = fence(dir.path(), Some(LAB_A));
    let source = Counted::default();
    for kind in WriteKind::ALL {
        if kind == WriteKind::OwnAvailability {
            continue;
        }
        let refused = fence
            .check(Target::new(kind, account(LAB_A)), &source)
            .await
            .unwrap_err();
        assert_eq!(refused.reason(), &DenyReason::AccountScope, "{kind:?}");
    }
    assert_eq!(source.asked(), 0, "the account itself has no members to fetch");
    let logged = entries(dir.path());
    assert_eq!(logged.len(), WriteKind::ALL.len() - 1);
    assert!(logged.iter().all(|entry| entry["reason"] == "account_scope"));
}

#[tokio::test]
async fn the_reasons_read_as_the_rule_says() {
    assert_eq!(
        DenyReason::NotOwnAccount.to_string(),
        "not the fence's own account's availability"
    );
    assert_eq!(
        DenyReason::AccountScope.to_string(),
        "only the account's own availability lands on the account itself"
    );
    assert_eq!(account(LAB_A).to_string(), format!("account {LAB_A}"));
    assert_eq!(WriteKind::OwnAvailability.as_str(), "own_availability");
    assert!(!WriteKind::OwnAvailability.creates_target());
}

/// A `lab.toml` with MRIs for accounts a and b but not c.
fn lab_toml(data_dir: &Path) -> String {
    let account = |alias: &str, mri: Option<&str>| {
        let mri = mri.map(|mri| format!("mri = \"{mri}\"\n")).unwrap_or_default();
        format!("[[accounts]]\nalias = \"{alias}\"\nupn = \"bot-{alias}@lab.example.com\"\n{mri}\n")
    };
    format!(
        "data_dir = \"{}\"\n\n{}{}{}",
        data_dir.display(),
        account("a", Some(LAB_A)),
        account("b", Some(LAB_B)),
        account("c", None),
    )
}

#[tokio::test]
async fn a_lab_accounts_fence_owns_that_account_only() {
    let dir = tempfile::tempdir().unwrap();
    let config = LabConfig::parse(&lab_toml(dir.path()), dir.path()).unwrap();
    let source = Counted::default();

    let fence_b = Fence::open_account(&config, Alias::B).unwrap();
    assert_eq!(fence_b.own_account(), Some(&Mri::new(LAB_B)));
    assert!(fence_b.check(own(LAB_B), &source).await.is_ok());
    let refused = fence_b.check(own(LAB_A), &source).await.unwrap_err();
    assert_eq!(refused.reason(), &DenyReason::NotOwnAccount);

    // c's MRI isn't in lab.toml yet: its fence owns no account, so it permits no availability at all
    let fence_c = Fence::open_account(&config, Alias::C).unwrap();
    assert_eq!(fence_c.own_account(), None);
    let refused = fence_c.check(own(LAB_C), &source).await.unwrap_err();
    assert_eq!(refused.reason(), &DenyReason::NotOwnAccount);

    // the fence of no account (the browser fence's) owns none
    assert_eq!(Fence::open(&config).unwrap().own_account(), None);
    assert_eq!(source.asked(), 0);
}
